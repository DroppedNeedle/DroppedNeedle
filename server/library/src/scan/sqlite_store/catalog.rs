//! Track catalog: classification, catalog commits, and missing detection.

use super::commit::walked_past;
use super::*;

/// The `scope_covers_path` rule in SQL for a catalog path column
/// `relative_path` and a scope bound as `?2`: "." covers everything, else
/// the path is the scope or extends it past a slash. Byte comparisons
/// only: paths are case-sensitive, so no `LIKE`, and every path under
/// "scope/" sorts between "scope/" and "scope0" ('0' follows '/').
const IN_SCOPE: &str = "(?2 = '.' OR relative_path = ?2 \
    OR (relative_path > ?2 || '/' AND relative_path < ?2 || '0'))";

/// [`IN_SCOPE`] for a `local_tracks t` alias.
const IN_SCOPE_T: &str = "(?2 = '.' OR t.relative_path = ?2 \
    OR (t.relative_path > ?2 || '/' AND t.relative_path < ?2 || '0'))";

/// Mark one listed path (`?2` under root `?1`) missing at `?3`, only while
/// run `?4` still walked past it.
const MARK_WALKED_PAST_MISSING: &str = concat!(
    "UPDATE local_tracks AS t SET availability = 'missing', missing_since = ?3 \
     WHERE t.root_id = ?1 AND t.relative_path = ?2 AND t.availability = 'indexed' AND ",
    walked_past!("?4")
);

/// The track catalog: classification against it, commits into it, and
/// missing detection.
impl SqliteScanStore {
    /// Commit one index window atomically: catalog rows, inventory
    /// marks, counters, and identify offers land together or not at all.
    /// A row the catalog refuses fails alone and is reported back. A
    /// window that runs out of time is split in two by path and each half
    /// committed as its own window, down to one file; a file that still
    /// runs out of time is reported back as refused, and its inventory row
    /// is marked failed so a resumed run does not try it again.
    pub fn commit_window(
        &self,
        stop: &RunStop,
        window: &IndexWindow,
    ) -> Result<WindowOutcome, ScanStoreError> {
        let mut outcome = WindowOutcome::default();
        self.commit_part(stop, window, &mut outcome)?;
        Ok(outcome)
    }

    fn commit_part(
        &self,
        stop: &RunStop,
        window: &IndexWindow,
        total: &mut WindowOutcome,
    ) -> Result<(), ScanStoreError> {
        match super::commit::commit_window(&self.db, stop, window) {
            Ok(outcome) => {
                self.landed(window, &outcome);
                total.committed += outcome.committed;
                total.enqueued += outcome.enqueued;
                total.failed.extend(outcome.failed);
                Ok(())
            }
            Err(ScanStoreError::TxTimeout { operation }) => {
                if let Some((first, second)) = super::commit::split_window(window) {
                    tracing::warn!(
                        operation,
                        files = window.items.len(),
                        "index window ran out of time; committing it in two halves"
                    );
                    self.commit_part(stop, &first, total)?;
                    return self.commit_part(stop, &second, total);
                }
                let Some(given_up) = super::commit::give_up_window(window) else {
                    return Err(ScanStoreError::TxTimeout { operation });
                };
                tracing::warn!(
                    operation,
                    path = window.through.1,
                    "one file's catalog write ran out of time; recorded as failed"
                );
                let outcome = super::commit::commit_window(&self.db, stop, &given_up)?;
                total.enqueued += outcome.enqueued;
                total.failed.extend(outcome.failed);
                Ok(())
            }
            Err(other) => Err(other),
        }
    }

    /// What a committed window changes in memory: a committed read clears
    /// the deferred re-offer shortcut, and a catalog write moves the
    /// classify snapshot's version.
    fn landed(&self, window: &IndexWindow, outcome: &WindowOutcome) {
        let failed: HashSet<(&str, &str)> = outcome
            .failed
            .iter()
            .map(|(root, path, _)| (root.as_str(), path.as_str()))
            .collect();
        let mut guard = self.lock();
        for item in &window.items {
            if !failed.contains(&(item.root_id.as_str(), item.relative_path.as_str())) {
                guard
                    .deferred
                    .remove(&(item.root_id.clone(), item.relative_path.clone()));
            }
        }
        if outcome.committed > 0 {
            guard.catalog_version += 1;
        }
    }

