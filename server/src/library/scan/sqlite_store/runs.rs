//! Scan runs: requests, claims, transitions, control, counters, recovery.

use super::*;

impl RunStore for SqliteScanStore {
    fn request_run(
        &self,
        request: &ScanRequest,
        run_id: &str,
        requested_at: f64,
    ) -> Result<ScanRequestResult, ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(internal)?;
        let result = request_run_inner(&tx, request, run_id, requested_at).map_err(internal)?;
        tx.commit().map_err(internal)?;
        Ok(result)
    }

    fn get_run(&self, run_id: &str) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
        let guard = self.lock();
        let run = load_run(&guard.conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
        let scopes = load_scopes(&guard.conn, run_id).map_err(internal)?;
        Ok((run.clone(), scopes, run.counters.clone()))
    }

    fn list_current(&self) -> Vec<ScanRun> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM library_scan_runs \
             WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
             ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, queued_at, id"
        )) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan list_current failed");
                return Vec::new();
            }
        };
        stmt.query_map([], map_run)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan list_current failed");
                Vec::new()
            })
    }

    fn list_history(&self, limit: usize, before: Option<(f64, &str)>) -> Vec<ScanRun> {
        let guard = self.lock();
        let (before_at, before_id) = match before {
            Some((terminal_at, run_id)) => (Some(terminal_at), Some(run_id)),
            None => (None, None),
        };
        let mut stmt = match guard.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE terminal_at IS NOT NULL \
             AND (?2 IS NULL OR terminal_at < ?2 OR (terminal_at = ?2 AND id < ?3)) \
             ORDER BY terminal_at DESC, id DESC LIMIT ?1"
        )) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan list_history failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![limit.max(1) as i64, before_at, before_id], map_run)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan list_history failed");
                Vec::new()
            })
    }

    fn latest_filesystem_terminal(&self) -> Option<ScanRun> {
        let guard = self.lock();
        let mut stmt = guard
            .conn
            .prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE terminal_at IS NOT NULL \
             ORDER BY terminal_at DESC, id DESC"
            ))
            .ok()?;
        let rows: Vec<ScanRun> = stmt
            .query_map([], map_run)
            .ok()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .ok()?;
        rows.into_iter()
            .find(|run| run.kind != ScanKind::PolicyReconcile || run.aggregate_scope == "all")
    }

    fn claim_next(&self, now: f64) -> Option<ScanRun> {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<Option<ScanRun>> {
            let tx = guard
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let run = claim_next_inner(&tx, now)?;
            tx.commit()?;
            Ok(run)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan claim_next failed");
            None
        })
    }

    fn resumable(&self) -> Option<ScanRun> {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                     WHERE state IN ('discovering','indexing','reconciling') \
                     AND requested_control = 'none' \
                     ORDER BY started_at, id LIMIT 1"
                ),
                [],
                map_run,
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan resumable failed");
                None
            })
    }

    fn transition(
        &self,
        run_id: &str,
        expected_state: ScanState,
        expected_revision: u64,
        new_state: ScanState,
        now: f64,
        terminal_code: Option<&str>,
    ) -> Result<ScanRun, ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(internal)?;
        let run = transition_inner(
            &tx,
            run_id,
            expected_state,
            expected_revision,
            new_state,
            now,
            terminal_code,
        )?;
        tx.commit().map_err(internal)?;
        Ok(run)
    }

    fn request_control(
        &self,
        run_id: &str,
        control: ScanControl,
        resume: bool,
        expected_revision: u64,
        now: f64,
    ) -> Result<(ScanRun, u64), ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(internal)?;
        let outcome = request_control_inner(&tx, run_id, control, resume, expected_revision, now)?;
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    fn record_failures(&self, run_id: &str, failures: Vec<ScanFailureRecord>) {
        if failures.is_empty() {
            return;
        }
        let mut guard = self.lock();
        let outcome = (|| -> rusqlite::Result<()> {
            let tx = guard
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            for failure in &failures {
                tx.execute(
                    "INSERT OR IGNORE INTO library_scan_failures \
                     (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                     recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        run_id,
                        failure.root_id,
                        failure.relative_path,
                        failure.failure_code,
                        failure.failure_detail,
                        phase_to_str(failure.phase),
                        failure.recorded_at,
                    ],
                )?;
            }
            tx.commit()
        })();
        // Failure rows are run bookkeeping, not catalog content: they
        // leave the catalog revision (and every read cache) alone.
        if let Err(error) = outcome {
            tracing::error!(%error, "scan record_failures failed");
        }
    }

    fn failures(&self, run_id: &str) -> Vec<ScanFailureRecord> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT root_id, relative_path, failure_code, recorded_at, failure_detail, phase \
             FROM library_scan_failures WHERE run_id = ?1 ORDER BY rowid",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan failures failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![run_id], map_failure)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan failures failed");
                Vec::new()
            })
    }

    fn add_counter(&self, run_id: &str, name: &str, delta: i64) {
        let Some(column) = counter_column(name) else {
            return;
        };
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            &format!("UPDATE library_scan_runs SET {column} = {column} + ?1 WHERE id = ?2"),
            params![delta, run_id],
        ) {
            tracing::error!(%error, "scan add_counter failed");
        }
    }

    fn add_counters(&self, run_id: &str, deltas: &[(&str, i64)]) {
        // Collapse to one UPDATE: duplicate names sum, unknown names drop
        // exactly like repeated add_counter calls.
        let mut summed: HashMap<&'static str, i64> = HashMap::new();
        for (name, delta) in deltas {
            if let Some(column) = counter_column(name) {
                *summed.entry(column).or_insert(0) += *delta;
            }
        }
        if summed.is_empty() {
            return;
        }
        let mut pairs: Vec<(&'static str, i64)> = summed.into_iter().collect();
        pairs.sort_by_key(|pair| pair.0);
        let assignments = pairs
            .iter()
            .map(|(column, _)| format!("{column} = {column} + ?"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(pairs.len() + 1);
        for (_, delta) in &pairs {
            values.push(rusqlite::types::Value::Integer(*delta));
        }
        values.push(rusqlite::types::Value::Text(run_id.to_owned()));
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            &format!("UPDATE library_scan_runs SET {assignments} WHERE id = ?"),
            params_from_iter(values),
        ) {
            tracing::error!(%error, "scan add_counters failed");
        }
    }

    fn set_counter(&self, run_id: &str, name: &str, value: i64) {
        let Some(column) = counter_column(name) else {
            return;
        };
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            &format!("UPDATE library_scan_runs SET {column} = ?1 WHERE id = ?2"),
            params![value, run_id],
        ) {
            tracing::error!(%error, "scan set_counter failed");
        }
    }

    fn stream_revision(&self, kind: &str) -> u64 {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?1",
                params![kind],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan stream_revision failed");
                None
            })
            .map(|value| value.max(0) as u64)
            .unwrap_or(0)
    }

    fn checkpoint_terminal(&self) {
        // The bounded inline autocheckpoint wraps the log mid-scan; fold
        // what remains back now, off the batch path and off the observed
        // scan wall. TRUNCATE also resets the log so the file never
        // accumulates folded frames across scans (RESTART will not reset
        // while the pool holds the WAL open). A lost race with the
        // checkpoint service (or an in-flight pool read) sleeps and
        // retries; whatever remains after attempts run out stays in the
        // WAL for the service, and the next scan's terminal fold retries.
        let guard = self.lock();
        for attempt in 0..TERMINAL_CHECKPOINT_ATTEMPTS {
            let checkpointed = guard
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i32>(2)?,
                    ))
                });
            match checkpointed {
                Ok((_, 0, folded)) => {
                    tracing::debug!(
                        folded,
                        attempts = attempt + 1,
                        "scan terminal checkpoint drained its log"
                    );
                    break;
                }
                Ok((busy, log, folded)) => {
                    tracing::debug!(
                        busy,
                        log,
                        folded,
                        attempt = attempt + 1,
                        "scan terminal checkpoint retrying"
                    );
                    std::thread::sleep(TERMINAL_CHECKPOINT_RETRY);
                }
                Err(error) => {
                    tracing::debug!(%error, "scan terminal checkpoint failed");
                    break;
                }
            }
        }
    }

    fn flush_invalidation(&self, terminal: bool) {
        let mut guard = self.lock();
        if terminal {
            // The classify snapshot is a within-scan accelerator: drop it
            // at every terminal flush so idle scans hold no catalog memory.
            guard.catalog_cache.clear();
        }
        if !terminal || !guard.catalog_dirty {
            return;
        }
        let outcome = (|| -> rusqlite::Result<()> {
            guard.conn.execute(
                "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 0) \
                 ON CONFLICT (singleton) DO NOTHING",
                [],
            )?;
            guard.conn.execute(
                "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
                [],
            )?;
            Ok(())
        })();
        match outcome {
            Ok(()) => guard.catalog_dirty = false,
            Err(error) => tracing::error!(%error, "scan flush_invalidation failed"),
        }
    }

    fn recover(&self, now: f64) -> Vec<ScanRun> {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<Vec<ScanRun>> {
            let tx = guard
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE library_scan_runs SET state = 'cancelled', terminal_at = ?1, \
                 updated_at = ?1, requested_control = 'none', \
                 row_revision = row_revision + 1, event_revision = event_revision + 1 \
                 WHERE state = 'stopping' OR requested_control = 'stop'",
                params![now],
            )?;
            tx.execute(
                "UPDATE library_scan_runs SET state = 'paused', requested_control = 'none', \
                 updated_at = ?1, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 \
                 WHERE state = 'pausing' OR requested_control = 'pause'",
                params![now],
            )?;
            let runs: Vec<ScanRun> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                     WHERE state IN ('queued','discovering','indexing','reconciling','paused') \
                     ORDER BY queued_at, id"
                ))?;
                stmt.query_map([], map_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            if !runs.is_empty() {
                bump_scan_stream(&tx)?;
            }
            tx.commit()?;
            Ok(runs)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan recover failed");
            Vec::new()
        })
    }

    fn recover_stopping(&self, now: f64) -> Vec<ScanRun> {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<Vec<ScanRun>> {
            let tx = guard
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE library_scan_runs SET state = 'cancelled', terminal_at = ?1, \
                 updated_at = ?1, requested_control = 'none', \
                 row_revision = row_revision + 1, event_revision = event_revision + 1 \
                 WHERE state = 'stopping' OR requested_control = 'stop'",
                params![now],
            )?;
            let runs: Vec<ScanRun> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                     WHERE state = 'cancelled' AND terminal_at = ?1 ORDER BY id"
                ))?;
                stmt.query_map(params![now], map_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            tx.commit()?;
            Ok(runs)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan recover_stopping failed");
            Vec::new()
        })
    }

    fn cleanup_terminal_inventory(&self, limit: usize) -> bool {
        let guard = self.lock();
        let pending = (|| -> rusqlite::Result<bool> {
            let target: Option<String> = guard
                .conn
                .query_row(
                    "SELECT id FROM library_scan_runs \
                     WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 1 \
                     ORDER BY terminal_at, id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(target) = target else {
                guard.conn.execute(
                    "DELETE FROM library_scan_runs WHERE id IN ( \
                     SELECT id FROM library_scan_runs \
                     WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 0 \
                     ORDER BY terminal_at DESC, id DESC LIMIT -1 OFFSET ?1)",
                    params![HISTORY_KEPT as i64],
                )?;
                return Ok(false);
            };
            let page = limit.max(1) as i64;
            guard.conn.execute(
                "DELETE FROM library_scan_failures WHERE rowid IN ( \
                 SELECT rowid FROM library_scan_failures \
                 WHERE run_id = ?1 AND failure_code != ?2 LIMIT ?3)",
                params![target, failure_codes::TAG_READ_DEFERRED, page],
            )?;
            if guard.conn.changes() > 0 {
                return Ok(true);
            }
            guard.conn.execute(
                "DELETE FROM library_scan_album_moves WHERE rowid IN ( \
                 SELECT rowid FROM library_scan_album_moves WHERE run_id = ?1 LIMIT ?2)",
                params![target, page],
            )?;
            if guard.conn.changes() > 0 {
                return Ok(true);
            }
            guard.conn.execute(
                "DELETE FROM library_scan_inventory WHERE rowid IN ( \
                 SELECT rowid FROM library_scan_inventory WHERE run_id = ?1 LIMIT ?2)",
                params![target, page],
            )?;
            let remaining: i64 = guard.conn.query_row(
                "SELECT COUNT(*) FROM library_scan_inventory WHERE run_id = ?1",
                params![target],
                |row| row.get(0),
            )?;
            if remaining == 0 {
                guard.conn.execute(
                    "UPDATE library_scan_runs SET inventory_cleanup_pending = 0 WHERE id = ?1",
                    params![target],
                )?;
            }
            Ok(true)
        })();
        pending.unwrap_or_else(|error| {
            tracing::error!(%error, "scan cleanup_terminal_inventory failed");
            false
        })
    }
}

