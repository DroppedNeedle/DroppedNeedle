//! Scan runs: requests, claims, transitions, control, counters, recovery.

use super::*;
use crate::activity::store::EventStream;
use crate::clock::LibraryClock as _;
use crate::scan::models::transition;
use crate::scan::sql;
use droppedneedle_kernel::db::statement::Statement;
use droppedneedle_kernel::db::time::Millis;

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
                .query_map([], map_run)?
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
            let mut stmt = conn.prepare(crate::scan::sql::LIST_HISTORY.text())?;
            let runs = stmt
                .query_map(
                    params![
                        limit.max(1) as i64,
                        before_at.map(crate::clock::ms),
                        before_id
                    ],
                    map_run,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(runs)
        })
    }

    pub fn latest_filesystem_terminal(&self) -> Result<Option<ScanRun>, ScanStoreError> {
        let rows = self.read("scan_latest_terminal", |conn| {
            let mut stmt = conn.prepare(crate::scan::sql::LATEST_FILESYSTEM_TERMINAL.text())?;
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
                .query_row(crate::scan::sql::RESUMABLE.text(), [], map_run)
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
                    crate::clock::ms(failure.recorded_at),
                ])?;
            }
            Ok(())
        })
    }

    pub fn failures(&self, run_id: &str) -> Result<Vec<ScanFailureRecord>, ScanStoreError> {
        self.read("scan_failures", |conn| {
            let mut stmt = conn.prepare(sql::RUN_FAILURES.text())?;
            let rows = stmt
                .query_map(params![run_id], map_failure)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One keyset page of a run's recorded failures, oldest first.
    /// `after` is the cursor the previous page handed back; the answer
    /// carries the next cursor while more rows remain.
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
                .query_map(params![run_id, after, (limit + 1) as i64], |row| {
                    let row = sql::FailurePageRow::read(row)?;
                    Ok((
                        row.rowid,
                        ScanFailureRecord {
                            root_id: row.root_id,
                            relative_path: row.relative_path,
                            failure_code: row.failure_code,
                            recorded_at: crate::clock::at_secs(row.recorded_at),
                            failure_detail: row.failure_detail,
                            phase: row.phase,
                        },
                    ))
                })?
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

    pub fn stream_revision(&self, kind: EventStream) -> Result<u64, ScanStoreError> {
        self.read("scan_stream_revision", |conn| {
            Ok(conn
                .query_row(
                    crate::scan::sql::STREAM_REVISION.text(),
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
            settle_all(tx, sql::RUNS_PAUSING_OR_STOPPING, now)?;
            let mut stmt = tx.prepare(crate::scan::sql::RECOVER.text())?;
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
            settle_all(tx, sql::RUNS_STOPPING, now)
        })
    }

    /// Run the end passes every ended run still owes, in order, each in its
    /// own background turn after the run's state changed, so no pass keeps
    /// a run from ending. Each turn moves the run's note on to the next
    /// pass, so a stop or a crash resumes where it left off and no pass
    /// runs twice.
    ///
    /// A pass that runs out of time or finds the database busy is tried
    /// again at once; if that fails too it is put off by the clock
    /// ([`PUT_OFF_WAITS_SECS`]), and this call answers `false`, leaving it
    /// and every later run's passes until its wait is over. A pass that
    /// has used every wait, or that fails any other way, is logged and
    /// passed over: the library-wide passes (`join_replaced`,
    /// `place_scanned`, `continue_tracks`, `converge_artists`) run again
    /// at the end of every later run, and the run-keyed ones keep their
    /// own "next scan tries again" records where they have them.
    ///
    /// Answers whether every owed pass is settled.
    pub fn settle_ended_runs(&self, stop: &RunStop) -> Result<bool, ScanStoreError> {
        use super::commit::END_PASSES;
        let owed: Vec<(String, String, f64, i64, Option<f64>)> =
            self.read_long(stop, "scan_end_owed", |conn| {
                let rows = conn
                    .prepare(sql::OWED_END_PASSES.text())?
                    .query_map([], |row| {
                        let row = sql::OwedEndPass::read(row)?;
                        Ok((
                            row.id,
                            row.next_end_pass,
                            crate::clock::at_secs(row.terminal_at),
                            row.end_pass_waits_used,
                            row.end_pass_retry_at.map(crate::clock::at_secs),
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })?;
        for (run_id, next, ended_at, used, retry_at) in owed {
            if retry_at.is_some_and(|at| still_waiting(self.clock.now_secs(), at)) {
                return Ok(false);
            }
            let mut waits_used = usize::try_from(used).unwrap_or(0);
            let Some(start) = END_PASSES
                .iter()
                .position(|pass| pass.name.as_str() == next)
            else {
                tracing::warn!(
                    run_id,
                    pass = next,
                    "a scan owes an end pass this release does not have; skipped"
                );
                self.write(stop, Lane::Background, "scan_end_passes_done", |tx| {
                    advance(tx, &run_id, END_PASSES.len())
                })?;
                continue;
            };
            for (at, pass) in END_PASSES.iter().enumerate().skip(start) {
                let mut ran = self.end_pass(stop, &run_id, ended_at, at, END_PASS_ALBUMS);
                if let Err(error) = &ran
                    && tried_again(error)
                {
                    tracing::warn!(run_id, pass = pass.name.as_str(), %error, "a scan's end pass is tried again");
                    Backoff::new().pause(stop, pass.name.as_str())?;
                    ran = self.end_pass(stop, &run_id, ended_at, at, 1);
                }
                match ran {
                    Ok(()) => {}
                    Err(
                        error @ (ScanStoreError::Stopped
                        | ScanStoreError::ReadCancelled { .. }
                        | ScanStoreError::LaneClosed),
                    ) => return Err(error),
                    Err(error) if tried_again(&error) && waits_used < PUT_OFF_WAITS_SECS.len() => {
                        let wait = PUT_OFF_WAITS_SECS[waits_used];
                        tracing::warn!(
                            run_id,
                            pass = pass.name.as_str(),
                            %error,
                            wait_secs = wait,
                            "a scan's end pass is put off; no new scan starts until it settles"
                        );
                        let retry_at = crate::clock::ms(self.clock.now_secs() + wait);
                        self.write(stop, Lane::Background, "scan_end_pass_put_off", |tx| {
                            tx.execute(
                                sql::PUT_OFF_END_PASS.text(),
                                params![run_id, (waits_used + 1) as i64, retry_at],
                            )?;
                            Ok(())
                        })?;
                        return Ok(false);
                    }
                    Err(error) => {
                        tracing::error!(
                            run_id,
                            pass = pass.name.as_str(),
                            %error,
                            "a scan's end pass failed and was passed over"
                        );
                        self.write(stop, Lane::Background, "scan_end_pass_over", |tx| {
                            advance(tx, &run_id, at + 1)
                        })?;
                    }
                }
                // The waits are the pass's own, and moving on cleared the
                // run's record of them.
                waits_used = 0;
            }
        }
        Ok(true)
    }

    /// Run end pass `at` of `run_id` once, a chunk of `albums` albums a
    /// turn where it works album by album, and move the note on past it.
    /// A pass whose albums fail still tidies what the albums done so far
    /// left, and answers the failure.
    fn end_pass(
        &self,
        stop: &RunStop,
        run_id: &str,
        ended_at: f64,
        at: usize,
        albums: usize,
    ) -> Result<(), ScanStoreError> {
        use super::commit::{END_PASSES, EndWork};
        let Some(pass) = END_PASSES.get(at) else {
            return Ok(());
        };
        match &pass.work {
            EndWork::Whole(run) => self.write(stop, Lane::Background, pass.name.as_str(), |tx| {
                let enqueued = run(tx, run_id, ended_at)?;
                if enqueued > 0 {
                    tx.execute(
                        &crate::scan::sql::add_to_counter(Counter::IdentificationEnqueued),
                        params![enqueued, run_id],
                    )?;
                }
                advance(tx, run_id, at + 1)
            }),
            EndWork::ByAlbum { each, tidy } => {
                let kept = self
                    .read_long(stop, pass.name.as_str(), |conn| {
                        Ok(super::commit::albums_left(conn, run_id)?)
                    })
                    .and_then(|left| {
                        self.write_chunked_by(
                            stop,
                            pass.name.as_str(),
                            &left,
                            albums,
                            |chunk, tx| {
                                for album_id in chunk {
                                    each(tx, run_id, album_id, ended_at)?;
                                }
                                Ok(chunk.len())
                            },
                        )
                    });
                match kept {
                    Ok(_) => self.write(stop, Lane::Background, pass.name.as_str(), |tx| {
                        tidy(tx, run_id, ended_at)?;
                        advance(tx, run_id, at + 1)
                    }),
                    Err(
                        error @ (ScanStoreError::Stopped
                        | ScanStoreError::ReadCancelled { .. }
                        | ScanStoreError::LaneClosed),
                    ) => Err(error),
                    Err(error) => {
                        let tidied = self.write(stop, Lane::Background, pass.name.as_str(), |tx| {
                            Ok(tidy(tx, run_id, ended_at)?)
                        });
                        match tidied {
                            Ok(()) => Err(error),
                            // A stop, or a tidy that is worth another try,
                            // says more than the albums' failure.
                            Err(tidy_error)
                                if !tried_again(&error)
                                    && (tried_again(&tidy_error)
                                        || matches!(
                                            tidy_error,
                                            ScanStoreError::Stopped
                                                | ScanStoreError::ReadCancelled { .. }
                                                | ScanStoreError::LaneClosed
                                        )) =>
                            {
                                tracing::error!(run_id, pass = pass.name.as_str(), %error, "a scan's end pass failed and its tidy did not finish");
                                Err(tidy_error)
                            }
                            Err(tidy_error) => {
                                tracing::error!(run_id, pass = pass.name.as_str(), error = %tidy_error, "a scan's end pass could not tidy after a failure");
                                Err(error)
                            }
                        }
                    }
                }
            }
        }
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
                    .query_row(sql::CLEANUP_TARGET.text(), [], |row| row.get(0))
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
/// every move of the run, so a turn holds far fewer albums than rows. A
/// pass tried again after it ran out of time takes one album a turn.
const END_PASS_ALBUMS: usize = 50;

/// How long an end pass that keeps failing on time or a busy database
/// waits before each later try: about half an hour in all. A checkpoint, a
/// backup or swap pressure can hold the database for minutes, and passing
/// over a run-keyed pass can lose a hands-off mark, so the tries are spread
/// well past that. They end because new scans wait meanwhile, and a pass
/// that can never finish must not hold them for good.
const PUT_OFF_WAITS_SECS: [f64; 5] = [60.0, 120.0, 240.0, 480.0, 900.0];

/// Whether a pass put off until `retry_at` still waits at `now`. A retry
/// time further off than the longest wait means the clock moved back, and
/// the pass is due rather than holding new scans until the clock catches up.
fn still_waiting(now: f64, retry_at: f64) -> bool {
    let longest = PUT_OFF_WAITS_SECS.iter().copied().fold(0.0, f64::max);
    retry_at > now && retry_at - now <= longest
}

/// An end pass failure worth one more try: the database was busy, or the
/// turn ran out of time.
fn tried_again(error: &ScanStoreError) -> bool {
    matches!(
        error,
        ScanStoreError::Busy { .. } | ScanStoreError::TxTimeout { .. }
    )
}

/// Move run `run_id` on to the pass at `at`, or to none once none is left.
fn advance(tx: &Transaction<'_>, run_id: &str, at: usize) -> Result<(), ScanStoreError> {
    let next = super::commit::END_PASSES.get(at).map(|pass| pass.name);
    tx.execute(sql::NEXT_END_PASS.text(), params![run_id, next])?;
    Ok(())
}

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
    let mut stmt = conn.prepare(crate::scan::sql::CURRENT_RUNS.text())?;
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
    let bookmarks = conn.execute(crate::scan::sql::DROP_ORPHAN_BOOKMARKS.text(), [])?;
    let users: Vec<String> = conn
        .prepare(crate::scan::sql::USERS_WITH_ORPHAN_QUEUE_ITEMS.text())?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for user in &users {
        let current: Option<i64> = conn
            .query_row(
                crate::scan::sql::QUEUE_CURRENT_INDEX.text(),
                params![user],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let items: Vec<(i64, String, bool)> = conn
            .prepare(crate::scan::sql::QUEUE_ITEMS_OF_USER.text())?
            .query_map(params![user], |row| {
                let row = crate::scan::sql::QueueItem::read(row)?;
                Ok((row.item_index, row.file_id, row.gone))
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
        conn.execute(crate::scan::sql::CLEAR_QUEUE_ITEMS.text(), params![user])?;
        for (position, (_, file_id)) in (0_i64..).zip(&kept) {
            conn.execute(
                crate::scan::sql::ADD_QUEUE_ITEM.text(),
                params![user, position, file_id],
            )?;
        }
        conn.execute(
            crate::scan::sql::SET_QUEUE_CURRENT_INDEX.text(),
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

/// Settle every run the statement `runs` lists, each by its own transition. Returns
/// the settled runs.
fn settle_all(
    tx: &Transaction<'_>,
    runs: Statement,
    now: f64,
) -> Result<Vec<ScanRun>, ScanStoreError> {
    let ids: Vec<String> = tx
        .prepare(runs.text())?
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
            Ok(row.get::<_, Option<Millis>>(0)?.map(crate::clock::at_secs))
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
            (event == Event::Claim).then_some(crate::clock::ms(now)),
            ended.then_some(crate::clock::ms(now)),
            terminal_code.filter(|_| ended).map(FailureCode::as_str),
            phase_started_at.map(crate::clock::ms),
            timings,
            crate::clock::ms(now),
        ],
    )?;
    if ended {
        super::commit::owe_end_passes(tx, run_id)?;
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
    let mut stmt = conn.prepare(crate::scan::sql::OPEN_RUN_TO_JOIN.text())?;
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
            crate::scan::sql::COALESCE_RUN_REQUEST.text(),
            params![crate::clock::ms(requested_at), covering.id],
        )?;
        bump_scan_stream(conn)?;
        let revision: i64 = conn.query_row(
            crate::scan::sql::RUN_REVISION.text(),
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
            crate::scan::sql::LAST_SCOPE_SEQUENCE.text(),
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
                crate::scan::sql::DROP_RUN_SCOPE.text(),
                params![queued_id, existing.root_id, existing.relative_path],
            )?;
        }
        for scope in &additions {
            sequence += 1;
            conn.execute(
                crate::scan::sql::ADD_RUN_SCOPE.text(),
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
        // Quirk (v2): aggregate_scope derives from the request alone.
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
            crate::scan::sql::SET_RUN_AGGREGATE_SCOPE.text(),
            params![aggregate, crate::clock::ms(requested_at), queued_id],
        )?;
        bump_scan_stream(conn)?;
        let revision: i64 = conn.query_row(
            crate::scan::sql::RUN_REVISION.text(),
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
        crate::scan::sql::ADD_RUN.text(),
        params![
            run_id,
            request.kind.as_str(),
            request.trigger.as_str(),
            request.requested_by_user_id,
            aggregate,
            crate::clock::ms(requested_at),
        ],
    )?;
    for (sequence, scope) in request.scopes.iter().enumerate() {
        conn.execute(
            crate::scan::sql::ADD_RUN_SCOPE.text(),
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
    let blocked: i64 = tx.query_row(crate::scan::sql::ACTIVE_RUN_COUNT.text(), [], |row| {
        row.get(0)
    })?;
    if blocked > 0 {
        return Ok(None);
    }
    let candidate: Option<String> = tx
        .query_row(crate::scan::sql::NEXT_QUEUED_RUN.text(), [], |row| {
            row.get(0)
        })
        .optional()?;
    candidate
        .map(|candidate| apply_event(tx, &candidate, None, Event::Claim, now, None))
        .transpose()
}

#[cfg(test)]
mod tests;
