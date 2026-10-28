//! Landings off the worker pass.
//!
//! A finished transfer's import can take minutes (a large album is read,
//! matched, copied, tagged and published), so it never runs inside a
//! pass: the poll pass starts it on its own task and moves on. A per-task
//! in-flight set keeps the poll and enqueue passes off a task while its
//! landing runs, and a reimport goes through the same path, so a reimport
//! and a pass can never land one task twice.
//!
//! The outcome settles the task: a
//! complete import completes it; held files, or tracks still missing,
//! blocklist this source and fail over to the next candidate (which is
//! asked only for the missing tracks); held or partial is the final word
//! only once the candidates run out. A single found only as its album's
//! track is held at once.

use std::sync::{Arc, Mutex};

use super::{Attempt, DownloadWorker, Pass, handled_count};
use crate::downloads::quarantine::QuarantineReason;
use crate::downloads::sources::{DownloadSource, SourceHandle};
use crate::downloads::state::{AttemptState, TaskStatus};
use crate::downloads::store::{AttemptRow, ReimportStart, TaskOutcome, TaskRow};
use crate::landing::specs::Disposition;
use crate::landing::{LandingReport, LandingResult};
use crate::reason::Reason;

/// The outcome a reason records when its own sentence is the log text.
fn said(reason: Reason) -> TaskOutcome<'static> {
    TaskOutcome::new(reason, reason.message())
}

/// Passes a landing may wait on files that are not ready before the task
/// fails (about five minutes at the 30-second cadence).
const MAX_LANDING_WAITS: u32 = 10;

/// Why a reimport could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReimportError {
    /// Imports or the task's download client are not set up.
    Unavailable(String),
    /// The journal could not be read or written.
    Journal(String),
}

/// One task's place in the in-flight set, given back when dropped (also
/// when a landing task panics).
pub(super) struct InFlight {
    set: Arc<Mutex<std::collections::HashSet<String>>>,
    task_id: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Ok(mut set) = self.set.lock() {
            set.remove(&self.task_id);
        }
    }
}

/// Where a landing came from: the poll pass (which can fail over and
/// wait) or an admin reimport (which settles on the spot).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Poll,
    Reimport { was_partial: bool },
}

impl DownloadWorker {
    /// Whether a landing runs for this task right now.
    pub(super) fn landing_in_flight(&self, task_id: &str) -> bool {
        self.in_flight
            .lock()
            .map(|set| set.contains(task_id))
            .unwrap_or(true)
    }

    /// Take the task's in-flight slot; `None` when a landing holds it.
    fn claim(&self, task_id: &str) -> Option<InFlight> {
        let mut set = self.in_flight.lock().ok()?;
        set.insert(task_id.to_owned()).then(|| InFlight {
            set: self.in_flight.clone(),
            task_id: task_id.to_owned(),
        })
    }

    /// Await every landing started so far (shutdown, tests).
    pub async fn wait_for_landings(&self) {
        loop {
            let pending: Vec<_> = match self.landing_tasks.lock() {
                Ok(mut tasks) => tasks.drain(..).collect(),
                Err(_) => return,
            };
            if pending.is_empty() {
                return;
            }
            for task in pending {
                if let Err(error) = task.await {
                    tracing::warn!(%error, "landing task ended abnormally");
                }
            }
        }
    }

    /// A finished transfer: the task shows `processing` and its landing
    /// starts on its own task.
    pub(super) async fn start_landing(
        self: &Arc<Self>,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        now: f64,
    ) {
        let Some(slot) = self.claim(&task.id) else {
            return;
        };
        if task.status != TaskStatus::Processing {
            let task_id = task.id.clone();
            self.step("downloads.processing", move |store| {
                store.transition_task(&task_id, TaskStatus::Processing, now, None)
            })
            .await;
        }
        self.spawn_landing(
            slot,
            task.clone(),
            attempt.clone(),
            source.clone(),
            handle.clone(),
            Origin::Poll,
        );
    }

