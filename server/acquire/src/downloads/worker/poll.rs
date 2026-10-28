//! The poll pass: live attempts report progress through the watchdog,
//! whose verdicts complete, fail over or requeue their tasks.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{DownloadWorker, Pass, handled_count, live_attempt};
use crate::downloads::quarantine::{QUARANTINE_TTL_SECONDS, QuarantineReason, failover_identities};
use crate::downloads::source_tag::SourceTag;
use crate::downloads::sources::{DownloadSource, SourceError, SourceHandle};
use crate::downloads::state::{AttemptState, TaskStatus};
use crate::downloads::store::{AttemptRow, TaskOutcome, TaskRow};
use crate::downloads::watchdog::PollSample;
use crate::reason::Reason;

/// First wait before polling a source again after its client stopped
/// answering; it doubles while the client keeps failing.
const POLL_BACKOFF_BASE_SECONDS: f64 = 30.0;

/// Longest wait between polls of a client that keeps failing, so one that
/// comes back is polled again within a few minutes.
const POLL_BACKOFF_CAP_SECONDS: f64 = 5.0 * 60.0;

/// A source whose client stopped answering polls: how many poll passes
/// failed in a row and when it is polled again.
#[derive(Debug, Clone, Copy)]
pub(super) struct PollBackoff {
    failures: u32,
    not_before: f64,
}

impl PollBackoff {
    /// The backoff after one more failed poll at `now`: the base doubled
    /// per failure, capped, then spread by up to 20% so sources that went
    /// down together are not retried together.
    fn after_failure(previous: Option<Self>, now: f64) -> Self {
        let failures = previous
            .map_or(0, |backoff| backoff.failures)
            .saturating_add(1);
        let grown = POLL_BACKOFF_BASE_SECONDS * 2_f64.powi(failures.min(16) as i32 - 1);
        let mut byte = [0u8; 1];
        let spread = if getrandom::fill(&mut byte).is_ok() {
            f64::from(byte[0]) / 255.0 * 0.2
        } else {
            0.0
        };
        Self {
            failures,
            not_before: now + grown.min(POLL_BACKOFF_CAP_SECONDS) * (1.0 + spread),
        }
    }
}

/// What one task's poll learned about its source.
enum Polled {
    /// The client answered (or the task needed no poll).
    Answered,
    /// The client did not answer.
    Unavailable(SourceTag),
}

/// Per-task poll memory: byte progress across passes.
#[derive(Debug, Clone)]
pub(super) struct PollMemory {
    pub(super) enqueued_at: f64,
    pub(super) last_bytes: u64,
    pub(super) last_move_at: f64,
    /// Start of the preferred-quality wait: the enqueue, or the last
    /// search that found no other copy.
    pub(super) give_way_from: f64,
}

/// Whether a person picked the copy this attempt fetches: they picked
/// the task's candidate from a search, it is on the attempt's source, and
/// this is that source's first attempt, the one the pick is enqueued as
/// (later ones search). A copy the automatic search review took is not.
fn picked_by_hand(task: &TaskRow, attempt: &AttemptRow) -> bool {
    task.picked_by_person && attempt.source == task.source && attempt.candidate_index == 0
}

