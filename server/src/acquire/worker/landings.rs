//! Landings off the worker pass.
//!
//! A finished transfer's import can take minutes (a large album is read,
//! matched, copied, tagged and published), so it never runs inside a
//! pass: the poll pass starts it on its own task and moves on. A per-task
//! in-flight set keeps the poll and enqueue passes off a task while its
//! landing runs, and a reimport goes through the same path, so a reimport
//! and a pass can never land one task twice.
//!
//! The outcome settles the task the way v2's failover loop did: a
//! complete import completes it; held files, or tracks still missing,
//! blocklist this source and fail over to the next candidate (which is
//! asked only for the missing tracks); held or partial is the final word
//! only once the candidates run out.

use std::sync::{Arc, Mutex};

use super::{Attempt, DownloadWorker, Pass, Source, handled_count, now_unix_f64, read_manifest};
use crate::acquire::downloads::quarantine::QuarantineReason;
use crate::acquire::downloads::sources::SourceHandle;
use crate::acquire::downloads::state::{AttemptState, TaskStatus};
use crate::acquire::downloads::store::{AttemptRow, TaskRow};
use crate::acquire::landing::reasons::explain;
use crate::acquire::landing::specs::Disposition;
use crate::acquire::landing::{LandingReport, LandingResult};

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
        source: &Source,
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
        source: Source,
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
        source: &Source,
        handle: &SourceHandle,
        origin: Origin,
    ) {
        let Some(landing) = self.landing.clone() else {
            return;
        };
        let now = now_unix_f64();
        let paths = match source.landed_paths(handle).await {
            Ok(paths) => paths,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "download client could not list the landed files");
                let detail = explain("files_unlisted").message;
                match origin {
                    Origin::Poll => self.wait_or_fail(task, attempt, detail, now).await,
                    Origin::Reimport { was_partial } => {
                        self.settle_failed(task, attempt, detail, true, was_partial, now)
                            .await;
                    }
                }
                return;
            }
        };
        let patient = origin == Origin::Poll && self.waits(&task.id) + 1 < MAX_LANDING_WAITS;
        let staging_root = (self.config)().staging_root;
        let manifest = read_manifest(&staging_root, &task.id).await;
        let report = landing
            .land(task, Some(&attempt.id), manifest.as_ref(), paths, patient)
            .await;
        self.conclude(task, attempt, source, handle, report, origin, now)
            .await;
    }

    /// Settle (or fail over) a task from its landing report.
    #[allow(clippy::too_many_arguments)]
    async fn conclude(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        source: &Source,
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
                // is neither blocklisted nor replaced (v2's local faults).
                // A task that imported anything before stays partial.
                self.settle_short(task, attempt, &report.result, now).await;
            }
            LandingResult::Imported { complete: false } | LandingResult::Held { .. } => {
                if origin == Origin::Poll && self.can_fail_over(task).await {
                    // v2: a release that finished but did not deliver what
                    // was asked is blocklisted before the failover, so the
                    // next candidate is a different source; held rows stay.
                    self.forget_landing(&task.id);
                    self.fail_over(
                        now,
                        task,
                        attempt,
                        source,
                        handle,
                        "the download did not deliver every requested track",
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
                        &rejection.detail,
                        rejection.quarantine,
                    )
                    .await;
                }
                (Disposition::Temporary, Origin::Poll) => {
                    tracing::info!(task_id = %task.id, code = rejection.code, detail = %rejection.detail, "landing deferred");
                    self.wait_or_fail(task, attempt, explain(rejection.code).message, now)
                        .await;
                }
                _ => {
                    // Our own fault, or a reimport that cannot fail over:
                    // keep the files for another try.
                    tracing::info!(task_id = %task.id, code = rejection.code, detail = %rejection.detail, "landing kept for retry");
                    self.settle_failed(
                        task,
                        attempt,
                        explain(rejection.code).message,
                        true,
                        was_partial,
                        now,
                    )
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
        let task_id = task.id.clone();
        let history = self
            .step("downloads.landing_history", move |store| {
                store.landing_history(&task_id)
            })
            .await
            .unwrap_or_default();
        let held = match result {
            LandingResult::Held { code, .. } => {
                Some(format!("Held for review: {}", explain(code).message))
            }
            _ => None,
        };
        if history.files_imported > 0 {
            self.settle(
                task,
                &attempt.id,
                TaskStatus::Partial,
                held.as_deref(),
                false,
                now,
            )
            .await;
        } else {
            let error = held.unwrap_or_else(|| explain("no_tracks").message.to_owned());
            self.settle(
                task,
                &attempt.id,
                TaskStatus::Failed,
                Some(&error),
                false,
                now,
            )
            .await;
        }
    }

    /// Fail a landed task; a reimport of a partial task stays partial.
    async fn settle_failed(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        detail: &str,
        preserve: bool,
        was_partial: bool,
        now: f64,
    ) {
        let status = if was_partial {
            TaskStatus::Partial
        } else {
            TaskStatus::Failed
        };
        self.settle(task, &attempt.id, status, Some(detail), preserve, now)
            .await;
    }

    /// Whether another candidate may still be tried for this task.
    async fn can_fail_over(&self, task: &TaskRow) -> bool {
        let Some(attempts) = self.attempts(&task.id).await else {
            return false;
        };
        handled_count(&attempts, None) < (self.config)().max_failover_attempts
    }

    fn waits(&self, task_id: &str) -> u32 {
        self.landing_waits
            .lock()
            .map(|waits| waits.get(task_id).copied().unwrap_or(0))
            .unwrap_or(MAX_LANDING_WAITS)
    }

    /// Wait another pass for files that are not ready, up to
    /// [`MAX_LANDING_WAITS`]; then fail with the files kept.
    async fn wait_or_fail(&self, task: &TaskRow, attempt: &AttemptRow, detail: &str, now: f64) {
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
            tracing::info!(task_id = %task.id, waited, detail, "landing waits for the next pass");
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
            Some(detail),
            true,
            now,
        )
        .await;
    }

    /// Finalize one landed task and its attempt, announce it, and resolve
    /// its requests.
    async fn settle(
        &self,
        task: &TaskRow,
        attempt_id: &str,
        status: TaskStatus,
        error: Option<&str>,
        preserve_attempt: bool,
        now: f64,
    ) {
        self.forget_landing(&task.id);
        let task_id = task.id.clone();
        let attempt_id = attempt_id.to_owned();
        let error = error.map(str::to_owned);
        let settled = self
            .step("downloads.settle_landing", move |store| {
                store.finalize_task_and_attempt(
                    &task_id,
                    status,
                    now,
                    error.as_deref(),
                    Some(&attempt_id),
                    preserve_attempt,
                )
            })
            .await;
        if settled.is_none() {
            return;
        }
        let (kind, outcome) = match status {
            TaskStatus::Completed => (
                crate::plugins::runtime::EventKind::DownloadCompleted,
                "completed",
            ),
            TaskStatus::Partial => (
                crate::plugins::runtime::EventKind::DownloadCompleted,
                "partial",
            ),
            _ => (crate::plugins::runtime::EventKind::DownloadFailed, "failed"),
        };
        crate::acquire::plugin_events::download_event(
            &self.plugins,
            kind,
            task,
            &task.source,
            outcome,
        );
        if let Some(hook) = &self.settled {
            hook(task.clone(), status).await;
        }
    }

    /// A task a person completed by hand (its last held files imported):
    /// announce it and resolve its requests like a landing would.
    pub async fn announce_completed(&self, task: &TaskRow) {
        crate::acquire::plugin_events::download_event(
            &self.plugins,
            crate::plugins::runtime::EventKind::DownloadCompleted,
            task,
            &task.source,
            "completed",
        );
        if let Some(hook) = &self.settled {
            hook(task.clone(), TaskStatus::Completed).await;
        }
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

    /// Import a failed or short-landed task's files again, right away
    /// (v2 `reimport_task`): the files its last attempt kept go through the
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
            .map_err(ReimportError::Journal)?
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
        let Some(source) = (self.sources)()
            .iter()
            .find(|source| source.journal_source() == attempt.row.source)
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
        let now = now_unix_f64();
        let begun = {
            let task_id = task_id.to_owned();
            self.journal
                .run_foreground("downloads.reimport", move |store| {
                    store.begin_reimport(&task_id, now)
                })
                .await
                .map_err(ReimportError::Journal)?
        };
        if !begun {
            return Ok(None);
        }
        let task = self
            .journal
            .read_task(task_id)
            .await
            .map_err(ReimportError::Journal)?
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
            .map_err(ReimportError::Journal)
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
