//! Track catalog: classification, catalog commits, and missing detection.

use super::*;

/// Artist every scan-committed track credits until identify reconciles it.
const UNKNOWN_ARTIST_ID: &str = "scan-unknown-artist";

/// Title for tracks sitting directly under a root (no parent directory).
const ROOT_ALBUM_TITLE: &str = "Unknown Album";

impl CatalogStore for SqliteScanStore {
    fn commit_indexed(
        &self,
        root_id: &str,
        relative_path: &str,
        size_bytes: u64,
        mtime_ns: i64,
        track_id: String,
        tags_read_at: f64,
    ) {
        self.commit_indexed_batch(&[CommitIndexedItem {
            root_id: root_id.to_owned(),
            relative_path: relative_path.to_owned(),
            size_bytes,
            mtime_ns,
            track_id,
            tags_read_at,
        }]);
    }

    fn commit_indexed_batch(&self, items: &[CommitIndexedItem]) {
        if items.is_empty() {
            return;
        }
        let mut guard = self.lock();
        match retry_on_busy("commit_indexed_batch", || {
            commit_indexed_batch_inner(&mut guard.conn, items)
        }) {
            Ok(()) => {
                // Cleared only on success: a failed batch keeps the
                // re-offer shortcut instead of losing it.
                for item in items {
                    guard
                        .deferred
                        .remove(&(item.root_id.clone(), item.relative_path.clone()));
                }
                guard.catalog_dirty = true;
                guard.catalog_version += 1;
            }
            Err(error) => tracing::error!(%error, "scan commit_indexed_batch failed"),
        }
    }

    fn mark_deferred(&self, root_id: &str, relative_path: &str, deferred: bool) {
        let mut guard = self.lock();
        let key = (root_id.to_owned(), relative_path.to_owned());
        if deferred {
            guard.deferred.insert(key);
        } else {
            guard.deferred.remove(&key);
        }
    }

