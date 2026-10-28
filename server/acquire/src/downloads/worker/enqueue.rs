//! The enqueue pass: queued tasks below the concurrency cap search and
//! enqueue through the source order, journaling each attempt.

use std::collections::HashSet;
use std::sync::Arc;

use super::poll::PollMemory;
use super::{Attempt, DownloadWorker, Pass, failover_exhausted, handled_count, live_attempt};
use crate::downloads::sources::{DownloadSource, SourceError, SourceHandle};
use crate::downloads::state::{AttemptState, TaskStatus};
use crate::downloads::store::{AttemptRow, StoreError, TaskOutcome, TaskRow};
use crate::reason::Reason;
use crate::target::reasons::TrackReason;

/// Enqueue backoff after a pass where no source could serve a task.
const ENQUEUE_BACKOFF_SECONDS: f64 = 15.0 * 60.0;

/// What a copy that gave way to the next one logs as the task's outcome.
const GAVE_WAY_NOTE: &str = "queued at the peer past the preferred-quality wait";

impl DownloadWorker {
    /// Enqueue queued tasks while under the concurrency cap. With no
    /// source on, the queue is held as it is.
    pub(super) async fn enqueue_pass(self: &Arc<Self>, pass: &Pass) {
        if pass.sources.is_empty() {
            // No source is on: queued tasks wait untouched, with no look,
            // no enqueue key and no backoff spent, so the first pass after
            // one is turned on starts them.
            return;
        }
        // The same while the library folder cannot take files: the
        // download would only wait to land.
        if pass.library.is_some() {
            return;
        }
        let Some(live) = self
            .read_step("downloads.live", |store| {
                store.list_active(&[TaskStatus::Downloading, TaskStatus::Processing])
            })
            .await
        else {
            // Without the live count the cap cannot hold; skip this pass.
            return;
        };
        let Some(queued) = self
            .read_step("downloads.queued", |store| {
                store.list_active(&[TaskStatus::Queued])
            })
            .await
        else {
            return;
        };
        self.prune_queue_memory(&queued);
        let mut budget = pass
            .config
            .max_concurrent_downloads
            .saturating_sub(live.len());
        for task in queued {
            if budget == 0 {
                break;
            }
            if self.enqueue_one(pass, &task).await {
                budget -= 1;
            }
        }
    }

    /// Forget backoff and warning marks for tasks that left the queue.
    fn prune_queue_memory(&self, queued: &[TaskRow]) {
        let ids: HashSet<&str> = queued.iter().map(|task| task.id.as_str()).collect();
        if let Ok(mut backoff) = self.enqueue_not_before.lock() {
            backoff.retain(|task_id, _| ids.contains(task_id.as_str()));
        }
        if let Ok(mut warned) = self.unsearchable_warned.lock() {
            warned.retain(|task_id| ids.contains(task_id.as_str()));
        }
        if let Ok(mut looked) = self.reviews.looked.lock() {
            looked.retain(|task_id| ids.contains(task_id.as_str()));
        }
    }