fn transition_inner(
    conn: &Connection,
    run_id: &str,
    expected_state: ScanState,
    expected_revision: u64,
    new_state: ScanState,
    now: f64,
    terminal_code: Option<&str>,
) -> Result<ScanRun, ScanStoreError> {
    let current =
        load_run(conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
    if current.state != expected_state || current.row_revision != expected_revision {
        return Err(ScanStoreError::StaleRevision {
            message: "The scan run changed before the transition was applied.".to_owned(),
        });
    }
    let phase = match new_state {
        ScanState::Discovering => Some(ScanPhase::Discovering),
        ScanState::Indexing => Some(ScanPhase::Indexing),
        ScanState::Reconciling => Some(ScanPhase::Reconciling),
        _ => None,
    };
    if new_state.is_terminal() {
        conn.execute(
            "UPDATE library_scan_runs SET state = ?1, terminal_at = ?2, terminal_code = ?3, \
             updated_at = ?2, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?4",
            params![state_to_str(new_state), now, terminal_code, run_id],
        )
        .map_err(internal)?;
    } else {
        conn.execute(
            "UPDATE library_scan_runs SET state = ?1, updated_at = ?2, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?3",
            params![state_to_str(new_state), now, run_id],
        )
        .map_err(internal)?;
    }
    if let Some(phase) = phase {
        conn.execute(
            "UPDATE library_scan_runs SET phase = ?1 WHERE id = ?2",
            params![phase_to_str(phase), run_id],
        )
        .map_err(internal)?;
    }
    bump_scan_stream(conn).map_err(internal)?;
    load_run(conn, run_id)
        .map_err(internal)?
        .ok_or_else(|| ScanStoreError::NotFound {
            run_id: run_id.to_owned(),
        })
}

fn request_control_inner(
    conn: &Connection,
    run_id: &str,
    control: ScanControl,
    resume: bool,
    expected_revision: u64,
    now: f64,
) -> Result<(ScanRun, u64), ScanStoreError> {
    let snapshot =
        load_run(conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
    let run_state = snapshot.state;
    let current_stream = read_scan_stream(conn);
    if !resume
        && control == ScanControl::Pause
        && matches!(run_state, ScanState::Pausing | ScanState::Paused)
    {
        return Ok((snapshot, current_stream));
    }
    if !resume
        && control == ScanControl::Stop
        && matches!(run_state, ScanState::Stopping | ScanState::Cancelled)
    {
        return Ok((snapshot, current_stream));
    }
    if resume
        && matches!(
            run_state,
            ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
        )
        && snapshot.requested_control == RequestedControl::None
    {
        return Ok((snapshot, current_stream));
    }
    if snapshot.row_revision != expected_revision {
        return Err(ScanStoreError::StaleRevision {
            message: "The scan run changed before the control was applied.".to_owned(),
        });
    }
    if resume {
        let Some(resume_phase) = snapshot.resume_phase else {
            return Err(ScanStoreError::InvalidControl {
                message: "Only a paused scan can be resumed.".to_owned(),
            });
        };
        if run_state != ScanState::Paused {
            return Err(ScanStoreError::InvalidControl {
                message: "Only a paused scan can be resumed.".to_owned(),
            });
        }
        let state = match resume_phase {
            ScanPhase::Discovering => ScanState::Discovering,
            ScanPhase::Indexing => ScanState::Indexing,
            ScanPhase::Reconciling => ScanState::Reconciling,
            ScanPhase::Queued => ScanState::Queued,
        };
        conn.execute(
            "UPDATE library_scan_runs SET state = ?1, requested_control = 'none', \
             resume_phase = NULL, updated_at = ?2, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?3",
            params![state_to_str(state), now, run_id],
        )
        .map_err(internal)?;
    } else if control == ScanControl::Pause {
        if !matches!(
            run_state,
            ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
        ) {
            return Err(ScanStoreError::InvalidControl {
                message: "This scan cannot be paused in its current state.".to_owned(),
            });
        }
        conn.execute(
            "UPDATE library_scan_runs SET state = 'pausing', requested_control = 'pause', \
             resume_phase = phase, updated_at = ?1, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?2",
            params![now, run_id],
        )
        .map_err(internal)?;
    } else if run_state == ScanState::Paused {
        conn.execute(
            "UPDATE library_scan_runs SET state = 'cancelled', requested_control = 'none', \
             terminal_at = ?1, updated_at = ?1, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?2",
            params![now, run_id],
        )
        .map_err(internal)?;
    } else if matches!(
        run_state,
        ScanState::Queued
            | ScanState::Discovering
            | ScanState::Indexing
            | ScanState::Reconciling
            | ScanState::Pausing
    ) {
        if run_state == ScanState::Queued {
            conn.execute(
                "UPDATE library_scan_runs SET state = 'cancelled', requested_control = 'stop', \
                 terminal_at = ?1, updated_at = ?1, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 WHERE id = ?2",
                params![now, run_id],
            )
            .map_err(internal)?;
        } else {
            conn.execute(
                "UPDATE library_scan_runs SET state = 'stopping', requested_control = 'stop', \
                 updated_at = ?1, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 WHERE id = ?2",
                params![now, run_id],
            )
            .map_err(internal)?;
        }
    } else {
        return Err(ScanStoreError::InvalidControl {
            message: "This scan cannot be stopped in its current state.".to_owned(),
        });
    }
    bump_scan_stream(conn).map_err(internal)?;
    let stream_revision = read_scan_stream(conn);
    let run =
        load_run(conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
    Ok((run, stream_revision))
}

fn request_run_inner(
    conn: &Connection,
    request: &ScanRequest,
    run_id: &str,
    requested_at: f64,
) -> rusqlite::Result<ScanRequestResult> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM library_scan_runs \
         WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
         ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, rowid"
    ))?;
    let current: Vec<ScanRun> = stmt
        .query_map([], map_run)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_active = current.iter().any(|run| run.state != ScanState::Queued);
    let queued_id = current
        .iter()
        .find(|run| run.state == ScanState::Queued)
        .map(|run| run.id.clone());

    let mut scopes_by_run: HashMap<String, Vec<ScanScope>> = HashMap::new();
    for run in &current {
        scopes_by_run.insert(run.id.clone(), load_scopes(conn, &run.id)?);
    }
    // Only a queued run may cover a request.
    let covering = current.iter().find(|run| {
        if run.state != ScanState::Queued || run.kind != request.kind {
            return false;
        }
        let Some(scopes) = scopes_by_run.get(&run.id) else {
            return false;
        };
        if scopes.is_empty() {
            return false;
        }
        request.scopes.iter().all(|requested| {
            scopes
                .iter()
                .any(|existing| scope_covers(existing, &requested.root_id, requested))
        })
    });
    if let Some(covering) = covering {
        conn.execute(
            "UPDATE library_scan_runs SET coalesced_request_count = coalesced_request_count + 1, \
             updated_at = ?1, row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = ?2",
            params![requested_at, covering.id],
        )?;
        bump_scan_stream(conn)?;
        let revision: i64 = conn.query_row(
            "SELECT row_revision FROM library_scan_runs WHERE id = ?1",
            params![covering.id],
            |row| row.get(0),
        )?;
        return Ok(ScanRequestResult {
            run_id: covering.id.clone(),
            disposition: Disposition::Coalesced,
            state: ScanState::Queued,
            row_revision: revision as u64,
            queued_reason: None,
            conflicting_kind: None,
        });
    }

    if let Some(queued_id) = queued_id {
        let Some(queued) = current.iter().find(|run| run.id == queued_id) else {
            return fresh_run(conn, request, run_id, requested_at, has_active);
        };
        let queued_scopes = scopes_by_run.get(&queued_id).cloned().unwrap_or_default();
        let incompatible = queued.kind != request.kind
            || queued_scopes
                .iter()
                .any(|scope| scope.policy_revision != request.policy_revision);
        if incompatible {
            return Ok(ScanRequestResult {
                run_id: queued_id,
                disposition: Disposition::Conflict,
                state: ScanState::Queued,
                row_revision: queued.row_revision,
                queued_reason: Some(
                    "The follow-up slot already contains incompatible work.".to_owned(),
                ),
                conflicting_kind: Some(queued.kind),
            });
        }
        // Normalize the union; per root keep the broadest
        // ancestor and drop its descendants.
        let additions: Vec<ScanScope> = request
            .scopes
            .iter()
            .filter(|requested| {
                !queued_scopes.iter().any(|existing| {
                    existing.root_id == requested.root_id
                        && scope_covers_path(&existing.relative_path, &requested.relative_path)
                })
            })
            .cloned()
            .collect();
        let mut sequence: i64 = conn.query_row(
            "SELECT COALESCE(MAX(scope_sequence), -1) FROM library_scan_run_scopes WHERE run_id = ?1",
            params![queued_id],
            |row| row.get(0),
        )?;
        for existing in queued_scopes.iter().filter(|existing| {
            additions.iter().any(|addition| {
                existing.root_id == addition.root_id
                    && scope_covers_path(&addition.relative_path, &existing.relative_path)
                    && existing.relative_path != addition.relative_path
            })
        }) {
            conn.execute(
                "DELETE FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![queued_id, existing.root_id, existing.relative_path],
            )?;
        }
        for scope in &additions {
            sequence += 1;
            conn.execute(
                "INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, scope_id, \
                 relative_path, root_path, effective_policy, policy_revision, estimated_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    queued_id,
                    sequence,
                    scope.root_id,
                    scope.scope_id,
                    scope.relative_path,
                    scope.root_path,
                    policy_to_str(scope.effective_policy),
                    scope.policy_revision,
                    scope.estimated_count,
                ],
            )?;
        }
        // Quirk port: aggregate_scope derives from the request alone.
        let aggregate = if request
            .scopes
            .iter()
            .any(|scope| scope.relative_path == ".")
        {
            "all"
        } else {
            "selected"
        };
        conn.execute(
            "UPDATE library_scan_runs SET aggregate_scope = ?1, updated_at = ?2, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?3",
            params![aggregate, requested_at, queued_id],
        )?;
        bump_scan_stream(conn)?;
        let revision: i64 = conn.query_row(
            "SELECT row_revision FROM library_scan_runs WHERE id = ?1",
            params![queued_id],
            |row| row.get(0),
        )?;
        return Ok(ScanRequestResult {
            run_id: queued_id,
            disposition: Disposition::Expanded,
            state: ScanState::Queued,
            row_revision: revision as u64,
            queued_reason: None,
            conflicting_kind: None,
        });
    }

    fresh_run(conn, request, run_id, requested_at, has_active)
}