    fn catalog_entries(&self, root_id: &str) -> Vec<(String, CatalogEntry)> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT relative_path, id, stat_revision, stat_revision_kind, file_size_bytes, \
             file_mtime_ns, tags_read_at FROM local_tracks \
             WHERE root_id = ?1 AND availability = 'indexed' ORDER BY relative_path",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan catalog_entries failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![root_id], |row| {
            let kind: String = row.get("stat_revision_kind")?;
            Ok((
                row.get::<_, String>("relative_path")?,
                CatalogEntry {
                    track_id: row.get("id")?,
                    revision: row.get("stat_revision")?,
                    size_bytes: row.get::<_, i64>("file_size_bytes")? as u64,
                    mtime_ns: row.get("file_mtime_ns")?,
                    revision_kind: if kind == "exact" {
                        RevisionKind::Exact
                    } else {
                        RevisionKind::LegacyFloat
                    },
                    tags_read_at: row.get("tags_read_at")?,
                },
            ))
        })
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan catalog_entries failed");
            Vec::new()
        })
    }

    fn album_for_track(&self, track_id: &str) -> Option<String> {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT local_album_id FROM local_tracks \
                 WHERE id = ?1 AND availability = 'indexed'",
                params![track_id],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan album_for_track failed");
                None
            })
    }

    fn track_at(&self, root_id: &str, relative_path: &str) -> Option<String> {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT id FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2 AND availability = 'indexed'",
                params![root_id, relative_path],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan track_at failed");
                None
            })
    }

    fn remove_catalog(&self, root_id: &str, relative_path: &str) {
        let mut guard = self.lock();
        let outcome = retry_on_busy("remove_catalog", || {
            let tx = guard.conn.transaction()?;
            let found: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT id, local_album_id FROM local_tracks \
                     WHERE root_id = ?1 AND relative_path = ?2",
                    params![root_id, relative_path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((track_id, album_id)) = found {
                tx.execute(
                    "UPDATE library_scan_inventory SET local_track_id = NULL \
                     WHERE local_track_id = ?1",
                    params![track_id],
                )?;
                tx.execute(
                    "DELETE FROM local_track_artists WHERE local_track_id = ?1",
                    params![track_id],
                )?;
                tx.execute("DELETE FROM local_tracks WHERE id = ?1", params![track_id])?;
                tx.commit()?;
                if let Some(album_id) = album_id
                    && let Err(error) = cleanup_emptied_album(&mut guard.conn, &album_id)
                {
                    tracing::debug!(%error, "scan kept an emptied album row");
                }
            }
            Ok(())
        });
        match outcome {
            Ok(()) => {
                // Cleared only on success, matching commit_indexed_batch:
                // a failed delete keeps the re-offer shortcut.
                guard
                    .deferred
                    .remove(&(root_id.to_owned(), relative_path.to_owned()));
                guard.catalog_dirty = true;
                guard.catalog_version += 1;
                if let Some(cached) = guard.catalog_cache.get_mut(root_id) {
                    cached.entries.remove(relative_path);
                }
            }
            Err(error) => tracing::error!(%error, "scan remove_catalog failed"),
        }
    }

    fn missing_catalog_paths(
        &self,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Vec<String> {
        // The exact scope_covers_path rule in SQL: "." covers everything,
        // else the path equals the scope or extends it past a slash. LIKE
        // metacharacters in the scope escape so a literal % never widens.
        let guard = self.lock();
        let sql = "SELECT t.relative_path FROM local_tracks t \
             WHERE t.root_id = ?1 AND t.availability = 'indexed' \
             AND (?2 = '.' OR t.relative_path = ?2 OR t.relative_path LIKE ?3 ESCAPE '\\') \
             AND NOT EXISTS ( \
             SELECT 1 FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?4 AND i.root_id = ?1 AND i.relative_path = t.relative_path \
             AND s.discovery_generation = i.discovery_generation) \
             ORDER BY t.relative_path";
        let mut stmt = match guard.conn.prepare(sql) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan missing_catalog_paths failed");
                return Vec::new();
            }
        };
        let prefix = format!("{}%", escape_like_prefix(scope_relative_path));
        stmt.query_map(
            params![root_id, scope_relative_path, prefix, run_id],
            |row| row.get::<_, String>(0),
        )
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan missing_catalog_paths failed");
            Vec::new()
        })
    }

    fn classify(
        &self,
        root_id: &str,
        paths: &[ClassifyInput],
        run_id: Option<&str>,
    ) -> HashMap<String, (Verdict, Option<String>)> {
        if paths.is_empty() {
            return HashMap::new();
        }
        let mut guard = self.lock();
        // One bulk load per catalog version: discovery classifies hundreds
        // of batches against an unchanging catalog, and every commit or
        // removal bumps the version, so the snapshot is never stale.
        let version = guard.catalog_version;
        let fresh = guard
            .catalog_cache
            .get(root_id)
            .is_some_and(|cached| cached.version == version);
        if !fresh {
            match load_catalog_map(&guard.conn, root_id) {
                Ok(entries) => {
                    guard
                        .catalog_cache
                        .insert(root_id.to_owned(), CachedCatalog { version, entries });
                }
                Err(error) => {
                    tracing::error!(%error, "scan classify failed");
                    return HashMap::new();
                }
            }
        }
        let mut verdicts = HashMap::with_capacity(paths.len());
        let mut promotions: Vec<(String, ClassifyInput)> = Vec::new();
        let mut skew: Vec<String> = Vec::new();
        {
            let Some(cached) = guard.catalog_cache.get(root_id) else {
                tracing::error!("scan classify lost its snapshot");
                return HashMap::new();
            };
            for input in paths {
                let key = normalize_key(&input.0);
                let Some(entry) = cached.entries.get(&key) else {
                    verdicts.insert(key, (Verdict::New, None));
                    continue;
                };
                let same_size = entry.size_bytes == input.1;
                let unchanged = match entry.revision_kind {
                    RevisionKind::Exact => entry.revision == input.4,
                    RevisionKind::LegacyFloat => {
                        let current_mtime = input.3;
                        let saved_mtime = entry.mtime_ns as f64 / 1_000_000_000.0;
                        let band = legacy_mtime_eps_seconds(current_mtime);
                        if let Some(tags_read_at) = entry.tags_read_at
                            && (current_mtime - tags_read_at).abs() > band
                        {
                            skew.push(key.clone());
                        }
                        same_size && (current_mtime - saved_mtime).abs() <= band
                    }
                };
                if unchanged && guard.deferred.contains(&(root_id.to_owned(), key.clone())) {
                    verdicts.insert(key, (Verdict::Changed, Some(entry.track_id.clone())));
                    continue;
                }
                if unchanged && entry.revision_kind != RevisionKind::Exact {
                    promotions.push((key.clone(), input.clone()));
                }
                verdicts.insert(
                    key,
                    (
                        if unchanged {
                            Verdict::Unchanged
                        } else {
                            Verdict::Changed
                        },
                        Some(entry.track_id.clone()),
                    ),
                );
            }
        }
        // Promotions mutate the row the verdict just read: write through to
        // the snapshot so later batches in this scan see exact revisions.
        for (key, input) in &promotions {
            let outcome = retry_on_busy("classify promotion", || {
                guard.conn.execute(
                    "UPDATE local_tracks SET stat_revision = ?1, file_size_bytes = ?2, \
                     file_mtime_ns = ?3, stat_revision_kind = 'exact' \
                     WHERE root_id = ?4 AND relative_path = ?5",
                    params![input.4, input.1 as i64, input.2, root_id, key],
                )
            });
            if let Err(error) = outcome {
                tracing::error!(%error, "scan classify promotion failed");
                continue;
            }
            if let Some(entry) = guard
                .catalog_cache
                .get_mut(root_id)
                .and_then(|cached| cached.entries.get_mut(key))
            {
                entry.revision.clone_from(&input.4);
                entry.size_bytes = input.1;
                entry.mtime_ns = input.2;
                entry.revision_kind = RevisionKind::Exact;
            }
        }
        if let Some(run_id) = run_id {
            for relative_path in &skew {
                let outcome = retry_on_busy("classify skew evidence", || {
                    guard.conn.execute(
                        "INSERT OR IGNORE INTO library_scan_failures \
                         (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                         recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', 0.0)",
                        params![
                            run_id,
                            root_id,
                            relative_path,
                            failure_codes::MTIME_SKEW,
                            "Legacy mtime drift beyond tolerance; the file clock and the read clock disagree.",
                        ],
                    )
                });
                if let Err(error) = outcome {
                    tracing::error!(%error, "scan classify skew evidence failed");
                }
            }
        }
        verdicts
    }
}

