//! Scan inventory: discovery scopes, generations, and inventory pages.

use super::*;

impl InventoryStore for SqliteScanStore {
    fn scope_discovery_state(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> ScopeDiscoveryState {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT discovery_state FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![run_id, root_id, relative_path],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan scope_discovery_state failed");
                None
            })
            .map(|state| discovery_from_str(&state))
            .unwrap_or(ScopeDiscoveryState::Pending)
    }

    fn scope_discovery_generation(&self, run_id: &str, root_id: &str, relative_path: &str) -> u64 {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT discovery_generation FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![run_id, root_id, relative_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan scope_discovery_generation failed");
                None
            })
            .map(|generation| generation.max(1) as u64)
            .unwrap_or(1)
    }

    fn complete_scope_discovery(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
        state: ScopeDiscoveryState,
        error_code: Option<&str>,
    ) {
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            "UPDATE library_scan_run_scopes SET discovery_state = ?1, error_code = ?2 \
             WHERE run_id = ?3 AND root_id = ?4 AND relative_path = ?5",
            params![
                discovery_to_str(state),
                error_code,
                run_id,
                root_id,
                relative_path
            ],
        ) {
            tracing::error!(%error, "scan complete_scope_discovery failed");
        }
    }

    fn restart_scope_discovery(&self, run_id: &str, root_id: &str, relative_path: &str) {
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            "UPDATE library_scan_run_scopes SET discovery_state = 'pending', error_code = NULL, \
             discovery_generation = discovery_generation + 1 \
             WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
            params![run_id, root_id, relative_path],
        ) {
            tracing::error!(%error, "scan restart_scope_discovery failed");
        }
    }

    fn prepare_discovery_resume(&self, run_id: &str) {
        let mut guard = self.lock();
        let outcome = (|| -> rusqlite::Result<()> {
            let tx = guard.conn.transaction()?;
            tx.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = 'pending', \
                 error_code = NULL, discovery_generation = discovery_generation + 1 \
                 WHERE run_id = ?1 AND discovery_state != 'completed'",
                params![run_id],
            )?;
            tx.execute(
                "UPDATE library_scan_runs SET discovered_count = ( \
                 SELECT COUNT(*) FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ), row_revision = row_revision + 1 WHERE id = ?1",
                params![run_id],
            )?;
            tx.commit()
        })();
        if let Err(error) = outcome {
            tracing::error!(%error, "scan prepare_discovery_resume failed");
        }
    }

    fn finalize_discovery(&self, run_id: &str, updated_at: f64) -> Result<ScanRun, ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard.conn.transaction().map_err(internal)?;
        if load_run(&tx, run_id).map_err(internal)?.is_none() {
            return Err(ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            });
        }
        tx.execute(
            "UPDATE library_scan_runs SET total_count = ( \
             SELECT COUNT(*) FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             ), discovered_count = ( \
             SELECT COUNT(*) FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             ), updated_at = ?2, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?1",
            params![run_id, updated_at],
        )
        .map_err(internal)?;
        bump_scan_stream(&tx).map_err(internal)?;
        let run =
            load_run(&tx, run_id)
                .map_err(internal)?
                .ok_or_else(|| ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                })?;
        tx.commit().map_err(internal)?;
        Ok(run)
    }

    fn cleanup_stale_inventory(&self, run_id: &str) -> usize {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<usize> {
            let tx = guard.conn.transaction()?;
            tx.execute(
                "DELETE FROM library_scan_inventory WHERE rowid IN ( \
                 SELECT i.rowid FROM library_scan_inventory i \
                 LEFT JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 \
                 AND (s.scope_sequence IS NULL \
                 OR s.discovery_generation != i.discovery_generation) \
                 LIMIT ?2)",
                params![run_id, STALE_PAGE as i64],
            )?;
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM library_scan_inventory i \
                 LEFT JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 \
                 AND (s.scope_sequence IS NULL \
                 OR s.discovery_generation != i.discovery_generation)",
                params![run_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            Ok(pending.max(0) as usize)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan cleanup_stale_inventory failed");
            0
        })
    }

    fn add_inventory_batch(
        &self,
        run_id: &str,
        items: Vec<ScanInventoryItem>,
        expected_run_revision: u64,
        updated_at: f64,
        generation: u64,
    ) -> Result<(u64, usize), ScanStoreError> {
        let mut guard = self.lock();
        // Revision gate first: this connection is the only scan-table
        // writer and the mutex serializes it, so the revision cannot move
        // between this check and the write below.
        let current = load_run(&guard.conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
        if current.row_revision != expected_run_revision {
            return Err(ScanStoreError::StaleRevision {
                message: "The scan run changed before inventory was recorded.".to_owned(),
            });
        }
        // One transaction with reused prepared statements; a lock race
        // against the sqlx pool retries instead of losing the batch. A page
        // that fails for any other reason lands row by row, so one bad
        // row costs one file instead of the whole page.
        let whole = retry_on_busy("add_inventory_batch", || {
            let tx = guard
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let outcome = write_inventory(&tx, run_id, &items, updated_at, generation, false)?;
            tx.commit()?;
            Ok(outcome)
        });
        let outcome = match whole {
            Ok(outcome) => outcome,
            Err(error) if is_transient(&error) => return Err(internal(error)),
            Err(error) => {
                tracing::warn!(%error, "inventory page failed whole; storing row by row");
                retry_on_busy("add_inventory_rows", || {
                    let tx = guard
                        .conn
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    let outcome =
                        write_inventory(&tx, run_id, &items, updated_at, generation, true)?;
                    tx.commit()?;
                    Ok(outcome)
                })
                .map_err(internal)?
            }
        };
        Ok(outcome)
    }

    fn inventory_for_run(&self, run_id: &str) -> Vec<ScanInventoryItem> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
             i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
             i.policy_revision, i.local_track_id, i.scope_relative_path \
             FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             ORDER BY i.rowid",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan inventory_for_run failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![run_id], map_inventory)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan inventory_for_run failed");
                Vec::new()
            })
    }

    fn inventory_page(
        &self,
        run_id: &str,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<InventoryPage, ScanStoreError> {
        // Keyset over the (run_id, root_id, relative_path) primary key:
        // every page seeks and reads only its rows. The generation join
        // probes the tiny scopes table per row; no sort, no offset rescan.
        // Read failures surface as Err: an empty page means end-of-run,
        // and the caller retries, then fails the run, instead of
        // completing the run short.
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
             i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
             i.policy_revision, i.local_track_id, i.scope_relative_path \
             FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             AND i.processing_state = 'pending' \
             AND (i.root_id, i.relative_path) > (?2, ?3) \
             ORDER BY i.root_id, i.relative_path LIMIT ?4",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan inventory_page failed");
                return Err(internal(error));
            }
        };
        let (after_root, after_path) = after.unwrap_or(("", ""));
        let items = stmt
            .query_map(
                params![run_id, after_root, after_path, limit.max(1) as i64],
                map_inventory,
            )
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|error| {
                tracing::error!(%error, "scan inventory_page failed");
                internal(error)
            })?;
        let cursor = items
            .last()
            .map(|item| (item.root_id.clone(), item.relative_path.clone()));
        Ok((items, cursor))
    }
}

