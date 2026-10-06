//! Track catalog: classification, catalog commits, and missing detection.

use super::*;

/// The `scope_covers_path` rule in SQL for a catalog path column
/// `relative_path` and a scope bound as `?2`: "." covers everything, else
/// the path is the scope or extends it past a slash. Byte comparisons
/// only: paths are case-sensitive, so no `LIKE`, and every path under
/// "scope/" sorts between "scope/" and "scope0" ('0' follows '/').
const IN_SCOPE: &str = "(?2 = '.' OR relative_path = ?2 \
    OR (relative_path > ?2 || '/' AND relative_path < ?2 || '0'))";

impl CatalogStore for SqliteScanStore {
    fn commit_window(&self, window: &IndexWindow) -> Result<WindowOutcome, ScanStoreError> {
        let mut guard = self.lock();
        let outcome = super::commit::commit_window(&mut guard.conn, window).map_err(internal)?;
        let failed: HashSet<(&str, &str)> = outcome
            .failed
            .iter()
            .map(|(root, path, _)| (root.as_str(), path.as_str()))
            .collect();
        // A committed read clears the deferred re-offer shortcut.
        for item in &window.items {
            if !failed.contains(&(item.root_id.as_str(), item.relative_path.as_str())) {
                guard
                    .deferred
                    .remove(&(item.root_id.clone(), item.relative_path.clone()));
            }
        }
        if outcome.committed > 0 {
            guard.catalog_dirty = true;
            guard.catalog_version += 1;
        }
        Ok(outcome)
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

    fn mark_missing(
        &self,
        run_id: &str,
        root_id: &str,
        relative_paths: &[String],
        now: f64,
    ) -> Result<usize, ScanStoreError> {
        if relative_paths.is_empty() {
            return Ok(0);
        }
        let mut guard = self.lock();
        let marked = retry_on_busy("mark_missing", || {
            let tx = guard
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let mut marked = 0usize;
            {
                let mut stmt = tx.prepare_cached(
                    "UPDATE local_tracks SET availability = 'missing', missing_since = ?3 \
                     WHERE root_id = ?1 AND relative_path = ?2 AND availability = 'indexed'",
                )?;
                for path in relative_paths {
                    marked += stmt.execute(params![root_id, path, now])?;
                }
            }
            tx.execute(
                "UPDATE library_scan_runs SET missing_count = missing_count + ?1 WHERE id = ?2",
                params![marked as i64, run_id],
            )?;
            tx.commit()?;
            Ok(marked)
        })
        .map_err(internal)?;
        if marked > 0 {
            guard.catalog_dirty = true;
            guard.catalog_version += 1;
            if let Some(cached) = guard.catalog_cache.get_mut(root_id) {
                for path in relative_paths {
                    cached.entries.remove(path);
                }
            }
        }
        Ok(marked)
    }

    fn indexed_count(
        &self,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<usize, ScanStoreError> {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM local_tracks WHERE root_id = ?1 \
                     AND availability = 'indexed' AND {IN_SCOPE}"
                ),
                params![root_id, scope_relative_path],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count.max(0) as usize)
            .map_err(internal)
    }

    fn missing_catalog_paths(
        &self,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Vec<String> {
        let guard = self.lock();
        let sql = format!(
            "SELECT t.relative_path FROM local_tracks t \
             WHERE t.root_id = ?1 AND t.availability = 'indexed' AND {IN_SCOPE} \
             AND NOT EXISTS ( \
             SELECT 1 FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?3 AND i.root_id = ?1 AND i.relative_path = t.relative_path \
             AND s.discovery_generation = i.discovery_generation) \
             ORDER BY t.relative_path"
        );
        let mut stmt = match guard.conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan missing_catalog_paths failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![root_id, scope_relative_path, run_id], |row| {
            row.get::<_, String>(0)
        })
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
        let shared_revision = match shared_catalog_revision(&guard.conn) {
            Ok(revision) => revision,
            Err(error) => {
                tracing::error!(%error, "scan classify failed");
                return HashMap::new();
            }
        };
        let fresh = guard.catalog_cache.get(root_id).is_some_and(|cached| {
            cached.version == version && cached.shared_revision == shared_revision
        });
        if !fresh {
            match load_catalog_map(&guard.conn, root_id) {
                Ok(entries) => {
                    guard.catalog_cache.insert(
                        root_id.to_owned(),
                        CachedCatalog {
                            version,
                            shared_revision,
                            entries,
                        },
                    );
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

/// The catalog revision every catalog writer bumps (0 before the first).
fn shared_catalog_revision(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE((SELECT value FROM library_catalog_revision WHERE singleton = 1), 0)",
        [],
        |row| row.get(0),
    )
}

/// Bulk catalog load backing in-memory classify: every indexed row of one
/// root, keyed by relative path for verdict lookups.
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