    /// Enqueue one queued task. Answers whether a fetch started.
    async fn enqueue_one(self: &Arc<Self>, pass: &Pass, task: &TaskRow) -> bool {
        let now = pass.now;
        if self.landing_in_flight(&task.id) || Self::parked(task) {
            // A parked task waits for its owner's pick from its search.
            return false;
        }
        if task.artist_name.trim().is_empty() && task.album_title.trim().is_empty() {
            // Edition asks carry no searchable names until the catalog
            // port lands; warn once per task and leave the row queued.
            let fresh = self
                .unsearchable_warned
                .lock()
                .map(|mut warned| warned.insert(task.id.clone()))
                .unwrap_or(false);
            if fresh {
                tracing::warn!(
                    task_id = %task.id,
                    "download task has no searchable names; leaving queued"
                );
            }
            return false;
        }
        if let Some(not_before) = self
            .enqueue_not_before
            .lock()
            .ok()
            .and_then(|backoff| backoff.get(&task.id).copied())
            && now < not_before
        {
            return false;
        }
        let Some(attempts) = self.attempts(&task.id).await else {
            // Without the attempt list the next index is unknown; never
            // guess index 0 and re-post a transfer.
            return false;
        };
        if live_attempt(&attempts).is_some() {
            // A pollable attempt exists; the poll pass owns this task.
            // Queued rows in that state just missed their transition.
            let task_id = task.id.clone();
            self.step("downloads.resume_live", move |store| {
                store.transition_task(&task_id, TaskStatus::Downloading, now, None)
            })
            .await;
            return false;
        }
        let handled = handled_count(&attempts, None);
        if failover_exhausted(handled, pass.config.max_failover_attempts) {
            self.fail_unserved(task, &attempts, now, None).await;
            return false;
        }
        if handled == 0 && task.search_job_id.is_none() && self.start_review(pass, task) {
            // The look runs off the pass; the next pass picks up its link,
            // or leaves the task parked.
            return false;
        }
        // The key marks the window between the client add and the attempt
        // journal write. A repeat claim means a crash landed in it: the
        // enqueue below re-attaches (usenet probes queue and history by the
        // deterministic job name; slskd re-enqueues by content) instead of
        // starting a second transfer.
        let key = format!("enqueue:{}:{handled}", task.id);
        let task_id = task.id.clone();
        let Some(fresh) = self
            .step("downloads.enqueue_key", move |store| {
                store.claim_key(&key, &task_id, "enqueue", now)
            })
            .await
        else {
            return false;
        };
        if !fresh {
            tracing::info!(task_id = %task.id, attempt = handled, "resuming an interrupted enqueue");
        }
        let mut every_source_refused = true;
        // A track's reason from the album path becomes the task's outcome
        // when nothing can serve it.
        let mut reason: Option<Reason> = None;
        let mut sources = pass.ordered_sources();
        // A task started from a manual pick tries the picked source first.
        if handled == 0
            && task.search_job_id.is_some()
            && task.candidate_index.is_some()
            && let Some(at) = sources
                .iter()
                .position(|source| source.tag() == task.source)
        {
            let picked = sources.remove(at);
            sources.insert(0, picked);
        }
        for source in sources {
            // Each source walks its own candidate list: the index is the
            // number of attempts this source already handled.
            let source_index = handled_count(&attempts, Some(&source.tag()));
            match source.enqueue(&task.id, source_index).await {
                Ok(handle) => {
                    self.journal_enqueued(pass, task, source, handled, source_index, &handle, None)
                        .await;
                    return true;
                }
                Err(SourceError::LocalFault(detail)) => {
                    // Our side is broken (disk, mount): back off without
                    // blocklisting a healthy release or trying the other
                    // side onto the same broken mount.
                    tracing::warn!(task_id = %task.id, %detail, "enqueue local fault");
                    self.backoff(&task.id, now);
                    return false;
                }
                Err(error) => {
                    match &error {
                        SourceError::Rejected {
                            reason: refused, ..
                        } => {
                            if TrackReason::from_reason(*refused).is_some() {
                                reason = Some(*refused);
                            }
                        }
                        _ => every_source_refused = false,
                    }
                    tracing::warn!(
                        task_id = %task.id,
                        source = %source.tag(),
                        %error,
                        "enqueue failed; trying the next source"
                    );
                }
            }
        }
        if every_source_refused && !pass.sources.is_empty() {
            // Every configured source answered that it has nothing for
            // this release: the candidate lists are spent.
            self.fail_unserved(task, &attempts, now, reason).await;
            return false;
        }
        self.backoff(&task.id, now);
        false
    }