/// Write one inventory page and bump the run. With `isolate`, every row
/// runs in its own savepoint and a refused row records a failure instead
/// of failing the page. Returns the new run revision and rows refused.
fn write_inventory(
    tx: &Connection,
    run_id: &str,
    items: &[ScanInventoryItem],
    updated_at: f64,
    generation: u64,
    isolate: bool,
) -> rusqlite::Result<(u64, usize)> {
    let mut insert = tx.prepare_cached(
        "INSERT INTO library_scan_inventory (run_id, root_id, relative_path, \
         scope_relative_path, discovery_generation, absolute_path, file_size_bytes, \
         file_mtime_ns, stat_revision, policy_revision, effective_policy, \
         comparison_result, local_track_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT (run_id, root_id, relative_path) DO UPDATE SET \
         scope_relative_path = excluded.scope_relative_path, \
         discovery_generation = excluded.discovery_generation, \
         absolute_path = excluded.absolute_path, \
         file_size_bytes = excluded.file_size_bytes, \
         file_mtime_ns = excluded.file_mtime_ns, \
         stat_revision = excluded.stat_revision, \
         policy_revision = excluded.policy_revision, \
         effective_policy = excluded.effective_policy, \
         comparison_result = excluded.comparison_result, \
         local_track_id = excluded.local_track_id, processing_state = 'pending'",
    )?;
    let mut failure = tx.prepare_cached(
        "INSERT OR IGNORE INTO library_scan_failures \
         (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
         recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', ?6)",
    )?;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut landed = 0i64;
    let mut refused = 0usize;
    for item in items {
        if !seen.insert(item.relative_path.as_str()) {
            failure.execute(params![
                run_id,
                item.root_id,
                item.relative_path,
                failure_codes::NFC_TWIN_COLLISION,
                "Two on-disk names normalize to the same inventory key; the first file won and this twin was skipped.",
                updated_at,
            ])?;
            continue;
        }
        let row = params![
            run_id,
            item.root_id,
            item.relative_path,
            item.scope_relative_path,
            generation as i64,
            item.absolute_path,
            item.file_size_bytes as i64,
            item.file_mtime_ns,
            item.stat_revision,
            item.policy_revision,
            policy_to_str(item.effective_policy),
            verdict_to_str(item.comparison_result),
            item.local_track_id,
        ];
        if !isolate {
            insert.execute(row)?;
            landed += 1;
            continue;
        }
        tx.execute("SAVEPOINT inventory_row", [])?;
        match insert.execute(row) {
            Ok(_) => {
                tx.execute("RELEASE inventory_row", [])?;
                landed += 1;
            }
            Err(error) => {
                tx.execute("ROLLBACK TO inventory_row", [])?;
                tx.execute("RELEASE inventory_row", [])?;
                tracing::error!(%error, path = item.relative_path, "inventory row refused");
                failure.execute(params![
                    run_id,
                    item.root_id,
                    item.relative_path,
                    failure_codes::WALK_ERROR,
                    format!("The inventory row could not be stored: {error}"),
                    updated_at,
                ])?;
                refused += 1;
            }
        }
    }
    tx.execute(
        "UPDATE library_scan_runs SET discovered_count = discovered_count + ?1, \
         updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?3",
        params![landed, updated_at, run_id],
    )?;
    let revision: i64 = tx.query_row(
        "SELECT row_revision FROM library_scan_runs WHERE id = ?1",
        params![run_id],
        |row| row.get(0),
    )?;
    Ok((revision as u64, refused))
}
