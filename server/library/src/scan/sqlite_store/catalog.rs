//! Track catalog: classification, catalog commits, and missing detection.

use super::*;
use droppedneedle_kernel::db::statement::ListLen;

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
    /// stored (files carried over from v2 that no scan has read since) or
    /// were stored without a track total, and the ones whose row does not
    /// say whether their length was measured.
    pub fn unread(
        &self,
        stop: &RunStop,
        track_ids: &[String],
    ) -> Result<UnreadRows, ScanStoreError> {
        if track_ids.is_empty() {
            return Ok(UnreadRows::default());
        }
        self.read_long(stop, "scan_unread_rows", |conn| {
            let mut stored = conn.prepare_cached(crate::scan::sql::HAS_EDITION_TAGS.text())?;
            let mut length = conn.prepare_cached(crate::scan::sql::LENGTH_UNREAD.text())?;
            let mut unread = UnreadRows::default();
            for id in track_ids {
                if !stored.query_row(params![id], |row| row.get::<_, bool>(0))? {
                    unread.edition_tags.insert(id.clone());
                }
                if length.query_row(params![id], |row| row.get::<_, bool>(0))? {
                    unread.length.insert(id.clone());
                }
            }
            Ok(unread)
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
                    crate::scan::sql::ALBUM_FOR_TRACK.text(),
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
                    crate::scan::sql::TRACK_AT.text(),
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
                let mut stmt =
                    tx.prepare_cached(crate::scan::sql::MARK_WALKED_PAST_MISSING.text())?;
                for path in chunk {
                    marked +=
                        stmt.execute(params![root_id, path, crate::clock::ms(now), run_id])?;
                }
            }
            tx.execute(
                crate::scan::sql::MARK_MISSING.text(),
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

    /// Retire rows of `root_id` whose files another library folder indexes
    /// (their real folders overlap): each `(path, home root, home path)`
    /// whose home row is indexed is marked missing, as the run walked past
    /// it, and continues into the home row, which takes what people made
    /// on it. A row whose home is not indexed yet stays as it is, so
    /// nothing is lost before the other folder's scan finds the file,
    /// unless its own file under `root_dir` is absent: then it is marked
    /// missing like any row whose file went. Returns how many rows retired.
    pub fn retire_yielded(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        root_dir: &std::path::Path,
        moves: &[(String, String, String)],
        now: f64,
    ) -> Result<usize, ScanStoreError> {
        if moves.is_empty() {
            return Ok(0);
        }
        // Read before the write: only a file the filesystem says is not
        // there counts as gone, never one it failed to answer for.
        let absent: HashSet<&str> = moves
            .iter()
            .filter(|(path, _, _)| {
                std::fs::symlink_metadata(root_dir.join(path))
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            })
            .map(|(path, _, _)| path.as_str())
            .collect();
        let retired = self.write_chunked(stop, "scan_retire_yielded", moves, |chunk, tx| {
            let mut pairs: Vec<(String, String)> = Vec::new();
            let mut gone = 0usize;
            {
                let mut find = tx.prepare_cached(crate::scan::sql::TRACK_AT.text())?;
                let mut mark =
                    tx.prepare_cached(crate::scan::sql::MARK_WALKED_PAST_MISSING.text())?;
                for (path, home_root, home_path) in chunk {
                    let Some(home) = find
                        .query_row(params![home_root, home_path], |row| row.get::<_, String>(0))
                        .optional()?
                    else {
                        if absent.contains(path.as_str()) {
                            gone += mark.execute(params![
                                root_id,
                                path,
                                crate::clock::ms(now),
                                run_id
                            ])?;
                        }
                        continue;
                    };
                    let Some(old) = find
                        .query_row(params![root_id, path], |row| row.get::<_, String>(0))
                        .optional()?
                    else {
                        continue;
                    };
                    if mark.execute(params![root_id, path, crate::clock::ms(now), run_id])? > 0 {
                        pairs.push((old, home));
                    }
                }
            }
            let marked = pairs.len() + gone;
            if marked == 0 {
                return Ok(0);
            }
            // A retired double's file is still there, under its home row.
            tx.execute(
                crate::scan::sql::MARK_MISSING.text(),
                params![gone as i64, run_id],
            )?;
            crate::catalog_revision::bump_catalog(tx)?;
            crate::corrections::continuation::continue_same_files(tx, &pairs, now)?;
            Ok(marked)
        });
        let mut guard = self.lock();
        if retired.as_ref().map_or(true, |retired| *retired > 0) {
            guard.catalog_version += 1;
            if let Some(cached) = guard.catalog_cache.get_mut(root_id) {
                for (path, _, _) in moves {
                    cached.entries.remove(path);
                }
            }
        }
        retired
    }

    /// Indexed catalog rows under one scope.
    pub fn indexed_count(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<usize, ScanStoreError> {
        self.read_long(stop, "scan_indexed_count", |conn| {
            Ok(conn
                .query_row(
                    crate::scan::sql::INDEXED_COUNT_IN_SCOPE.text(),
                    params![root_id, scope_relative_path, run_id],
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
                    crate::scan::sql::VANISH_COUNTS.text(),
                    params![root_id, scope_relative_path, run_id],
                    crate::scan::sql::VanishCounts::read,
                )
                .map(|row| {
                    (
                        row.walked_past_count.max(0) as usize,
                        row.total.max(0) as usize,
                    )
                })?)
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
                crate::scan::sql::SCOPE_GUARDED.text(),
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

    /// Rows walked past under the scope whose file this run's inventory holds
    /// at the same place below one of `twins`.
    pub fn rows_found_at_twins(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
        twins: &[&str],
    ) -> Result<usize, ScanStoreError> {
        let twins = serde_json::to_string(twins).map_err(|error| ScanStoreError::Internal {
            message: format!("scan twins did not encode: {error}"),
        })?;
        self.read_long(stop, "scan_rows_found_at_twins", |conn| {
            Ok(conn
                .query_row(
                    crate::scan::sql::ROWS_FOUND_AT_TWINS.text(),
                    params![root_id, scope_relative_path, run_id, twins],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count.max(0) as usize)?)
        })
    }

    /// Refuse a link a walk passed when indexed rows lie under it: a scope of
    /// the run, refused like a rule's scope that is a link, so one rule keeps
    /// them from being judged gone. Answers whether rows lie under it.
    pub fn refuse_link(
        &self,
        stop: &RunStop,
        run_id: &str,
        scope: &ScanScope,
        relative_path: &str,
    ) -> Result<bool, ScanStoreError> {
        self.write(stop, Lane::Background, "scan_refuse_link", |tx| {
            let holds: bool = tx.query_row(
                crate::scan::sql::FOLDER_HOLDS_INDEXED_TRACKS.text(),
                params![scope.root_id, relative_path],
                |row| row.get(0),
            )?;
            if holds {
                tx.execute(
                    crate::scan::sql::ADD_REFUSED_SCOPE.text(),
                    params![
                        run_id,
                        scope.root_id,
                        relative_path,
                        scope.root_path,
                        scope.effective_policy.as_str(),
                        scope.policy_revision,
                        FailureCode::ScopeThroughLink
                    ],
                )?;
            }
            Ok(holds)
        })
    }

    /// Whether the scope's folder went to another spelling, which the walk
    /// read from a whole listing: losing all its rows is no vanished share.
    pub fn scope_renamed(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<bool, ScanStoreError> {
        self.read_long(stop, "scan_scope_renamed", |conn| {
            Ok(conn.query_row(
                crate::scan::sql::SCOPE_RENAMED.text(),
                params![
                    run_id,
                    root_id,
                    scope_relative_path,
                    FailureCode::ScopeRenamed
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
        self.read_long(stop, "scan_missing_paths", |conn| {
            let mut stmt = conn.prepare(crate::scan::sql::MISSING_CATALOG_PATHS.text())?;
            let rows = stmt
                .query_map(params![root_id, scope_relative_path, run_id], |row| {
                    let row = crate::scan::sql::MissingCatalogPath::read(row)?;
                    Ok((row.relative_path, row.held_back))
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
                    RevisionKind::LegacyFloat
                    | RevisionKind::LegacyReview
                    | RevisionKind::Unclassified => {
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
                crate::scan::sql::REFRESH_TRACK_STAT.text(),
            )?;
            for (key, input) in &promotions {
                promote.execute(params![input.4, input.1 as i64, input.2, root_id, key])?;
            }
            if let Some(run_id) = run_id {
                let mut evidence = tx.prepare_cached(
                    crate::scan::sql::RECORD_CLASSIFY_FAILURE.text(),
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
    /// rows included. An approximate size for
    /// the scan a caller is about to request.
    pub fn estimate_scopes(&self, scopes: &[ScanScope]) -> Result<u64, ScanStoreError> {
        if scopes.is_empty() {
            return Ok(0);
        }
        // One count over the union, so nested or repeated scopes never
        // count a track twice. Each scope binds its root and path.
        let sql = crate::scan::sql::scopes_track_count(ListLen::new(scopes.len()));
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
        crate::scan::sql::SHARED_CATALOG_REVISION.text(),
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
    let mut stmt = conn.prepare(crate::scan::sql::LOAD_CATALOG_MAP.text())?;
    let rows = stmt.query_map(params![root_id], |row| {
        let row = crate::scan::sql::CatalogRow::read(row)?;
        Ok((
            row.relative_path,
            CatalogEntry {
                track_id: row.id,
                revision: row.stat_revision,
                size_bytes: row.file_size_bytes as u64,
                mtime_ns: row.file_mtime_ns,
                revision_kind: row.stat_revision_kind,
                tags_read_at: row.tags_read_at.map(crate::clock::at_secs),
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