    /// Of these catalog tracks, the ones whose edition hints were never
    /// stored: files carried over from v2 that no scan has read since.
    pub fn without_edition_tags(
        &self,
        stop: &RunStop,
        track_ids: &[String],
    ) -> Result<HashSet<String>, ScanStoreError> {
        if track_ids.is_empty() {
            return Ok(HashSet::new());
        }
        self.read_long(stop, "scan_edition_tags_missing", |conn| {
            let mut stored = conn.prepare_cached(crate::scan::sql::HAS_EDITION_TAGS.text())?;
            let mut missing = HashSet::new();
            for id in track_ids {
                if !stored.query_row(params![id], |row| row.get::<_, bool>(0))? {
                    missing.insert(id.clone());
                }
            }
            Ok(missing)
        })
    }

    pub fn mark_deferred(&self, root_id: &str, relative_path: &str, deferred: bool) {
        let mut guard = self.lock();
        let key = (root_id.to_owned(), relative_path.to_owned());
        if deferred {
            guard.deferred.insert(key);
        } else {
            guard.deferred.remove(&key);
        }
    }

    /// Catalog album of one indexed track: the track-to-album join.
    pub fn album_for_track(&self, track_id: &str) -> Result<Option<String>, ScanStoreError> {
        self.read("scan_track_album", |conn| {
            Ok(conn
                .query_row(
                    "SELECT local_album_id FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed'",
                    params![track_id],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Indexed track at one path.
    pub fn track_at(
        &self,
        root_id: &str,
        relative_path: &str,
    ) -> Result<Option<String>, ScanStoreError> {
        self.read("scan_track_at", |conn| {
            Ok(conn
                .query_row(
                    "SELECT id FROM local_tracks \
                     WHERE root_id = ?1 AND relative_path = ?2 AND availability = 'indexed'",
                    params![root_id, relative_path],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Mark catalog rows missing (never delete: plays, playlists, and
    /// identities keep pointing at them) and count them on the run, in
    /// chunks: each chunk marks its rows and counts them in one turn.
    /// Each row is checked again inside its chunk, so a library write that
    /// landed after the listing keeps its track. Returns rows marked.
    pub fn mark_missing(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        relative_paths: &[String],
        now: f64,
    ) -> Result<usize, ScanStoreError> {
        if relative_paths.is_empty() {
            return Ok(0);
        }
        let marked = self.write_chunked(stop, "scan_mark_missing", relative_paths, |chunk, tx| {
            let mut marked = 0usize;
            {
                let mut stmt = tx.prepare_cached(MARK_WALKED_PAST_MISSING)?;
                for path in chunk {
                    marked += stmt.execute(params![root_id, path, now, run_id])?;
                }
            }
            tx.execute(
                "UPDATE library_scan_runs SET missing_count = missing_count + ?1 WHERE id = ?2",
                params![marked as i64, run_id],
            )?;
            if marked > 0 {
                crate::catalog_revision::bump_catalog(tx)?;
            }
            Ok(marked)
        });
        let mut guard = self.lock();
        // Chunks that committed before a stop stay marked: drop what the
        // snapshot knows of the paths either way.
        if marked.as_ref().map_or(true, |marked| *marked > 0) {
            guard.catalog_version += 1;
            if let Some(cached) = guard.catalog_cache.get_mut(root_id) {
                for path in relative_paths {
                    cached.entries.remove(path);
                }
            }
        }
        marked
    }

    /// Indexed catalog rows under one scope.
    pub fn indexed_count(
        &self,
        stop: &RunStop,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<usize, ScanStoreError> {
        self.read_long(stop, "scan_indexed_count", |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM local_tracks WHERE root_id = ?1 \
                         AND availability = 'indexed' AND {IN_SCOPE}"
                    ),
                    params![root_id, scope_relative_path],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count.max(0) as usize)?)
        })
    }

    /// For one walked scope before indexing: indexed rows the walk did
    /// not see, and the rows the scope holds once its new files index.
    /// Feeds the mass-missing guard.
    pub fn vanish_counts(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<(usize, usize), ScanStoreError> {
        self.read_long(stop, "scan_vanish_counts", |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT \
                         (SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = ?1 \
                          AND t.availability = 'indexed' AND {IN_SCOPE_T} AND {WALKED}), \
                         (SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = ?1 \
                          AND t.availability = 'indexed' AND {IN_SCOPE_T}) \
                         + (SELECT COUNT(*) FROM library_scan_inventory i WHERE i.run_id = ?3 \
                          AND i.root_id = ?1 AND i.scope_relative_path = ?2 \
                          AND i.comparison_result = 'new')",
                        WALKED = walked_past!("?3"),
                    ),
                    params![root_id, scope_relative_path, run_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map(|(missing, total)| (missing.max(0) as usize, total.max(0) as usize))?)
        })
    }

    /// True when the run held missing detection back for this scope (the
    /// mass-missing guard). Such a scope counts as not walked: nothing in
    /// it is missing, moved, or taken over.
    pub fn scope_guarded(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<bool, ScanStoreError> {
        self.read_long(stop, "scan_scope_guarded", |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM library_scan_failures WHERE run_id = ?1 \
                 AND root_id = ?2 AND relative_path = ?3 AND failure_code = ?4)",
                params![
                    run_id,
                    root_id,
                    scope_relative_path,
                    FailureCode::MassMissingGuard
                ],
                |row| row.get(0),
            )?)
        })
    }

    /// Catalog paths under `scope_relative_path` that the run's current
    /// inventory does not contain: the missing set for one cleanly-walked
    /// scope, each with whether the run held back a folder covering it
    /// (a mount found empty), where nothing is marked. Only indexed catalog
    /// rows qualify, and scope matching is the exact [`scope_covers_path`]
    /// rule, answered in one query.
    pub fn missing_catalog_paths(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<Vec<(String, bool)>, ScanStoreError> {
        let sql = format!(
            "SELECT t.relative_path, {} FROM local_tracks t \
             WHERE t.root_id = ?1 AND t.availability = 'indexed' AND {IN_SCOPE} \
             AND {} ORDER BY t.relative_path",
            super::held_scopes::held_back!("?3", "t"),
            walked_past!("?3"),
        );
        self.read_long(stop, "scan_missing_paths", |conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![root_id, scope_relative_path, run_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Verdicts for one discovery batch against the root's catalog. Rows
    /// whose legacy revision holds are promoted to exact, and legacy clock
    /// skew is recorded as evidence, in one write beside the batch.
    pub fn classify(
        &self,
        stop: &RunStop,
        root_id: &str,
        paths: &[ClassifyInput],
        run_id: Option<&str>,
    ) -> Result<HashMap<String, (Verdict, Option<String>)>, ScanStoreError> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        // One bulk load per catalog version: discovery classifies hundreds
        // of batches against an unchanging catalog, and every commit or
        // removal bumps the version, so the snapshot is never stale. The
        // load runs with the store's mutex released.
        let version = self.lock().catalog_version;
        let shared_revision = self.read_long(stop, "scan_catalog_revision", |conn| {
            Ok(shared_catalog_revision(conn)?)
        })?;
        let fresh = self
            .lock()
            .catalog_cache
            .get(root_id)
            .is_some_and(|cached| {
                cached.version == version && cached.shared_revision == shared_revision
            });
        if !fresh {
            let entries = self.read_long(stop, "scan_classify_snapshot", |conn| {
                Ok(load_catalog_map(conn, root_id)?)
            })?;
            self.lock().catalog_cache.insert(
                root_id.to_owned(),
                CachedCatalog {
                    version,
                    shared_revision,
                    entries,
                },
            );
        }
        let mut verdicts = HashMap::with_capacity(paths.len());
        let mut promotions: Vec<(String, ClassifyInput)> = Vec::new();
        let mut skew: Vec<String> = Vec::new();
        {
            let guard = self.lock();
            let Some(cached) = guard.catalog_cache.get(root_id) else {
                return Err(ScanStoreError::Internal {
                    message: "scan classify lost its snapshot".to_owned(),
                });
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
        let skew = if run_id.is_some() { skew } else { Vec::new() };
        if promotions.is_empty() && skew.is_empty() {
            return Ok(verdicts);
        }
        self.write(stop, Lane::Bulk, "scan_classify_evidence", |tx| {
            let mut promote = tx.prepare_cached(
                "UPDATE local_tracks SET stat_revision = ?1, file_size_bytes = ?2, \
                 file_mtime_ns = ?3, stat_revision_kind = 'exact' \
                 WHERE root_id = ?4 AND relative_path = ?5",
            )?;
            for (key, input) in &promotions {
                promote.execute(params![input.4, input.1 as i64, input.2, root_id, key])?;
            }
            if let Some(run_id) = run_id {
                let mut evidence = tx.prepare_cached(
                    "INSERT OR IGNORE INTO library_scan_failures \
                     (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                     recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', 0.0)",
                )?;
                for relative_path in &skew {
                    evidence.execute(params![
                        run_id,
                        root_id,
                        relative_path,
                        FailureCode::MtimeSkew,
                        "Legacy mtime drift beyond tolerance; the file clock and the read clock disagree.",
                    ])?;
                }
            }
            Ok(())
        })?;
        // Promotions changed the rows the verdicts just read: write through
        // to the snapshot so later batches in this scan see exact revisions.
        let mut guard = self.lock();
        if let Some(cached) = guard.catalog_cache.get_mut(root_id) {
            for (key, input) in &promotions {
                if let Some(entry) = cached.entries.get_mut(key) {
                    entry.revision.clone_from(&input.4);
                    entry.size_bytes = input.1;
                    entry.mtime_ns = input.2;
                    entry.revision_kind = RevisionKind::Exact;
                }
            }
        }
        Ok(verdicts)
    }

    /// Catalog tracks under any of the scopes, each counted once, missing
    /// rows included (v2 `estimate_scan_scope`). An approximate size for
    /// the scan a caller is about to request.
    pub fn estimate_scopes(&self, scopes: &[ScanScope]) -> Result<u64, ScanStoreError> {
        if scopes.is_empty() {
            return Ok(0);
        }
        // One count over the union, so nested or repeated scopes never
        // count a track twice. Each scope binds its root and path.
        let clauses: Vec<String> = (0..scopes.len())
            .map(|index| {
                let root = 2 * index + 1;
                let path = root + 1;
                format!(
                    "(root_id = ?{root} AND (?{path} = '.' OR relative_path = ?{path} \
                     OR (relative_path > ?{path} || '/' AND relative_path < ?{path} || '0')))"
                )
            })
            .collect();
        let sql = format!(
            "SELECT COUNT(*) FROM local_tracks WHERE {}",
            clauses.join(" OR ")
        );
        let values: Vec<&str> = scopes
            .iter()
            .flat_map(|scope| [scope.root_id.as_str(), scope.relative_path.as_str()])
            .collect();
        let count: i64 = self.read("scan_estimate", |conn| {
            Ok(conn.query_row(&sql, params_from_iter(values), |row| row.get(0))?)
        })?;
        Ok(count.max(0) as u64)
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
/// root, plus rows a curator excluded by hand (they stay excluded, and an
/// unchanged one needs no tag read), keyed by relative path.
fn load_catalog_map(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<HashMap<String, CatalogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT relative_path, id, stat_revision, stat_revision_kind, file_size_bytes, \
         file_mtime_ns, tags_read_at FROM local_tracks \
         WHERE root_id = ?1 AND (availability = 'indexed' OR manual_excluded = 1)",
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