/// Catalog-commit path: upsert the track row plus its album, artist, and
/// join rows so the reads layer shows the file as indexed. Metadata comes
/// from the path alone (the coordinator passes no tags); identify owns
/// real grouping and reconciliation later. One transaction with reused
/// prepared statements for the whole batch: same rows as one transaction
/// per file, one commit instead of hundreds.
fn commit_indexed_batch_inner(
    conn: &mut Connection,
    items: &[CommitIndexedItem],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let mut artist = tx.prepare(
        "INSERT INTO local_artists (id, display_name, folded_name, normalized_name, kind, \
         created_at, updated_at) VALUES (?1, 'Unknown Artist', ?2, '', 'unknown', ?3, ?3) \
         ON CONFLICT (id) DO NOTHING",
    )?;
    let mut album = tx.prepare(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'automatic', ?7, ?7) \
         ON CONFLICT (id) DO NOTHING",
    )?;
    let mut track = tx.prepare(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, \
         tags_read_at, title, title_folded, album_title, album_title_folded, disc_number, \
         track_number, file_format, availability, ingest_source, imported_at, \
         membership_source, title_provenance, album_title_provenance, album_artist_provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'exact', ?10, ?11, ?12, ?13, ?14, 1, 0, \
         ?15, 'indexed', 'scan', ?10, 'automatic', 'parsed', ?16, 'absent') \
         ON CONFLICT (root_id, relative_path) DO UPDATE SET \
         local_album_id = excluded.local_album_id, file_path = excluded.file_path, \
         path_hash = excluded.path_hash, file_size_bytes = excluded.file_size_bytes, \
         file_mtime_ns = excluded.file_mtime_ns, stat_revision = excluded.stat_revision, \
         stat_revision_kind = excluded.stat_revision_kind, \
         tags_read_at = excluded.tags_read_at, title = excluded.title, \
         title_folded = excluded.title_folded, album_title = excluded.album_title, \
         album_title_folded = excluded.album_title_folded, \
         file_format = excluded.file_format, availability = 'indexed', missing_since = NULL, \
         excluded_at = NULL",
    )?;
    let mut track_artist = tx.prepare(
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
         VALUES (?1, 0, ?2, 'main') ON CONFLICT (local_track_id, position) DO NOTHING",
    )?;
    let mut album_artist = tx.prepare(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES (?1, 0, ?2, 'main') ON CONFLICT (local_album_id, position) DO NOTHING",
    )?;
    let unknown_folded = fold_text("Unknown Artist");
    for item in items {
        let file_name = item
            .relative_path
            .rsplit('/')
            .next()
            .unwrap_or(item.relative_path.as_str());
        let (stem, extension) = match file_name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
                (stem, extension)
            }
            _ => (file_name, ""),
        };
        let title = if stem.is_empty() {
            "Unknown Track"
        } else {
            stem
        };
        let file_format = if extension.is_empty() {
            "unknown".to_owned()
        } else {
            extension.to_ascii_lowercase()
        };
        let parent = item
            .relative_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or(".");
        let parent = if parent.is_empty() { "." } else { parent };
        let album_title = if parent == "." {
            ROOT_ALBUM_TITLE
        } else {
            parent.rsplit('/').next().unwrap_or(parent)
        };
        let album_provenance = if parent == "." {
            "placeholder"
        } else {
            "parsed"
        };
        let album_id = format!(
            "scan-album-{}",
            &sha256_hex(&format!(
                "scan-album-v1\0{root_id}\0{parent}",
                root_id = item.root_id
            ))[..32]
        );
        let grouping_key = format!("scan:{root_id}:{parent}", root_id = item.root_id);
        let path_hash = sha256_hex(&format!(
            "scan-track-v1\0{root_id}\0{relative_path}",
            root_id = item.root_id,
            relative_path = item.relative_path
        ));
        artist.execute(params![
            UNKNOWN_ARTIST_ID,
            unknown_folded,
            item.tags_read_at
        ])?;
        album.execute(params![
            album_id,
            item.root_id,
            grouping_key,
            album_title,
            fold_text(album_title),
            UNKNOWN_ARTIST_ID,
            item.tags_read_at,
        ])?;
        track.execute(params![
            item.track_id,
            album_id,
            item.root_id,
            item.relative_path,
            item.relative_path,
            path_hash,
            item.size_bytes as i64,
            item.mtime_ns,
            exact_stat_revision(item.size_bytes, item.mtime_ns),
            item.tags_read_at,
            title,
            fold_text(title),
            album_title,
            fold_text(album_title),
            file_format,
            album_provenance,
        ])?;
        // The join uses the passed track id directly: the upsert keeps a
        // conflicting row's id, and that id is always the passed one.
        // Changed files carry their classify-provided id (no catalog
        // writer runs between classify and commit on the single worker),
        // new files insert it (the walk dedupes keys, so no twin can
        // claim the row first), and re-reads follow the same two cases.
        track_artist.execute(params![item.track_id, UNKNOWN_ARTIST_ID])?;
        album_artist.execute(params![album_id, UNKNOWN_ARTIST_ID])?;
    }
    drop(artist);
    drop(album);
    drop(track);
    drop(track_artist);
    drop(album_artist);
    tx.commit()
}