    /// A task left `processing` with no live attempt (a reimport cut off
    /// by a restart): land the files its kept attempt names again.
    pub(super) async fn resume_landing(
        self: &Arc<Self>,
        pass: &Pass,
        task: &TaskRow,
        attempts: &[Attempt],
    ) -> bool {
        if self.landing.is_none() {
            return false;
        }
        let Some(attempt) = landable_attempt(attempts) else {
            return false;
        };
        let (Some(handle), Some(source)) = (
            attempt.handle.clone(),
            pass.source_for(&attempt.row.source).cloned(),
        ) else {
            return false;
        };
        if pass.library.is_some() {
            // The landing waits for the library folder with the files.
            self.touch_poll(&task.id, pass.now).await;
            return true;
        }
        let Some(slot) = self.claim(&task.id) else {
            return true;
        };
        // With the slot held nothing else can land this task; only a task
        // still `processing` is an interrupted landing (one that settled
        // since the pass read it is left alone).
        let task = match self.journal.read_task(&task.id).await {
            Ok(Some(fresh)) if fresh.status == TaskStatus::Processing => fresh,
            Ok(_) => return true,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "task re-read failed; resume skipped");
                return true;
            }
        };
        tracing::info!(task_id = %task.id, "resuming an interrupted landing");
        self.spawn_landing(
            slot,
            task,
            attempt.row.clone(),
            source,
            handle,
            Origin::Poll,
        );
        true
    }

    fn spawn_landing(
        self: &Arc<Self>,
        slot: InFlight,
        task: TaskRow,
        attempt: AttemptRow,
        source: Arc<dyn DownloadSource>,
        handle: SourceHandle,
        origin: Origin,
    ) {
        let worker = self.clone();
        let join = tokio::spawn(async move {
            let _slot = slot;
            worker
                .run_landing(&task, &attempt, &source, &handle, origin)
                .await;
        });
        match self.landing_tasks.lock() {
            Ok(mut tasks) => {
                tasks.retain(|task| !task.is_finished());
                tasks.push(join);
            }
            Err(_) => tracing::warn!("landing task list unavailable; it runs untracked"),
        }
    }

    /// One landing, start to settle.
    async fn run_landing(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        origin: Origin,
    ) {
        let Some(landing) = self.landing.clone() else {
            return;
        };
        let now = self.now_unix_f64();
        let landed = match source.landed(handle).await {
            Ok(landed) => landed,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "download client could not list the landed files");
                let outcome = said(Reason::FilesUnlisted);
                match origin {
                    Origin::Poll => self.wait_or_fail(task, attempt, outcome, now).await,
                    Origin::Reimport { was_partial } => {
                        self.settle_failed(task, attempt, outcome, true, was_partial, now)
                            .await;
                    }
                }
                return;
            }
        };
        let patient = origin == Origin::Poll && self.waits(&task.id) + 1 < MAX_LANDING_WAITS;
        let report = landing.land(task, Some(&attempt.id), landed, patient).await;
        self.conclude(task, attempt, source, handle, report, origin, now)
            .await;
    }

    /// Settle (or fail over) a task from its landing report.
    #[allow(clippy::too_many_arguments)]
    async fn conclude(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Arc<dyn DownloadSource>,
        handle: &SourceHandle,
        report: LandingReport,
        origin: Origin,
        now: f64,
    ) {
        let was_partial = matches!(origin, Origin::Reimport { was_partial: true });
        match report.result {
            LandingResult::Imported { complete: true } => {
                self.settle(task, &attempt.id, TaskStatus::Completed, None, false, now)
                    .await;
            }
            LandingResult::Imported { complete: false } | LandingResult::Held { .. }
                if report.local_only =>
            {
                // Short only for reasons on our side (a destination already
                // taken, an upgrade waiting): the source did its job, so it
                // is neither blocklisted nor replaced.
                // A task that imported anything before stays partial.
                self.settle_short(task, attempt, &report.result, now).await;
            }
            LandingResult::Held {
                code: Reason::AlbumVersionOnly,
                ..
            } => {
                // Only the album's track was found for a single: other
                // sources would offer the same, so the task stops here and
                // the person decides. The source is not blamed.
                self.settle_short(task, attempt, &report.result, now).await;
            }
            LandingResult::Imported { complete: false } | LandingResult::Held { .. } => {
                if origin == Origin::Poll && self.can_fail_over(task).await {
                    // A release that finished but did not deliver what
                    // was asked is blocklisted before the failover, so the
                    // next candidate is a different source; held rows stay.
                    self.forget_landing(&task.id);
                    self.fail_over(
                        now,
                        task,
                        attempt,
                        source,
                        handle,
                        TaskOutcome::new(
                            Reason::TracksMissing,
                            "the download did not deliver every requested track",
                        ),
                        Some(QuarantineReason::VerifyFailed),
                    )
                    .await;
                    return;
                }
                self.settle_short(task, attempt, &report.result, now).await;
            }
            LandingResult::Rejected(rejection) => match (rejection.disposition, origin) {
                (Disposition::Permanent, Origin::Poll) => {
                    self.forget_landing(&task.id);
                    self.fail_over(
                        now,
                        task,
                        attempt,
                        source,
                        handle,
                        TaskOutcome::new(rejection.code, &rejection.detail),
                        rejection.quarantine,
                    )
                    .await;
                }
                (_, Origin::Poll) if rejection.code == Reason::LibraryFolderMissing => {
                    // The files are fine and the folder may come back by
                    // itself (a share mounting again): the landing waits
                    // without using up its waits and lands again then.
                    tracing::info!(task_id = %task.id, folder = %rejection.detail, "landing waits for the library folder");
                    self.touch_poll(&task.id, now).await;
                }
                (Disposition::Temporary, Origin::Poll)
                    if rejection.code == Reason::JournalUnreadable =>
                {
                    // A busy database never uses up the landing's waits, so
                    // it can never fail the task: the next pass lands again.
                    tracing::info!(task_id = %task.id, detail = %rejection.detail, "landing waits for the database");
                    let task_id = task.id.clone();
                    self.step("downloads.touch_poll", move |store| {
                        store.touch_poll(&task_id, now)
                    })
                    .await;
                }
                (Disposition::Temporary, Origin::Poll) => {
                    tracing::info!(task_id = %task.id, code = %rejection.code, detail = %rejection.detail, "landing deferred");
                    self.wait_or_fail(task, attempt, said(rejection.code), now)
                        .await;
                }
                _ => {
                    // Our own fault, or a reimport that cannot fail over:
                    // keep the files for another try.
                    tracing::info!(task_id = %task.id, code = %rejection.code, detail = %rejection.detail, "landing kept for retry");
                    self.settle_failed(task, attempt, said(rejection.code), true, was_partial, now)
                        .await;
                }
            },
        }
    }

    /// Settle a landing that held files or landed short, with no candidate
    /// left: tracks imported by this or an earlier landing make it partial,
    /// held files alone make it a failure held for review.
    async fn settle_short(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        result: &LandingResult,
        now: f64,
    ) {
        let held = match result {
            LandingResult::Held { code, .. } => {
                Some((*code, format!("Held for review: {}", code.message())))
            }
            _ => None,
        };
        self.forget_landing(&task.id);
        let task_id = task.id.clone();
        let attempt_id = attempt.id.clone();
        // The history and the settle share one transaction: a history that
        // cannot be read settles nothing, so imported tracks never end as
        // a plain failure.
        let settled = self
            .step("downloads.settle_short", move |store| {
                let history = store.landing_history(&task_id)?;
                let (status, outcome) = if history.files_imported > 0 {
                    (TaskStatus::Partial, held)
                } else {
                    let outcome = held.unwrap_or_else(|| {
                        (Reason::NoTracks, Reason::NoTracks.message().to_owned())
                    });
                    (TaskStatus::Failed, Some(outcome))
                };
                store.finalize_task_and_attempt(
                    &task_id,
                    status,
                    now,
                    outcome
                        .as_ref()
                        .map(|(reason, text)| TaskOutcome::new(*reason, text)),
                    Some(&attempt_id),
                    false,
                )?;
                Ok(status)
            })
            .await;
        if let Some(status) = settled {
            self.announce_settled(task, status).await;
        }
    }

    /// Fail a landed task; a reimport of a partial task stays partial.
    async fn settle_failed(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        outcome: TaskOutcome<'_>,
        preserve: bool,
        was_partial: bool,
        now: f64,
    ) {
        let status = if was_partial {
            TaskStatus::Partial
        } else {
            TaskStatus::Failed
        };
        self.settle(task, &attempt.id, status, Some(outcome), preserve, now)
            .await;
    }

    /// Whether another candidate may still be tried for this task.
    async fn can_fail_over(&self, task: &TaskRow) -> bool {
        let Some(attempts) = self.attempts(&task.id).await else {
            return false;
        };
        // While the settings do not read no other candidate is tried. Passes
        // are skipped then, so no poll landing reaches this in practice.
        let Some(config) = self.inputs.config() else {
            return false;
        };
        handled_count(&attempts, None) < config.max_failover_attempts
    }

    fn waits(&self, task_id: &str) -> u32 {
        self.landing_waits
            .lock()
            .map(|waits| waits.get(task_id).copied().unwrap_or(0))
            .unwrap_or(MAX_LANDING_WAITS)
    }

    /// Wait another pass for files that are not ready, up to
    /// [`MAX_LANDING_WAITS`]; then fail with the files kept.
    async fn wait_or_fail(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        outcome: TaskOutcome<'_>,
        now: f64,
    ) {
        let waited = self
            .landing_waits
            .lock()
            .map(|mut waits| {
                let count = waits.entry(task.id.clone()).or_insert(0);
                *count += 1;
                *count
            })
            .unwrap_or(MAX_LANDING_WAITS);
        if waited < MAX_LANDING_WAITS {
            tracing::info!(task_id = %task.id, waited, detail = outcome.text, "landing waits for the next pass");
            let task_id = task.id.clone();
            self.step("downloads.touch_poll", move |store| {
                store.touch_poll(&task_id, now)
            })
            .await;
            return;
        }
        self.settle(
            task,
            &attempt.id,
            TaskStatus::Failed,
            Some(outcome),
            true,
            now,
        )
        .await;
    }

    /// Finalize one landed task, its attempt and its requests, and
    /// announce it.
    async fn settle(
        &self,
        task: &TaskRow,
        attempt_id: &str,
        status: TaskStatus,
        outcome: Option<TaskOutcome<'_>>,
        preserve_attempt: bool,
        now: f64,
    ) {
        self.forget_landing(&task.id);
        let task_id = task.id.clone();
        let attempt_id = attempt_id.to_owned();
        let outcome = outcome.map(|outcome| (outcome.reason, outcome.text.to_owned()));
        let settled = self
            .step("downloads.settle_landing", move |store| {
                store.finalize_task_and_attempt(
                    &task_id,
                    status,
                    now,
                    outcome
                        .as_ref()
                        .map(|(reason, text)| TaskOutcome::new(*reason, text)),
                    Some(&attempt_id),
                    preserve_attempt,
                )
            })
            .await;
        if settled.is_some() {
            self.announce_settled(task, status).await;
        }
    }

    /// Announce a settled landing to plugins. Its requests settled in the
    /// same write as the task.
    async fn announce_settled(&self, task: &TaskRow, status: TaskStatus) {
        let (kind, outcome) = match status {
            TaskStatus::Completed => (
                droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
                "completed",
            ),
            TaskStatus::Partial => (
                droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
                "partial",
            ),
            _ => (
                droppedneedle_plugins::runtime::EventKind::DownloadFailed,
                "failed",
            ),
        };
        crate::plugin_events::download_event(&self.plugins, kind, task, &task.source, outcome);
    }

    /// A task a person completed by hand (its last held files imported):
    /// announce it like a landing would.
    pub async fn announce_completed(&self, task: &TaskRow) {
        crate::plugin_events::download_event(
            &self.plugins,
            droppedneedle_plugins::runtime::EventKind::DownloadCompleted,
            task,
            &task.source,
            "completed",
        );
    }

    /// Drop a task's poll memory and landing waits.
    fn forget_landing(&self, task_id: &str) {
        if let Ok(mut cache) = self.poll_cache.lock() {
            cache.remove(task_id);
        }
        if let Ok(mut waits) = self.landing_waits.lock() {
            waits.remove(task_id);
        }
    }

    /// Import a failed or short-landed task's files again, right away:
    /// the files its last attempt kept go through the
    /// same landing as a finished transfer, without a new search or
    /// download. `Ok(None)` when the task is missing, not reimportable, or
    /// already landing; the row is the task after the landing.
    pub async fn reimport(
        self: &Arc<Self>,
        task_id: &str,
    ) -> Result<Option<TaskRow>, ReimportError> {
        if self.landing.is_none() {
            return Err(ReimportError::Unavailable(
                "imports are not available yet".to_owned(),
            ));
        }
        let Some(task) = self
            .journal
            .read_task(task_id)
            .await
            .map_err(|error| ReimportError::Journal(error.to_string()))?
        else {
            return Ok(None);
        };
        if !matches!(task.status, TaskStatus::Failed | TaskStatus::Partial) {
            return Ok(None);
        }
        let Some(attempts) = self.attempts(task_id).await else {
            return Err(ReimportError::Journal(
                "download attempts unreadable".to_owned(),
            ));
        };
        let Some(attempt) = landable_attempt(&attempts) else {
            return Ok(None);
        };
        let Some(handle) = attempt.handle.clone() else {
            return Ok(None);
        };
        let Some(source) = self
            .inputs
            .sources()
            .iter()
            .find(|source| source.tag() == attempt.row.source)
            .cloned()
        else {
            return Err(ReimportError::Unavailable(format!(
                "the {} download client is not configured",
                attempt.row.source
            )));
        };
        // The slot is held before the task turns `processing`, so no pass
        // can see a processing task without its landing.
        let Some(slot) = self.claim(task_id) else {
            return Ok(None);
        };
        let was_partial = task.status == TaskStatus::Partial;
        let now = self.now_unix_f64();
        let begun = {
            let task_id = task_id.to_owned();
            let attempt_id = attempt.row.id.clone();
            self.journal
                .run_foreground("downloads.reimport", move |store| {
                    store.begin_reimport(&task_id, &attempt_id, now)
                })
                .await
                .map_err(|error| ReimportError::Journal(error.to_string()))?
        };
        match begun {
            ReimportStart::Begun => {}
            ReimportStart::NotReimportable => return Ok(None),
            ReimportStart::FilesGone => {
                return Err(ReimportError::Unavailable(
                    "The download's files were cleaned up, so it can't be imported again."
                        .to_owned(),
                ));
            }
        }
        let task = self
            .journal
            .read_task(task_id)
            .await
            .map_err(|error| ReimportError::Journal(error.to_string()))?
            .unwrap_or(task);
        let row = self
            .attempts(task_id)
            .await
            .and_then(|fresh| {
                fresh
                    .into_iter()
                    .find(|fresh| fresh.row.id == attempt.row.id)
            })
            .map(|fresh| fresh.row)
            .unwrap_or_else(|| attempt.row.clone());
        let worker = self.clone();
        let landed = tokio::spawn(async move {
            let _slot = slot;
            worker
                .run_landing(
                    &task,
                    &row,
                    &source,
                    &handle,
                    Origin::Reimport { was_partial },
                )
                .await;
        });
        // The landing runs on its own task, so a client that hangs up
        // never stops it half way.
        if let Err(error) = landed.await {
            tracing::warn!(task_id, %error, "reimport landing ended abnormally");
        }
        self.journal
            .read_task(task_id)
            .await
            .map_err(|error| ReimportError::Journal(error.to_string()))
    }
}

/// The newest attempt whose kept files a landing can read: its handle is
/// journaled and its cleanup has not run (preserved, or cleanup still
/// owed).
fn landable_attempt(attempts: &[Attempt]) -> Option<&Attempt> {
    attempts.iter().rev().find(|attempt| {
        attempt.handle.is_some()
            && matches!(
                attempt.row.state,
                AttemptState::Preserved | AttemptState::CleanupPending
            )
    })
}
