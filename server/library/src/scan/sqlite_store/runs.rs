//! Scan runs: requests, claims, transitions, control, counters, recovery.

use super::*;
use crate::scan::models::transition;
use crate::scan::sql;
use droppedneedle_kernel::db::statement::Statement;

/// Scan runs: requests, claims, state transitions, control, failures,
/// counters, and boot recovery.
impl SqliteScanStore {
    /// Record a scan request. A store failure is `Err`, never a
    /// conflict answer naming a run that was never written.
    pub fn request_run(
        &self,
        request: &ScanRequest,
        run_id: &str,
        requested_at: f64,
    ) -> Result<ScanRequestResult, ScanStoreError> {
        self.write_once(Lane::Foreground, "scan_request_run", |tx| {
            Ok(request_run_inner(tx, request, run_id, requested_at)?)
        })
    }

    /// One run with its scopes, for the request path.
    pub fn get_run(
        &self,
        run_id: &str,
    ) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
        self.read("scan_run", |conn| run_with_scopes(conn, run_id))
    }

    /// [`Self::get_run`] for the scan worker, on the long pool.
    pub fn reread_run(
        &self,
        stop: &RunStop,
        run_id: &str,
    ) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
        self.read_long(stop, "scan_run", |conn| run_with_scopes(conn, run_id))
    }

    pub fn list_current(&self) -> Result<Vec<ScanRun>, ScanStoreError> {
        self.read("scan_current_runs", current_runs)
    }

    /// The runs work that must never run beside a scan waits for: the
    /// active and queued runs, then the ended runs that still owe their end
    /// passes, whose passes may yet merge or move albums. On the long pool,
    /// which waits for a connection instead of answering busy.
    pub fn list_current_waiting(&self, stop: &RunStop) -> Result<Vec<ScanRun>, ScanStoreError> {
        self.read_long(stop, "scan_current_runs", |conn| {
            let mut runs = current_runs(conn)?;
            let owing = conn
                .prepare(sql::RUNS_OWING_END_PASSES.text())?
                .query_map(params![FailureCode::RunEndPending], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            runs.extend(owing);
            Ok(runs)
        })
    }

    /// Terminal runs, newest first. `before` is a `(terminal_at, run id)`
    /// keyset cursor: only runs strictly older are listed.
    pub fn list_history(
        &self,
        limit: usize,
        before: Option<(f64, &str)>,
    ) -> Result<Vec<ScanRun>, ScanStoreError> {
        let (before_at, before_id) = match before {
            Some((terminal_at, run_id)) => (Some(terminal_at), Some(run_id)),
            None => (None, None),
        };
        self.read("scan_history", |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE terminal_at IS NOT NULL \
                 AND (?2 IS NULL OR terminal_at < ?2 OR (terminal_at = ?2 AND id < ?3)) \
                 ORDER BY terminal_at DESC, id DESC LIMIT ?1"
            ))?;
            let runs = stmt
                .query_map(params![limit.max(1) as i64, before_at, before_id], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(runs)
        })
    }

    pub fn latest_filesystem_terminal(&self) -> Result<Option<ScanRun>, ScanStoreError> {
        let rows = self.read("scan_latest_terminal", |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE terminal_at IS NOT NULL \
                 ORDER BY terminal_at DESC, id DESC"
            ))?;
            let runs = stmt
                .query_map([], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(runs)
        })?;
        Ok(rows
            .into_iter()
            .find(|run| run.kind != ScanKind::PolicyReconcile || run.aggregate_scope == "all"))
    }

    pub fn claim_next(&self, stop: &RunStop, now: f64) -> Result<Option<ScanRun>, ScanStoreError> {
        self.write(stop, Lane::Background, "scan_claim_next", |tx| {
            claim_next_inner(tx, now)
        })
    }

    /// The started run the worker should take up again: one it was working
    /// on, or one whose pause or stop has yet to take effect.
    pub fn resumable(&self, stop: &RunStop) -> Result<Option<ScanRun>, ScanStoreError> {
        self.read_long(stop, "scan_resumable", |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                         WHERE state IN ('discovering','indexing','reconciling','pausing','stopping') \
                         ORDER BY started_at, id LIMIT 1"
                    ),
                    [],
                    map_run,
                )
                .optional()?)
        })
    }

    /// Move a run by `event`, if it still has row revision
    /// `expected_revision`. A worker write: it waits out a busy database.
    pub fn transition(
        &self,
        stop: &RunStop,
        run_id: &str,
        expected_revision: u64,
        event: Event,
        now: f64,
        terminal_code: Option<FailureCode>,
    ) -> Result<ScanRun, ScanStoreError> {
        self.write(stop, Lane::Background, "scan_transition", |tx| {
            apply_event(
                tx,
                run_id,
                Some(expected_revision),
                event,
                now,
                terminal_code,
            )
        })
    }

    /// A pause, resume or stop asked for through the API. One that already
    /// took effect answers the run as it is; otherwise the run must still
    /// have row revision `expected_revision`. Returns the run and the scan
    /// stream revision.
    pub fn request_control(
        &self,
        run_id: &str,
        event: Event,
        expected_revision: u64,
        now: f64,
    ) -> Result<(ScanRun, u64), ScanStoreError> {
        self.write_once(Lane::Foreground, "scan_control", |tx| {
            let run = load_run(tx, run_id)?.ok_or_else(|| not_found(run_id))?;
            if !already_done(run.state, event) {
                apply_event(tx, run_id, Some(expected_revision), event, now, None)?;
            }
            let run = load_run(tx, run_id)?.ok_or_else(|| not_found(run_id))?;
            Ok((run, read_scan_stream(tx)?))
        })
    }

    /// Record failure rows: those of an index window or an inventory page,
    /// and the walk's and the guard's own.
    pub fn record_failures(
        &self,
        stop: &RunStop,
        run_id: &str,
        failures: &[ScanFailureRecord],
    ) -> Result<(), ScanStoreError> {
        if failures.is_empty() {
            return Ok(());
        }
        // Failure rows are run bookkeeping, not catalog content: they
        // leave the catalog revision (and every read cache) alone.
        self.write(stop, Lane::Bulk, "scan_failures", |tx| {
            let mut insert = tx.prepare_cached(sql::RECORD_FAILURE.text())?;
            for failure in failures {
                insert.execute(params![
                    run_id,
                    failure.root_id,
                    failure.relative_path,
                    failure.failure_code,
                    failure.failure_detail,
                    failure.phase.as_str(),
                    failure.recorded_at,
                ])?;
            }
            Ok(())
        })
    }

    pub fn failures(&self, run_id: &str) -> Result<Vec<ScanFailureRecord>, ScanStoreError> {
        self.read("scan_failures", |conn| {
            let mut stmt = conn.prepare(sql::RUN_FAILURES.text())?;
            let rows = stmt
                .query_map(params![run_id, FailureCode::RunEndPending], map_failure)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One keyset page of a run's recorded failures, oldest first.
    /// `after` is the cursor the previous page handed back; the answer
    /// carries the next cursor while more rows remain (v2
    /// `list_scan_run_failures`).
    pub fn failures_page(
        &self,
        run_id: &str,
        limit: usize,
        after: Option<i64>,
    ) -> Result<(Vec<ScanFailureRecord>, Option<i64>), ScanStoreError> {
        let limit = limit.max(1);
        let mut rows = self.read("scan_failures_page", |conn| {
            let mut stmt = conn.prepare(sql::RUN_FAILURES_PAGE.text())?;
            let rows = stmt
                .query_map(
                    params![
                        run_id,
                        after,
                        (limit + 1) as i64,
                        FailureCode::RunEndPending
                    ],
                    |row| Ok((row.get::<_, i64>("rowid")?, map_failure(row)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let next = if more {
            rows.last().map(|(rowid, _)| *rowid)
        } else {
            None
        };
        Ok((rows.into_iter().map(|(_, failure)| failure).collect(), next))
    }

    pub fn stream_revision(&self, kind: &str) -> Result<u64, ScanStoreError> {
        self.read("scan_stream_revision", |conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?1",
                    params![kind],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| value.max(0) as u64)
                .unwrap_or(0))
        })
    }

    /// Drop the classify snapshot when a run ends. It is a within-scan
    /// accelerator, so an idle scanner holds no catalog memory.
    pub fn drop_classify_snapshot(&self) {
        self.lock().catalog_cache.clear();
    }

    /// Boot recovery: every pause or stop asked for before the restart
    /// takes effect, then the runs still to drive are listed.
    pub fn recover(&self, stop: &RunStop, now: f64) -> Result<Vec<ScanRun>, ScanStoreError> {
        self.write(stop, Lane::Background, "scan_recover", |tx| {
            settle_all(tx, "state IN ('pausing','stopping')", now)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                 WHERE state IN ('queued','discovering','indexing','reconciling','paused') \
                 ORDER BY queued_at, id"
            ))?;
            let runs = stmt
                .query_map([], map_run)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(runs)
        })
    }

    /// Boot recovery while the library is off: every stop asked for before
    /// the restart takes effect. Returns the runs it ended.
    pub fn recover_stopping(
        &self,
        stop: &RunStop,
        now: f64,
    ) -> Result<Vec<ScanRun>, ScanStoreError> {
        self.write(stop, Lane::Background, "scan_recover_stopping", |tx| {
            settle_all(tx, "state = 'stopping'", now)
        })
    }

    /// Run the end passes every ended run still owes, in order, each in its
    /// own background turn after the run's state changed, so no pass keeps
    /// a run from ending. Each turn moves the run's note on to the next
    /// pass, so a stop or a crash resumes where it left off and no pass
    /// runs twice. A pass that fails or runs out of time is logged and
    /// passed over: the library-wide passes (`join_replaced`,
    /// `place_scanned`, `continue_tracks`, `converge_artists`) run again at
    /// the end of every later run, and the run-keyed ones keep their own
    /// "next scan tries again" records where they have them.
    pub fn settle_ended_runs(&self, stop: &RunStop) -> Result<(), ScanStoreError> {
        use super::commit::{END_PASSES, EndWork};
        let owed: Vec<(String, String, f64)> = self.read_long(stop, "scan_end_owed", |conn| {
            let rows = conn
                .prepare(sql::OWED_END_PASSES.text())?
                .query_map(params![FailureCode::RunEndPending], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        for (run_id, next, ended_at) in owed {
            let start = END_PASSES
                .iter()
                .position(|pass| pass.name == next)
                .unwrap_or(0);
            for (at, pass) in END_PASSES.iter().enumerate().skip(start) {
                let following = END_PASSES.get(at + 1).map(|pass| pass.name);
                let advance = |tx: &Transaction<'_>| -> Result<(), ScanStoreError> {
                    match following {
                        Some(name) => tx.execute(
                            sql::NEXT_END_PASS.text(),
                            params![run_id, name, FailureCode::RunEndPending],
                        )?,
                        None => tx.execute(
                            sql::END_PASSES_DONE.text(),
                            params![run_id, FailureCode::RunEndPending],
                        )?,
                    };
                    Ok(())
                };
                let ran = match &pass.work {
                    EndWork::Whole(run) => self.write(stop, Lane::Background, pass.name, |tx| {
                        let enqueued = run(tx, &run_id, ended_at)?;
                        if enqueued > 0 {
                            let column = Counter::IdentificationEnqueued.column();
                            tx.execute(
                                &format!(
                                    "UPDATE library_scan_runs SET {column} = {column} + ?1 \
                                     WHERE id = ?2"
                                ),
                                params![enqueued, run_id],
                            )?;
                        }
                        advance(tx)
                    }),
                    EndWork::ByAlbum { each, tidy } => self
                        .read_long(stop, pass.name, |conn| {
                            Ok(super::commit::albums_left(conn, &run_id)?)
                        })
                        .and_then(|albums| {
                            self.write_chunked_by(
                                stop,
                                pass.name,
                                &albums,
                                END_PASS_ALBUMS,
                                |chunk, tx| {
                                    for album_id in chunk {
                                        each(tx, &run_id, album_id, ended_at)?;
                                    }
                                    Ok(chunk.len())
                                },
                            )
                        })
                        .and_then(|_| {
                            self.write(stop, Lane::Background, pass.name, |tx| {
                                tidy(tx, &run_id, ended_at)?;
                                advance(tx)
                            })
                        }),
                };
                match ran {
                    Ok(()) => {}
                    Err(
                        error @ (ScanStoreError::Stopped
                        | ScanStoreError::ReadCancelled { .. }
                        | ScanStoreError::LaneClosed),
                    ) => return Err(error),
                    Err(error) => {
                        tracing::error!(
                            run_id,
                            pass = pass.name,
                            %error,
                            "a scan's end pass failed and was passed over"
                        );
                        self.write(stop, Lane::Background, "scan_end_pass_over", |tx| {
                            advance(tx)
                        })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Clear finished runs' inventory and album moves, then prune history
    /// past the last [`HISTORY_KEPT`] runs. A run's failure rows stay until
    /// the run itself is pruned, so its history can list them. Every chunk
    /// is a complete change, so a stop or a crash leaves a valid prefix
    /// and the next call carries on from the rows still there.
    pub fn cleanup_terminal_inventory(&self, stop: &RunStop) -> Result<(), ScanStoreError> {
        loop {
            let target: Option<String> = self.read_long(stop, "scan_cleanup_target", |conn| {
                Ok(conn
                    .query_row(
                        sql::CLEANUP_TARGET.text(),
                        params![FailureCode::RunEndPending],
                        |row| row.get(0),
                    )
                    .optional()?)
            })?;
            let Some(target) = target else {
                break;
            };
            let (moves, rows) = self.read_long(stop, "scan_cleanup_rows", |conn| {
                let rowids = |statement: Statement| -> rusqlite::Result<Vec<i64>> {
                    conn.prepare(statement.text())?
                        .query_map(params![target], |row| row.get(0))?
                        .collect()
                };
                Ok((
                    rowids(sql::ALBUM_MOVE_ROWIDS)?,
                    rowids(sql::INVENTORY_ROWIDS)?,
                ))
            })?;
            self.write_chunked(stop, "scan_cleanup_moves", &moves, |chunk, tx| {
                delete_rowids(tx, sql::DELETE_ALBUM_MOVE, &target, chunk)
            })?;
            self.write_chunked(stop, "scan_cleanup_inventory", &rows, |chunk, tx| {
                delete_rowids(tx, sql::DELETE_INVENTORY_ROW, &target, chunk)
            })?;
            self.write(stop, Lane::Background, "scan_cleanup_done", |tx| {
                tx.execute(sql::CLEANUP_DONE.text(), params![target])?;
                Ok(())
            })?;
        }
        let pruned: Vec<String> = self.read_long(stop, "scan_history_prune", |conn| {
            let ids = conn
                .prepare(sql::PRUNED_RUNS.text())?
                .query_map(params![HISTORY_KEPT as i64], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(ids)
        })?;
        self.write_chunked(stop, "scan_history_prune", &pruned, |chunk, tx| {
            let mut delete = tx.prepare_cached(sql::DELETE_RUN.text())?;
            let mut deleted = 0;
            for run_id in chunk {
                deleted += delete.execute(params![run_id])?;
            }
            Ok(deleted)
        })?;
        self.write(stop, Lane::Background, "scan_compat_orphans", |tx| {
            Ok(prune_compat_orphans(tx)?)
        })
    }
}

/// Albums an end pass works through in one turn. One album's work reads
/// every move of the run, so a turn holds far fewer albums than rows.
const END_PASS_ALBUMS: usize = 50;

/// Delete one chunk of a run's rows by rowid.
pub(super) fn delete_rowids(
    tx: &Transaction<'_>,
    statement: Statement,
    run_id: &str,
    rowids: &[i64],
) -> Result<usize, ScanStoreError> {
    let mut delete = tx.prepare_cached(statement.text())?;
    let mut deleted = 0;
    for rowid in rowids {
        deleted += delete.execute(params![rowid, run_id])?;
    }
    Ok(deleted)
}

/// The active and queued runs, the active one first.
fn current_runs(conn: &Connection) -> Result<Vec<ScanRun>, ScanStoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM library_scan_runs \
         WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
         ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, queued_at, id"
    ))?;
    let runs = stmt
        .query_map([], map_run)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(runs)
}

/// One run and its scopes.
fn run_with_scopes(
    conn: &Connection,
    run_id: &str,
) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
    let run = load_run(conn, run_id)?.ok_or_else(|| ScanStoreError::NotFound {
        run_id: run_id.to_owned(),
    })?;
    let scopes = load_scopes(conn, run_id)?;
    let counters = run.counters;
    Ok((run, scopes, counters))
}

/// Drop saved compat play queue entries and bookmarks whose track row is
/// gone, so clients never resume onto an id that can't play. A track that
/// is only missing keeps its rows: an unmounted share may come back. A
/// queue keeps its order, and its current entry moves to the next track
/// still there. Runs inside the caller's write.
fn prune_compat_orphans(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let gone = |table: &str| {
        format!("NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.id = {table}.file_id)")
    };
    let bookmarks = conn.execute(
        &format!(
            "DELETE FROM compat_bookmarks WHERE {}",
            gone("compat_bookmarks")
        ),
        [],
    )?;
    let queue_gone = gone("compat_play_queue_items");
    let users: Vec<String> = conn
        .prepare(&format!(
            "SELECT DISTINCT user_id FROM compat_play_queue_items WHERE {queue_gone}"
        ))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for user in &users {
        let current: Option<i64> = conn
            .query_row(
                "SELECT current_index FROM compat_play_queues WHERE user_id = ?1",
                params![user],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let items: Vec<(i64, String, bool)> = conn
            .prepare(&format!(
                "SELECT item_index, file_id, {queue_gone} FROM compat_play_queue_items \
                 WHERE user_id = ?1 ORDER BY item_index"
            ))?
            .query_map(params![user], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let kept: Vec<(i64, String)> = items
            .into_iter()
            .filter(|(_, _, gone)| !gone)
            .map(|(index, file_id, _)| (index, file_id))
            .collect();
        let new_current = current
            .and_then(|old| kept.iter().position(|(index, _)| *index >= old))
            .and_then(|position| i64::try_from(position).ok());
        conn.execute(
            "DELETE FROM compat_play_queue_items WHERE user_id = ?1",
            params![user],
        )?;
        for (position, (_, file_id)) in (0_i64..).zip(&kept) {
            conn.execute(
                "INSERT INTO compat_play_queue_items (user_id, item_index, file_id) \
                 VALUES (?1, ?2, ?3)",
                params![user, position, file_id],
            )?;
        }
        conn.execute(
            "UPDATE compat_play_queues SET current_index = ?2 WHERE user_id = ?1",
            params![user, new_current],
        )?;
    }
    if bookmarks > 0 || !users.is_empty() {
        tracing::info!(
            bookmarks,
            queues = users.len(),
            "removed saved compat entries for tracks no longer in the library"
        );
    }
    Ok(())
}

fn not_found(run_id: &str) -> ScanStoreError {
    ScanStoreError::NotFound {
        run_id: run_id.to_owned(),
    }
}

/// True when `event` asks for what the run already did or is doing, so a
/// repeated request answers the run as it is.
fn already_done(state: RunState, event: Event) -> bool {
    use super::super::models::Outcome;
    match event {
        Event::Pause => matches!(state, RunState::Pausing(_) | RunState::Paused(_)),
        Event::Resume => matches!(state, RunState::Running(_)),
        Event::Stop => matches!(
            state,
            RunState::Stopping(_)
                | RunState::Ended {
                    outcome: Outcome::Cancelled,
                    ..
                }
        ),
        _ => false,
    }
}

/// Settle every run `filter` selects, each by its own transition. Returns
/// the settled runs.
fn settle_all(
    tx: &Transaction<'_>,
    filter: &str,
    now: f64,
) -> Result<Vec<ScanRun>, ScanStoreError> {
    let ids: Vec<String> = tx
        .prepare(&format!(
            "SELECT id FROM library_scan_runs WHERE {filter} ORDER BY queued_at, id"
        ))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    ids.iter()
        .map(|run_id| apply_event(tx, run_id, None, Event::Settle, now, None))
        .collect()
}

/// Move run `run_id` by `event` and write the state it reaches, in the
/// caller's transaction: the one place a run's state changes. With an
/// `expected_revision`, a run that moved on since is refused. A run that
/// ends takes `terminal_code` and owes its end passes. Each phase's time is
/// added up as the run leaves it.
fn apply_event(
    tx: &Transaction<'_>,
    run_id: &str,
    expected_revision: Option<u64>,
    event: Event,
    now: f64,
    terminal_code: Option<FailureCode>,
) -> Result<ScanRun, ScanStoreError> {
    let run = load_run(tx, run_id)?.ok_or_else(|| not_found(run_id))?;
    if expected_revision.is_some_and(|expected| expected != run.row_revision) {
        return Err(ScanStoreError::StaleRevision {
            message: "The scan run changed before the change was applied.".to_owned(),
        });
    }
    let next = transition(run.state, event).map_err(|refused| ScanStoreError::InvalidControl {
        message: match refused.event {
            Event::Pause => "This scan cannot be paused in its current state.",
            Event::Resume => "Only a paused scan can be resumed.",
            Event::Stop => "This scan cannot be stopped in its current state.",
            _ => "The scan run cannot make that change in its current state.",
        }
        .to_owned(),
    })?;
    let mut timings = run.phase_timings.clone();
    let mut phase_started_at: Option<f64> =
        tx.query_row(sql::PHASE_STARTED_AT.text(), params![run_id], |row| {
            row.get(0)
        })?;
    if run.state.timed() != next.timed() {
        if let (Some(phase), Some(started)) = (run.state.timed(), phase_started_at) {
            *timings.entry(phase).or_default() += (now - started).max(0.0);
        }
        phase_started_at = next.timed().map(|_| now);
    }
    let timings = serde_json::to_string(&timings).map_err(|error| ScanStoreError::Internal {
        message: format!("scan phase timings not written: {error}"),
    })?;
    let ended = next.is_ended();
    tx.execute(
        sql::WRITE_RUN_STATE.text(),
        params![
            run_id,
            next.scan_state().as_str(),
            next.phase().as_str(),
            next.resume_phase().map(ScanPhase::as_str),
            next.requested_control().as_str(),
            (event == Event::Claim).then_some(now),
            ended.then_some(now),
            terminal_code.filter(|_| ended).map(FailureCode::as_str),
            phase_started_at,
            timings,
            now,
        ],
    )?;
    if ended {
        super::commit::owe_end_passes(tx, run_id, now)?;
    }
    bump_scan_stream(tx)?;
    load_run(tx, run_id)?.ok_or_else(|| not_found(run_id))
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
    let has_active = current.iter().any(|run| run.state != RunState::Queued);
    let queued_id = current
        .iter()
        .find(|run| run.state == RunState::Queued)
        .map(|run| run.id.clone());

    let mut scopes_by_run: HashMap<String, Vec<ScanScope>> = HashMap::new();
    for run in &current {
        scopes_by_run.insert(run.id.clone(), load_scopes(conn, &run.id)?);
    }
    // Only a queued run may cover a request.
    let covering = current.iter().find(|run| {
        if run.state != RunState::Queued || run.kind != request.kind {
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
                    scope.effective_policy.as_str(),
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
            request.kind.as_str(),
            request.trigger.as_str(),
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
                scope.effective_policy.as_str(),
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

fn claim_next_inner(tx: &Transaction<'_>, now: f64) -> Result<Option<ScanRun>, ScanStoreError> {
    let blocked: i64 = tx.query_row(
        "SELECT COUNT(*) FROM library_scan_runs \
         WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping')",
        [],
        |row| row.get(0),
    )?;
    if blocked > 0 {
        return Ok(None);
    }
    let candidate: Option<String> = tx
        .query_row(
            "SELECT id FROM library_scan_runs WHERE state = 'queued' ORDER BY rowid LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    candidate
        .map(|candidate| apply_event(tx, &candidate, None, Event::Claim, now, None))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column<T: rusqlite::types::FromSql>(conn: &rusqlite::Connection, sql: &str) -> Vec<T> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn compat_orphans_go_and_the_queue_keeps_its_place() {
        let (_dir, store) = scratch_store();
        let check = |conn: &Connection| -> Result<(), ScanStoreError> {
            conn.execute_batch(
                "INSERT INTO auth_users (id, display_name, created_at) VALUES
                 ('u', 'U', '2026-01-01'), ('v', 'V', '2026-01-01');
             INSERT INTO local_artists (id, display_name, folded_name, kind, created_at,
                 updated_at) VALUES ('ar', 'Artist', 'artist', 'person', 1000, 1000);
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                 album_artist_name, album_artist_name_folded, album_artist_id, grouping_source,
                 created_at, updated_at) VALUES
                 ('al', 'r1', 'g1', 'Album', 'album', 'Artist', 'artist', 'ar', 'automatic',
                  1000, 1000);
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                 path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                 album_title, album_title_folded, disc_number, track_number, duration_seconds,
                 file_format, availability, ingest_source, imported_at, membership_source) VALUES
                 ('a', 'al', 'r1', '/m/a', 'a', 'ha', 1, 1, 'sa', 'A', 'a', 'Album', 'album',
                  1, 1, 1.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'),
                 ('c', 'al', 'r1', '/m/c', 'c', 'hc', 1, 1, 'sc', 'C', 'c', 'Album', 'album',
                  1, 2, 1.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'),
                 ('d', 'al', 'r1', '/m/d', 'd', 'hd', 1, 1, 'sd', 'D', 'd', 'Album', 'album',
                  1, 3, 1.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic');
             INSERT INTO compat_bookmarks (user_id, file_id, position_ms, created_at, changed_at)
                 VALUES ('u', 'a', 1, 0, 0), ('u', 'gone', 2, 0, 0);
             INSERT INTO compat_play_queues (user_id, current_index, updated_at)
                 VALUES ('u', 1, 0), ('v', 0, 0);
             INSERT INTO compat_play_queue_items (user_id, item_index, file_id) VALUES
                 ('u', 0, 'a'), ('u', 1, 'gone'), ('u', 2, 'c'), ('u', 3, 'd'),
                 ('v', 0, 'a');",
            )
            .unwrap();
            prune_compat_orphans(conn).unwrap();
            let bookmarks: Vec<String> = column(conn, "SELECT file_id FROM compat_bookmarks");
            assert_eq!(bookmarks, ["a"]);
            let queue: Vec<String> = column(
                conn,
                "SELECT file_id FROM compat_play_queue_items WHERE user_id = 'u' ORDER BY item_index",
            );
            assert_eq!(queue, ["a", "c", "d"]);
            let indexes: Vec<i64> = column(
                conn,
                "SELECT item_index FROM compat_play_queue_items WHERE user_id = 'u' ORDER BY item_index",
            );
            assert_eq!(indexes, [0, 1, 2]);
            let current: Vec<Option<i64>> = column(
                conn,
                "SELECT current_index FROM compat_play_queues ORDER BY user_id",
            );
            // The next surviving track becomes current; other queues are untouched.
            assert_eq!(current, [Some(1), Some(0)]);
            Ok(())
        };
        store
            .write_once(Lane::Foreground, "test", |tx| check(tx))
            .unwrap();
    }
}