impl DownloadWorker {
    /// Poll every live task through the watchdog. Each source's tasks are
    /// polled in their own round, the rounds side by side, so a client that
    /// stops answering only holds up its own tasks.
    pub(super) async fn poll_pass(self: &Arc<Self>, pass: &Pass) {
        let Some(tasks) = self
            .read_step("downloads.live", |store| {
                store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
            })
            .await
        else {
            return;
        };
        let live: HashSet<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.retain(|task_id, _| live.contains(task_id.as_str()));
        }
        if let Ok(mut backoff) = self.poll_backoff.lock() {
            backoff.retain(|tag, _| pass.sources.iter().any(|source| source.tag() == *tag));
        }
        // Grouped by the source of each task's live attempt; the task row's
        // own source names only a candidate a person picked.
        let mut rounds: HashMap<Option<SourceTag>, Vec<&TaskRow>> = HashMap::new();
        for task in &tasks {
            let Some(attempts) = self.attempts(&task.id).await else {
                continue;
            };
            let tag = live_attempt(&attempts).map(|attempt| attempt.row.source.clone());
            rounds.entry(tag).or_default().push(task);
        }
        futures_util::future::join_all(
            rounds
                .into_values()
                .map(|round| self.poll_round(pass, round)),
        )
        .await;
    }

    /// Poll one source's tasks in turn. The first poll its client does not
    /// answer backs the source off; its other tasks wait for a later pass,
    /// and their stall clocks keep running.
    async fn poll_round(self: &Arc<Self>, pass: &Pass, tasks: Vec<&TaskRow>) {
        for task in tasks {
            match self.poll_one(pass, task).await {
                Polled::Answered => {}
                Polled::Unavailable(tag) => {
                    if let Ok(mut backoff) = self.poll_backoff.lock() {
                        let next = PollBackoff::after_failure(backoff.get(&tag).copied(), pass.now);
                        tracing::warn!(
                            source = %tag,
                            failures = next.failures,
                            retry_in_seconds = (next.not_before - pass.now).round(),
                            "download client not answering; polls back off"
                        );
                        backoff.insert(tag, next);
                    }
                }
            }
        }
    }

    /// Whether `tag`'s client is backing off at `now`.
    fn backing_off(&self, tag: &SourceTag, now: f64) -> bool {
        self.poll_backoff
            .lock()
            .is_ok_and(|backoff| backoff.get(tag).is_some_and(|wait| now < wait.not_before))
    }

    /// Poll one live task: judge the sample, then complete, fail over, or
    /// keep waiting. Poll errors never fail the task directly; the stall
    /// math ages a dead client out into failover instead.
    async fn poll_one(self: &Arc<Self>, pass: &Pass, task: &TaskRow) -> Polled {
        let now = pass.now;
        if self.landing_in_flight(&task.id) {
            return Polled::Answered;
        }
        // The pass listed its tasks before any of them were polled; a
        // landing that settled since then must not be seen as live again.
        let task = match self.journal.read_task(&task.id).await {
            Ok(Some(fresh)) if fresh.status == task.status => fresh,
            Ok(_) => return Polled::Answered,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "task re-read failed; poll skipped");
                return Polled::Answered;
            }
        };
        let task = &task;
        let Some(attempts) = self.attempts(&task.id).await else {
            // A read failure is not "no attempt": leave the task alone.
            return Polled::Answered;
        };
        if task.status == TaskStatus::Processing
            && live_attempt(&attempts).is_none()
            && self.resume_landing(pass, task, &attempts).await
        {
            // A landing the last process left running (a reimport whose
            // attempt is no longer live) picks up again instead of the
            // task being requeued for a fresh download.
            return Polled::Answered;
        }
        let Some(attempt) = live_attempt(&attempts).cloned() else {
            // No pollable attempt: back to queued for a fresh enqueue.
            let task_id = task.id.clone();
            self.step("downloads.requeue", move |store| {
                store.transition_task(&task_id, TaskStatus::Queued, now, None)
            })
            .await;
            return Polled::Answered;
        };
        let Some(handle) = attempt.handle.clone() else {
            return Polled::Answered;
        };
        let Some(source) = pass.source_for(&attempt.row.source) else {
            // Plugin sources have no adapter yet; leave the row for a
            // worker that knows them rather than failing it here.
            return Polled::Answered;
        };
        if self.backing_off(&attempt.row.source, now) {
            return Polled::Answered;
        }
        let mut memory = self.poll_memory(&task.id, task, &attempt.row, now);
        match source.poll(&handle).await {
            Ok(progress) => {
                if progress.downloaded_bytes > memory.last_bytes {
                    memory.last_move_at = now;
                    memory.last_bytes = progress.downloaded_bytes;
                }
                let soulseek = attempt.row.source == SourceTag::Soulseek;
                // A Soulseek transfer slskd reports a size for exists as a
                // record, queued at the peer: it waits there rather than
                // counting as never started.
                let materialized = soulseek && progress.total_bytes.is_some();
                let give_way_waited = (soulseek
                    && !picked_by_hand(task, &attempt.row)
                    && handled_count(&attempts, None) < pass.config.max_failover_attempts)
                    .then_some(now - memory.give_way_from);
                let sample = PollSample {
                    elapsed_seconds: now - memory.enqueued_at,
                    idle_seconds: now - memory.last_move_at,
                    has_active_transfer: progress.has_active_transfer,
                    downloaded_bytes: progress.downloaded_bytes,
                    all_terminal: progress.all_terminal,
                    all_succeeded: progress.all_succeeded,
                    materialize_wait_seconds: if progress.downloaded_bytes == 0
                        && !progress.has_active_transfer
                        && !materialized
                    {
                        now - memory.enqueued_at
                    } else {
                        0.0
                    },
                };
                self.store_poll_memory(&task.id, &memory);
                self.apply_verdict(
                    pass,
                    task,
                    &attempt.row,
                    source,
                    &handle,
                    progress,
                    sample,
                    give_way_waited,
                )
                .await;
                self.clear_backoff(&attempt.row.source);
                Polled::Answered
            }
            Err(SourceError::Rejected { reason, detail }) => {
                self.fail_over(
                    now,
                    task,
                    &attempt.row,
                    source,
                    &handle,
                    TaskOutcome::new(reason, &detail),
                    Some(QuarantineReason::DownloadFailed),
                )
                .await;
                self.clear_backoff(&attempt.row.source);
                Polled::Answered
            }
            Err(error) => {
                // Unavailable or local fault: stamp the poll and let the
                // stall math age the task out; a momentary outage must
                // not fail a healthy transfer. Only a client that does not
                // answer backs its source off.
                tracing::warn!(task_id = %task.id, %error, "download poll failed");
                let task_id = task.id.clone();
                self.step("downloads.touch_poll", move |store| {
                    store.touch_poll(&task_id, now)
                })
                .await;
                match error {
                    SourceError::Unavailable(_) => Polled::Unavailable(attempt.row.source.clone()),
                    _ => Polled::Answered,
                }
            }
        }
    }

    /// Forget a source's backoff once its client answers again.
    fn clear_backoff(&self, tag: &SourceTag) {
        if let Ok(mut backoff) = self.poll_backoff.lock() {
            backoff.remove(tag);
        }
    }

    /// Cached poll memory, seeded from the live attempt on first sight.
    fn poll_memory(
        &self,
        task_id: &str,
        task: &TaskRow,
        attempt: &AttemptRow,
        now: f64,
    ) -> PollMemory {
        if let Ok(cache) = self.poll_cache.lock()
            && let Some(memory) = cache.get(task_id)
        {
            return memory.clone();
        }
        // After a restart the preferred-quality wait starts again from the
        // last poll: when it began is not stored. The copy is measured from
        // its own enqueue, as an unbroken run measures it, because the task
        // can be hours older (approval, earlier copies).
        let last_polled_at = task.last_polled_at.unwrap_or(now);
        PollMemory {
            enqueued_at: attempt.created_at,
            last_bytes: 0,
            last_move_at: last_polled_at,
            give_way_from: last_polled_at,
        }
    }

    /// Store refreshed poll memory.
    fn store_poll_memory(&self, task_id: &str, memory: &PollMemory) {
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.insert(task_id.to_owned(), memory.clone());
        }
    }

    /// Apply one watchdog verdict to its task and attempt.
    #[allow(clippy::too_many_arguments)]
    async fn apply_verdict(
        self: &Arc<Self>,
        pass: &Pass,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        progress: crate::downloads::sources::TransferProgress,
        sample: PollSample,
        give_way_waited: Option<f64>,
    ) {
        use crate::downloads::watchdog::WatchdogOutcome;
        let now = pass.now;
        let (reason, text) = match pass.watchdog.evaluate_with(&sample, give_way_waited) {
            WatchdogOutcome::Continue => {
                self.record_progress(pass, task, &progress).await;
                return;
            }
            WatchdogOutcome::Completed if self.landing.is_some() && pass.library.is_some() => {
                // The files wait where they are until the library folder
                // can take them; nothing is read or decided meanwhile.
                self.touch_poll(&task.id, now).await;
                return;
            }
            WatchdogOutcome::Completed if self.landing.is_some() => {
                self.start_landing(task, attempt, source, handle, now).await;
                return;
            }
            WatchdogOutcome::Completed => {
                let task_id = task.id.clone();
                let attempt_id = attempt.id.clone();
                self.step("downloads.complete", move |store| {
                    store.finalize_task_and_attempt(
                        &task_id,
                        TaskStatus::Completed,
                        now,
                        None,
                        Some(&attempt_id),
                        false,
                    )
                })
                .await;
                if let Ok(mut cache) = self.poll_cache.lock() {
                    cache.remove(&task.id);
                }
                crate::plugin_events::download_event(
                    &self.plugins,
                    droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
                    task,
                    &source.tag().to_string(),
                    "completed",
                );
                return;
            }
            WatchdogOutcome::Terminal => (
                Reason::SourceFailed,
                format!(
                    "source batch terminal ({} files succeeded)",
                    progress.succeeded_filenames.len()
                ),
            ),
            WatchdogOutcome::Stalled => (Reason::Stalled, "transfer stalled".to_owned()),
            WatchdogOutcome::GiveWay => {
                if !self.give_way(pass, task, attempt, source).await {
                    // No other copy now: this one keeps its place in the
                    // queue, and the next look waits another full wait.
                    if let Ok(mut cache) = self.poll_cache.lock()
                        && let Some(memory) = cache.get_mut(&task.id)
                    {
                        memory.give_way_from = now;
                    }
                    self.record_progress(pass, task, &progress).await;
                }
                return;
            }
            WatchdogOutcome::QueuedTimeout => {
                (Reason::QueueTimeout, "stuck in the remote queue".to_owned())
            }
            WatchdogOutcome::MaterializeTimeout => (
                Reason::NeverStarted,
                "no transfer materialized after enqueue".to_owned(),
            ),
            WatchdogOutcome::Deadline => (Reason::Deadline, "poll deadline hit".to_owned()),
        };
        self.fail_over(
            now,
            task,
            attempt,
            source,
            handle,
            TaskOutcome::new(reason, &text),
            Some(QuarantineReason::DownloadFailed),
        )
        .await;
    }

    /// Move a copy that sat in a peer's queue past the preferred-quality
    /// wait on to the source's next copy. The next copy is enqueued first,
    /// and only once it is journaled is the waiting one handed to cleanup,
    /// so a peer that is the only one sharing the album keeps its place.
    /// The slow peer is not blocklisted: the next search skips it because
    /// this task already tried it. Answers whether the copy moved.
    async fn give_way(
        &self,
        pass: &Pass,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
    ) -> bool {
        // No new copy while the library folder cannot take files, as for
        // any enqueue.
        if pass.library.is_some() {
            return false;
        }
        let Some(attempts) = self.attempts(&task.id).await else {
            return false;
        };
        let handled = handled_count(&attempts, None);
        let source_index = handled_count(&attempts, Some(&source.tag()));
        let key = format!("enqueue:{}:{handled}", task.id);
        let task_id = task.id.clone();
        let now = pass.now;
        if self
            .step("downloads.enqueue_key", move |store| {
                store.claim_key(&key, &task_id, "enqueue", now)
            })
            .await
            .is_none()
        {
            return false;
        }
        match source.enqueue(&task.id, source_index).await {
            Ok(next) => {
                let moved = self
                    .journal_enqueued(
                        pass,
                        task,
                        source,
                        handled,
                        source_index,
                        &next,
                        Some(attempt),
                    )
                    .await;
                if moved {
                    tracing::info!(task_id = %task.id, "queued copy gave way to the next one");
                }
                moved
            }
            Err(error) => {
                tracing::info!(task_id = %task.id, %error, "no other copy to give way to; still waiting");
                false
            }
        }
    }

    /// Fail one attempt over to the next candidate: blocklist the failed
    /// release under `quarantine` (never for a reason that says nothing
    /// against the source), discard client records, then settle the attempt
    /// and requeue the task in one write so the next pass enqueues the
    /// following candidate.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn fail_over(
        &self,
        now: f64,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        outcome: TaskOutcome<'_>,
        quarantine: Option<QuarantineReason>,
    ) {
        if let Some(quarantine) = quarantine
            && outcome.reason.against_source()
        {
            self.quarantine_failed(task, attempt, handle, quarantine, now)
                .await;
        }
        let settled_state = match source.discard(handle).await {
            Ok(_) => AttemptState::Complete,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "discard failed; cleanup will retry");
                AttemptState::CleanupPending
            }
        };
        let attempt_id = attempt.id.clone();
        let revision = attempt.row_revision;
        let task_id = task.id.clone();
        let (reason, text) = (outcome.reason, outcome.text.to_owned());
        self.step("downloads.fail_over", move |store| {
            store.transition_attempt(
                &attempt_id,
                revision,
                settled_state,
                now,
                Some("discard"),
                None,
                true,
            )?;
            store.transition_task(
                &task_id,
                TaskStatus::Queued,
                now,
                Some(TaskOutcome::new(reason, &text)),
            )
        })
        .await;
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.remove(&task.id);
        }
    }

    /// Blocklist a failed release by source identity, scoped to the album
    /// for retry-clearing.
    async fn quarantine_failed(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        handle: &SourceHandle,
        quarantine: QuarantineReason,
        now: f64,
    ) {
        let identities = failover_identities(handle);
        if identities.is_empty() {
            return;
        }
        let source = attempt.source.to_string();
        let scope = if task.release_group_mbid.is_empty() {
            None
        } else {
            Some(task.release_group_mbid.clone())
        };
        self.step("downloads.quarantine", move |store| {
            for identity in &identities {
                store.record_quarantine(
                    &source,
                    identity,
                    quarantine.as_str(),
                    scope.as_deref(),
                    now,
                    QUARANTINE_TTL_SECONDS,
                )?;
            }
            Ok(())
        })
        .await;
    }
}