fn fresh_run(
    conn: &Connection,
    request: &ScanRequest,
    run_id: &str,
    requested_at: f64,
    has_active: bool,
) -> rusqlite::Result<ScanRequestResult> {
    let aggregate = if request
        .scopes
        .iter()
        .any(|scope| scope.relative_path == ".")
    {
        "all"
    } else {
        "selected"
    };
    conn.execute(
        "INSERT INTO library_scan_runs (id, kind, trigger, requested_by_user_id, state, phase, \
         aggregate_scope, queued_at, updated_at, inventory_cleanup_pending, \
         row_revision, event_revision) \
         VALUES (?1, ?2, ?3, ?4, 'queued', 'queued', ?5, ?6, ?6, 1, 1, 0)",
        params![
            run_id,
            kind_to_str(request.kind),
            trigger_to_str(request.trigger),
            request.requested_by_user_id,
            aggregate,
            requested_at,
        ],
    )?;
    for (sequence, scope) in request.scopes.iter().enumerate() {
        conn.execute(
            "INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, scope_id, \
             relative_path, root_path, effective_policy, policy_revision, estimated_count) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                sequence as i64,
                scope.root_id,
                scope.scope_id,
                scope.relative_path,
                scope.root_path,
                policy_to_str(scope.effective_policy),
                scope.policy_revision,
                scope.estimated_count,
            ],
        )?;
    }
    bump_scan_stream(conn)?;
    Ok(ScanRequestResult {
        run_id: run_id.to_owned(),
        disposition: if has_active {
            Disposition::Queued
        } else {
            Disposition::Started
        },
        state: ScanState::Queued,
        row_revision: 1,
        queued_reason: if has_active {
            Some("Another scan is active.".to_owned())
        } else {
            None
        },
        conflicting_kind: None,
    })
}

fn claim_next_inner(conn: &Connection, now: f64) -> rusqlite::Result<Option<ScanRun>> {
    let blocked: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_scan_runs \
         WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping')",
        [],
        |row| row.get(0),
    )?;
    if blocked > 0 {
        return Ok(None);
    }
    let candidate: Option<String> = conn
        .query_row(
            "SELECT id FROM library_scan_runs WHERE state = 'queued' ORDER BY rowid LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE library_scan_runs SET state = 'discovering', phase = 'discovering', \
         started_at = COALESCE(started_at, ?1), updated_at = ?1, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?2",
        params![now, candidate],
    )?;
    bump_scan_stream(conn)?;
    load_run(conn, &candidate)
}