    /// Settle a task no source can serve any more, keeping its last
    /// attempt. Earlier landings decide how: tracks imported along the way
    /// make it partial, held files make it a failure held for review,
    /// else the track's reason from the album path or no source could
    /// serve it.
    async fn fail_unserved(
        &self,
        task: &TaskRow,
        attempts: &[Attempt],
        now: f64,
        reason: Option<Reason>,
    ) {
        let task_id = task.id.clone();
        let last_attempt = attempts.last().map(|attempt| attempt.row.id.clone());
        // The history and the settle share one transaction: a history that
        // cannot be read settles nothing and the next pass tries again, so
        // imported tracks never end as a plain failure.
        let settled = self
            .step("downloads.fail_unserved", move |store| {
                let history = store.landing_history(&task_id)?;
                let (status, outcome, text) = if history.files_imported > 0 {
                    (
                        TaskStatus::Partial,
                        Reason::TracksMissing,
                        "imported what was found; no source had the rest".to_owned(),
                    )
                } else if let Some(held) = history.held_reason {
                    (
                        TaskStatus::Failed,
                        held,
                        format!("Held for review: {}", held.message()),
                    )
                } else {
                    let reason = reason.unwrap_or(Reason::NoSource);
                    let text = match reason {
                        Reason::NoSource => "no source could serve this release".to_owned(),
                        other => format!("{other}: {} {}", other.message(), other.action()),
                    };
                    (TaskStatus::Failed, reason, text)
                };
                store.finalize_task_and_attempt(
                    &task_id,
                    status,
                    now,
                    Some(TaskOutcome::new(outcome, &text)),
                    last_attempt.as_deref(),
                    true,
                )?;
                Ok(status)
            })
            .await;
        let Some(status) = settled else {
            return;
        };
        let (kind, outcome) = if status == TaskStatus::Partial {
            (
                droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
                "partial",
            )
        } else {
            (
                droppedneedle_plugins::runtime::EventKind::DownloadFailed,
                "failed",
            )
        };
        crate::plugin_events::download_event(&self.plugins, kind, task, &task.source, outcome);
    }

    /// Journal one successful enqueue: the attempt row plus the
    /// Downloading transition in one write, then fresh poll memory. A copy
    /// that gives way names the live attempt it `replaces`: that attempt is
    /// handed to cleanup for discarding, and the task's outcome says why,
    /// in the same write. If the attempt id is already taken (another
    /// writer journaled this slot), the new client job is aborted so it
    /// never runs untracked. Answers whether the attempt was journaled.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn journal_enqueued(
        &self,
        pass: &Pass,
        task: &TaskRow,
        source: &Arc<dyn DownloadSource>,
        attempt_number: i64,
        source_index: i64,
        handle: &SourceHandle,
        replaces: Option<&AttemptRow>,
    ) -> bool {
        let now = pass.now;
        let handle_json = match serde_json::to_string(handle) {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(task_id = %task.id, %error, "client handle encode failed");
                return false;
            }
        };
        let attempt_id = format!("{}-a{attempt_number}", task.id);
        let task_id = task.id.clone();
        let journal_source = source.tag().to_string();
        let job_name = handle.job_name.clone();
        let replaced = replaces.map(|attempt| (attempt.id.clone(), attempt.row_revision));
        let journaled = self
            .step("downloads.enqueued", move |store| {
                let outcome = match &replaced {
                    Some((replaced_id, revision)) => {
                        // A copy that changed since it was polled is left
                        // alone: the write rolls back and the new job is
                        // aborted.
                        store
                            .transition_attempt(
                                replaced_id,
                                *revision,
                                AttemptState::CleanupPending,
                                now,
                                Some("discard"),
                                None,
                                true,
                            )?
                            .ok_or_else(|| {
                                StoreError::InvalidTransition(format!(
                                    "attempt {replaced_id} changed before it could give way"
                                ))
                            })?;
                        Some(TaskOutcome::new(Reason::QueueGaveWay, GAVE_WAY_NOTE))
                    }
                    None => None,
                };
                store.insert_attempt(
                    &attempt_id,
                    &task_id,
                    &journal_source,
                    source_index,
                    &job_name,
                    &handle_json,
                    AttemptState::Acquiring,
                    now,
                )?;
                store.transition_task(&task_id, TaskStatus::Downloading, now, outcome)
            })
            .await;
        if journaled.is_none() {
            if let Err(error) = source.abort(handle).await {
                tracing::warn!(task_id = %task.id, %error, "untracked client job abort failed");
            }
            return false;
        }
        crate::plugin_events::download_event(
            &self.plugins,
            droppedneedle_plugins::runtime::EventKind::DownloadStarted,
            task,
            &source.tag().to_string(),
            "started",
        );
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.insert(
                task.id.clone(),
                PollMemory {
                    enqueued_at: now,
                    last_bytes: 0,
                    last_move_at: now,
                    give_way_from: now,
                },
            );
        }
        true
    }

    /// Push one task's next enqueue attempt out by the backoff window.
    fn backoff(&self, task_id: &str, now: f64) {
        if let Ok(mut backoff) = self.enqueue_not_before.lock() {
            backoff.insert(task_id.to_owned(), now + ENQUEUE_BACKOFF_SECONDS);
        }
    }
}