/// Delete an album left with no tracks, with its scan-owned joins.
/// Identity rows owned elsewhere block the delete (all-or-nothing) and the album
/// stays; the caller logs that at debug.
fn cleanup_emptied_album(conn: &mut Connection, album_id: &str) -> rusqlite::Result<()> {
    let remaining: i64 = conn.query_row(
        "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1",
        params![album_id],
        |row| row.get(0),
    )?;
    if remaining > 0 {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM local_album_artists WHERE local_album_id = ?1",
        params![album_id],
    )?;
    tx.execute(
        "DELETE FROM local_album_artwork WHERE local_album_id = ?1",
        params![album_id],
    )?;
    tx.execute("DELETE FROM local_albums WHERE id = ?1", params![album_id])?;
    tx.commit()
}

/// Bulk catalog load backing in-memory classify: same row shape as
/// [`ScanStore::catalog_entries`], keyed for verdict lookups.
fn load_catalog_map(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<HashMap<String, CatalogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT relative_path, id, stat_revision, stat_revision_kind, file_size_bytes, \
         file_mtime_ns, tags_read_at FROM local_tracks \
         WHERE root_id = ?1 AND availability = 'indexed'",
    )?;
    let rows = stmt.query_map(params![root_id], |row| {
        let kind: String = row.get("stat_revision_kind")?;
        Ok((
            row.get::<_, String>("relative_path")?,
            CatalogEntry {
                track_id: row.get("id")?,
                revision: row.get("stat_revision")?,
                size_bytes: row.get::<_, i64>("file_size_bytes")? as u64,
                mtime_ns: row.get("file_mtime_ns")?,
                revision_kind: if kind == "exact" {
                    RevisionKind::Exact
                } else {
                    RevisionKind::LegacyFloat
                },
                tags_read_at: row.get("tags_read_at")?,
            },
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (key, entry) = row?;
        map.insert(key, entry);
    }
    Ok(map)
}
