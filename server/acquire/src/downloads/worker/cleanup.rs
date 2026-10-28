//! The cleanup pass: claimed journal rows abort and discard client
//! records and settle; kept files that aged out are released.

use std::time::{Duration, Instant};

use super::{DownloadWorker, Pass};
use crate::downloads::recovery::{FAILOVER_CLAIM_LIMIT, FAILOVER_LEASE_SECONDS};
use crate::downloads::sources::SourceHandle;
use crate::downloads::state::{AttemptState, TaskStatus};
use crate::downloads::store::{AttemptRow, StoreError};

/// How long a failed download's files are kept for "Retry import" (90
/// days).
const KEPT_FILES_SECONDS: f64 = 90.0 * 86_400.0;

/// Service health key of the cleanup warning.
const CLEANUP_HEALTH_SERVICE: &str = "acquisition_cleanup";

/// How long the cleanup warning outlives the pass that reported it, so it
/// lapses once the worker stops.
const CLEANUP_HEALTH_TTL: Duration = Duration::from_secs(90);

impl DownloadWorker {
    /// Claim due cleanup rows and settle them: abort a cancelled task's
    /// live transfer, discard client records, then mark complete; a
    /// failure defers with backoff.
    pub(super) async fn cleanup_pass(&self, pass: &Pass) {
        let now = pass.now;
        let worker_id = pass.config.worker_id.clone();
        let Some(claimed) = self
            .step("downloads.cleanup_claim", move |store| {
                store.claim_cleanup_attempts(
                    &worker_id,
                    now,
                    FAILOVER_CLAIM_LIMIT,
                    FAILOVER_LEASE_SECONDS,
                )
            })
            .await
        else {
            return;
        };
        for attempt in claimed {
            self.cleanup_one(pass, &attempt).await;
        }
        self.report_cleanup_health().await;
    }

    /// Show downloads whose source files keep failing to clear on service
    /// health, or clear the warning once none are left. A read failure
    /// leaves the entry as it is.
    async fn report_cleanup_health(&self) {
        let Some(count) = self
            .read_step("downloads.cleanup_count", |store| {
                store.cleanup_warning_count()
            })
            .await
        else {
            return;
        };
        if count == 0 {
            self.health.heal(CLEANUP_HEALTH_SERVICE);
            return;
        }
        let plural = if count == 1 { "" } else { "s" };
        self.health.report(
            CLEANUP_HEALTH_SERVICE,
            "source files",
            format!(
                "Temporary files couldn't be removed for {count} download{plural}. \
                 Your library is safe. Retrying automatically."
            ),
            CLEANUP_HEALTH_TTL,
            Instant::now(),
        );
    }

    /// Settle one claimed cleanup row.
    async fn cleanup_one(&self, pass: &Pass, attempt: &AttemptRow) {
        let now = pass.now;
        if self.landing_in_flight(&attempt.task_id) {
            // A landing (a reimport) is reading these files: try later.
            self.defer_cleanup(attempt, "landing_in_flight", now).await;
            return;
        }
        let read = async {
            Ok::<_, StoreError>((
                self.journal.read_attempt_handle(&attempt.id).await?,
                self.journal.read_task(&attempt.task_id).await?,
            ))
        };
        let (handle_json, task) = match read.await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(attempt_id = %attempt.id, %error, "cleanup read failed");
                return;
            }
        };
        let decoded = handle_json.and_then(|json| match serde_json::from_str(&json) {
            Ok(handle) => Some(handle),
            Err(error) => {
                tracing::warn!(
                    attempt_id = %attempt.id,
                    %error,
                    "stored client handle does not decode; cleaning by job name"
                );
                None
            }
        });
        let handle: SourceHandle = decoded.unwrap_or_else(|| SourceHandle {
            job_name: attempt.job_name.clone(),
            ..SourceHandle::new(attempt.source.clone())
        });
        let Some(source) = pass.source_for(&attempt.source) else {
            // No adapter for this source: defer with backoff rather than
            // spinning on a row this worker can never settle.
            self.defer_cleanup(attempt, "no_adapter", now).await;
            return;
        };
        let cancelled = task.is_some_and(|task| task.status == TaskStatus::Cancelled);
        if cancelled
            && attempt.disposition == "discard"
            && let Err(error) = source.abort(&handle).await
        {
            tracing::warn!(attempt_id = %attempt.id, %error, "cancelled transfer abort failed");
            self.defer_cleanup(attempt, "abort_failed", now).await;
            return;
        }
        match source.discard(&handle).await {
            Ok(_) => {
                let attempt_id = attempt.id.clone();
                let revision = attempt.row_revision;
                let disposition = attempt.disposition.clone();
                self.step("downloads.cleanup_done", move |store| {
                    store.transition_attempt(
                        &attempt_id,
                        revision,
                        AttemptState::Complete,
                        now,
                        Some(&disposition),
                        None,
                        true,
                    )
                })
                .await;
            }
            Err(error) => {
                tracing::warn!(attempt_id = %attempt.id, %error, "cleanup discard failed");
                self.defer_cleanup(attempt, "discard_failed", now).await;
            }
        }
    }

    /// Push one cleanup row out with backoff.
    async fn defer_cleanup(&self, attempt: &AttemptRow, code: &'static str, now: f64) {
        let attempt_id = attempt.id.clone();
        let revision = attempt.row_revision;
        self.step("downloads.cleanup_defer", move |store| {
            store.record_cleanup_failure(&attempt_id, revision, code, now)
        })
        .await;
    }

    /// Hand kept files to cleanup once they are 90 days old or their task
    /// was cancelled or cleared. A reimport that started first keeps them:
    /// its task is processing, which this move skips.
    pub(super) async fn release_kept(&self, pass: &Pass) {
        let now = pass.now;
        let Some(released) = self
            .step("downloads.release_kept", move |store| {
                store.release_kept(now, now - KEPT_FILES_SECONDS)
            })
            .await
        else {
            return;
        };
        for (attempt_id, task_id) in released {
            tracing::info!(
                %task_id,
                %attempt_id,
                "files kept for a retry are no longer kept; they go to cleanup"
            );
        }
    }
}
