//! Scan inventory: discovery scopes, generations, and inventory pages.

use super::*;
use crate::scan::sql;
use droppedneedle_kernel::db::savepoint;

/// Discovery state and the per-run file inventory.
impl SqliteScanStore {
    pub fn scope_discovery_state(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<ScopeDiscoveryState, ScanStoreError> {
        let state: Option<String> = self.read_long(stop, "scan_scope_state", |conn| {
            Ok(conn
                .query_row(
                    "SELECT discovery_state FROM library_scan_run_scopes \
                     WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                    params![run_id, root_id, relative_path],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })?;
        state.map_or(Ok(ScopeDiscoveryState::Pending), |state| {
            ScopeDiscoveryState::parse(&state).ok_or_else(|| ScanStoreError::Internal {
                message: format!("a scan scope holds an unknown discovery state: {state:?}"),
            })
        })
    }

    pub fn scope_discovery_generation(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<u64, ScanStoreError> {
        let generation: Option<i64> = self.read_long(stop, "scan_scope_generation", |conn| {
            Ok(conn
                .query_row(
                    "SELECT discovery_generation FROM library_scan_run_scopes \
                     WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                    params![run_id, root_id, relative_path],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?)
        })?;
        Ok(generation
            .map(|generation| generation.max(1) as u64)
            .unwrap_or(1))
    }

    pub fn complete_scope_discovery(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
        state: ScopeDiscoveryState,
        error_code: Option<FailureCode>,
    ) -> Result<(), ScanStoreError> {
        self.write(stop, Lane::Background, "scan_scope_done", |tx| {
            tx.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = ?1, error_code = ?2 \
                 WHERE run_id = ?3 AND root_id = ?4 AND relative_path = ?5",
                params![state.as_str(), error_code, run_id, root_id, relative_path],
            )?;
            Ok(())
        })
    }

    /// Record the library write revision a scope's walk starts from,
    /// before it reads its first folder. Kept for the rest of the
    /// discovery generation: a write after it keeps its track from
    /// counting as walked past. Restarting the scope's discovery clears it.
    pub fn begin_scope_walk(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<(), ScanStoreError> {
        self.write(stop, Lane::Background, "scan_scope_walk_start", |tx| {
            tx.execute(
                "UPDATE library_scan_run_scopes SET walk_revision = COALESCE(walk_revision, \
                 (SELECT revision FROM library_scan_fences WHERE singleton = 1)) \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![run_id, root_id, relative_path],
            )?;
            Ok(())
        })
    }

    pub fn restart_scope_discovery(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<(), ScanStoreError> {
        self.write(stop, Lane::Background, "scan_scope_restart", |tx| {
            tx.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = 'pending', error_code = NULL, \
                 discovery_generation = discovery_generation + 1, walk_revision = NULL \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![run_id, root_id, relative_path],
            )?;
            Ok(())
        })
    }

    pub fn prepare_discovery_resume(
        &self,
        stop: &RunStop,
        run_id: &str,
    ) -> Result<(), ScanStoreError> {
        self.write(stop, Lane::Background, "scan_discovery_resume", |tx| {
            tx.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = 'pending', \
                 error_code = NULL, discovery_generation = discovery_generation + 1, \
                 walk_revision = NULL \
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
            Ok(())
        })
    }

    pub fn finalize_discovery(
        &self,
        stop: &RunStop,
        run_id: &str,
        updated_at: f64,
    ) -> Result<ScanRun, ScanStoreError> {
        self.write(stop, Lane::Background, "scan_discovery_done", |tx| {
            if load_run(tx, run_id)?.is_none() {
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
            )?;
            bump_scan_stream(tx)?;
            load_run(tx, run_id)?.ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })
        })
    }

    /// Delete the run's stale-generation inventory: rows whose scope is
    /// gone or was walked again since. Read first, then deleted in chunks,
    /// each row only while it is still stale, so a stop leaves the rest
    /// for the next call.
    pub fn cleanup_stale_inventory(
        &self,
        stop: &RunStop,
        run_id: &str,
    ) -> Result<(), ScanStoreError> {
        let stale: Vec<i64> = self.read_long(stop, "scan_stale_inventory", |conn| {
            let rows = conn
                .prepare(sql::STALE_INVENTORY_ROWIDS.text())?
                .query_map(params![run_id], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            Ok(rows)
        })?;
        self.write_chunked(stop, "scan_stale_inventory", &stale, |chunk, tx| {
            super::runs::delete_rowids(tx, sql::DELETE_STALE_INVENTORY_ROW, run_id, chunk)
        })?;
        Ok(())
    }

    /// Record one inventory page. Returns the run revision after the
    /// write plus how many rows could not be stored: when the page fails
    /// as a whole it lands row by row, and each refused row gets a
    /// `WALK_ERROR` failure naming the store error.
    pub fn add_inventory_batch(
        &self,
        stop: &RunStop,
        run_id: &str,
        items: Vec<ScanInventoryItem>,
        expected_run_revision: u64,
        updated_at: f64,
        generation: u64,
    ) -> Result<(u64, usize), ScanStoreError> {
        // The revision gate runs inside the write, so the run cannot move
        // between the check and the rows. A page that fails on a row lands
        // row by row, so one bad row costs one file instead of the page.
        let write = |isolate: bool, name: &'static str| {
            self.write(stop, Lane::Bulk, name, |tx| {
                let current = load_run(tx, run_id)?.ok_or_else(|| ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                })?;
                if current.row_revision != expected_run_revision {
                    return Err(ScanStoreError::StaleRevision {
                        message: "The scan run changed before inventory was recorded.".to_owned(),
                    });
                }
                Ok(write_inventory(
                    tx, run_id, &items, updated_at, generation, isolate,
                )?)
            })
        };
        match write(false, "scan_inventory_page") {
            Err(ScanStoreError::Internal { message }) => {
                tracing::warn!(
                    error = message,
                    "inventory page failed whole; storing row by row"
                );
                write(true, "scan_inventory_page_rows")
            }
            other => other,
        }
    }

    /// The run's last `limit` current-generation inventory rows in
    /// discovery order, each with the catalog track id its path holds.
    pub fn inventory_for_run(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<ScanInventoryItem>, ScanStoreError> {
        // Inventory rows freeze at discovery; the track id lands in the
        // catalog at index time, so it comes from there when missing.
        let mut rows = self.read("scan_run_files", |conn| {
            let mut stmt = conn.prepare(
                "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
                 i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
                 i.policy_revision, COALESCE(i.local_track_id, t.id) AS local_track_id, \
                 i.scope_relative_path \
                 FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 LEFT JOIN local_tracks t ON t.root_id = i.root_id \
                 AND t.relative_path = i.relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ORDER BY i.rowid DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![run_id, limit as i64], map_inventory)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        rows.reverse();
        Ok(rows)
    }

    /// One bounded page of the run's unprocessed inventory rows in
    /// `(root_id, relative_path)` order,
    /// starting after `after` (none for the first page). Returns the page
    /// plus the cursor for the next page (none when the page came up
    /// short). Keyset paging keeps every page O(page): no offset rescan,
    /// no sort, a seek on the primary key. Processing order is path
    /// order. A read failure is `Err`, never an empty page: an empty
    /// page means end-of-run, and the caller must fail the run on `Err`
    /// instead of completing the run short.
    pub fn inventory_page(
        &self,
        stop: &RunStop,
        run_id: &str,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<InventoryPage, ScanStoreError> {
        // Keyset over the (run_id, root_id, relative_path) primary key:
        // every page seeks and reads only its rows. The generation join
        // probes the tiny scopes table per row; no sort, no offset rescan.
        let (after_root, after_path) = after.unwrap_or(("", ""));
        let items = self.read_long(stop, "scan_inventory_page", |conn| {
            let mut stmt = conn.prepare_cached(
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
            )?;
            let rows = stmt
                .query_map(
                    params![run_id, after_root, after_path, limit.max(1) as i64],
                    map_inventory,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
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
                FailureCode::NfcTwinCollision,
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
            item.effective_policy.as_str(),
            item.comparison_result.as_str(),
            item.local_track_id,
        ];
        if !isolate {
            insert.execute(row)?;
            landed += 1;
            continue;
        }
        match savepoint(tx, "inventory_row", || insert.execute(row))? {
            Ok(_) => landed += 1,
            Err(error) => {
                tracing::error!(%error, path = item.relative_path, "inventory row refused");
                failure.execute(params![
                    run_id,
                    item.root_id,
                    item.relative_path,
                    FailureCode::WalkError,
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
