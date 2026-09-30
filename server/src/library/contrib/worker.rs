//! Verification worker: background loop that confirms MusicBrainz results.
//!
//! Ports v2 `LibraryContributionVerificationWorker`. The worker claims queued
//! jobs under a 90s lease, re-reads the release from MusicBrainz on the
//! [`RequestPriority::BackgroundSync`] lane (never the user lane), runs the
//! sibling-owned evidence decision, and finishes the job as linked or
//! needs-review. Shutdown is a `watch` flag; `main.rs` wiring is owned by the
//! stage-8 integrator - this module only exposes [`spawn_verification_worker`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle::providers::slots::RequestPriority;

use super::error::ContribError;
use super::models::*;
use super::rules::verification_retry_delay_seconds;
use super::seams::*;
use super::service::ContributionService;

pub const LEASE_SECONDS: f64 = 90.0;
pub const MAX_AUTOMATIC_WINDOW_SECONDS: f64 = 2.0 * 60.0 * 60.0;
pub const MAX_AUTOMATIC_ATTEMPTS: u32 = 10;
pub const CLEANUP_INTERVAL_SECONDS: f64 = 60.0 * 60.0;

#[derive(Debug, Clone)]
pub struct VerificationWorkerConfig {
    pub worker_id: String,
    pub poll_interval: Duration,
    pub lease_seconds: f64,
}

impl Default for VerificationWorkerConfig {
    fn default() -> Self {
        Self {
            worker_id: "contrib-verify-1".to_string(),
            poll_interval: Duration::from_secs(5),
            lease_seconds: LEASE_SECONDS,
        }
    }
}

pub struct VerificationWorker {
    service: Arc<ContributionService>,
    musicbrainz: Arc<dyn MusicBrainzContrib>,
    identity: Arc<dyn ContributionIdentity>,
    store: Arc<dyn ContributionStore>,
    config: VerificationWorkerConfig,
    next_cleanup_at: Mutex<f64>,
}

impl VerificationWorker {
    pub fn new(
        service: Arc<ContributionService>,
        musicbrainz: Arc<dyn MusicBrainzContrib>,
        identity: Arc<dyn ContributionIdentity>,
        config: VerificationWorkerConfig,
    ) -> Self {
        let store = service.store().clone();
        Self {
            service,
            musicbrainz,
            identity,
            store,
            config,
            next_cleanup_at: Mutex::new(0.0),
        }
    }

    pub async fn recover(&self, now: f64) -> u64 {
        let recovered = self.store.recover_verification_leases(now).await;
        let due = self
            .next_cleanup_at
            .lock()
            .map(|g| now >= *g)
            .unwrap_or(true);
        if due {
            let _ = self.service.purge_expired_provider_data(now, 200).await;
            self.store.clean_records(now).await;
            if let Ok(mut guard) = self.next_cleanup_at.lock() {
                *guard = now + CLEANUP_INTERVAL_SECONDS;
            }
        }
        recovered
    }

    pub async fn claim(&self, now: f64) -> Option<VerificationJobRow> {
        self.store
            .claim_verification(&self.config.worker_id, now, self.config.lease_seconds)
            .await
    }

    pub async fn run_once(&self, now: f64) -> Result<Option<VerificationOutcome>, ContribError> {
        let job = match self.claim(now).await {
            Some(job) => job,
            None => return Ok(None),
        };
        Ok(Some(self.run_claimed(&job, now).await?))
    }

    pub async fn run_claimed(
        &self,
        job: &VerificationJobRow,
        now: f64,
    ) -> Result<VerificationOutcome, ContribError> {
        let job_revision = self
            .store
            .heartbeat_verification(
                &job.id,
                &self.config.worker_id,
                job.row_revision,
                now,
                self.config.lease_seconds,
            )
            .await?;
        let contribution = match self.service.get(&job.contribution_id).await {
            Ok(contribution) => contribution,
            Err(ContribError::ContributionNotFound | ContribError::AlbumNotFound) => {
                return Ok(VerificationOutcome::SubjectMissing);
            }
            Err(other) => return Err(other),
        };
        if contribution.state != ContributionState::Verifying
            || contribution.result_release_mbid.is_none()
        {
            return Ok(VerificationOutcome::NoLongerVerifying);
        }
        let release_mbid = contribution.result_release_mbid.clone().unwrap_or_default();
        // Honest background priority: verification never jumps the user queue.
        let verified = match self
            .musicbrainz
            .get_release_for_verification(&release_mbid, RequestPriority::BackgroundSync, true)
            .await
        {
            Ok(verified) => verified,
            // Deterministic payload-shape failure: review immediately, no
            // retry, breaker untouched (v2 owner option A, 2026-08-20).
            Err(ProviderFailure::Unmappable) => {
                return self
                    .finish_without_candidate(
                        job,
                        job_revision,
                        &contribution,
                        UNMAPPABLE_PROVIDER_PAYLOAD,
                        now,
                    )
                    .await;
            }
            Err(ProviderFailure::Unavailable {
                retry_after_seconds,
            }) => {
                // Quirk (v2): a bogus retry-after (NaN, infinite,
                // non-positive) is dropped, not clamped.
                let retry_after = retry_after_seconds
                    .filter(|candidate| candidate.is_finite() && *candidate > 0.0);
                return self
                    .retry_or_review(
                        job,
                        job_revision,
                        &contribution,
                        FAILURE_MB_UNAVAILABLE,
                        now,
                        retry_after,
                    )
                    .await;
            }
        };
        let Some(verified) = verified else {
            return self
                .retry_or_review(
                    job,
                    job_revision,
                    &contribution,
                    FAILURE_MB_NOT_PROPAGATED,
                    now,
                    None,
                )
                .await;
        };
        if verified.release_mbid != release_mbid {
            return self
                .finish_without_candidate(
                    job,
                    job_revision,
                    &contribution,
                    FAILURE_RETURNED_RELEASE_MISMATCH,
                    now,
                )
                .await;
        }
        let (decision, context) = self
            .service
            .build_attachment_evidence(&contribution, &verified)
            .await?;
        let identified = decision.outcome == AttachmentOutcome::Identified;
        let outcome = if identified {
            VerificationOutcome::Linked
        } else {
            VerificationOutcome::NeedsReview
        };
        let failure_code = (!identified).then(|| {
            decision
                .reason_code
                .clone()
                .unwrap_or_else(|| "ATTACHMENT_CONTRADICTION".to_string())
        });
        let attempt = self.service.verification_attempt(
            &contribution,
            &decision,
            job.requested_by_user_id.as_deref(),
            &context.tracks,
            if identified {
                "identified"
            } else {
                "needs_review"
            },
            failure_code.clone(),
            now,
        );
        let selected = decision
            .candidates
            .iter()
            .find(|c| Some(c.key()) == decision.selected_candidate_key);
        let identities = FinishIdentities {
            release_mbid: selected.and_then(|c| c.release_mbid.clone()),
            release_group_mbid: selected.map(|c| c.release_group_mbid.clone()),
            artist_mbid: selected
                .and_then(|c| c.artist_mbid.clone())
                .or_else(|| verified.artist_mbid.clone()),
        };
        let expected_album_revision = context.album.as_ref().map(|a| a.row_revision).unwrap_or(0);
        let result = self
            .store
            .finish_verification(
                &job.id,
                &self.config.worker_id,
                job_revision,
                contribution.row_revision,
                expected_album_revision,
                &attempt,
                outcome,
                failure_code.as_deref(),
                &identities,
                now,
            )
            .await?;
        if result == VerificationOutcome::Linked {
            let (_, _, policy) = self.identity.input_revisions(&context.tracks);
            self.service
                .after_linked(&contribution, &policy, now)
                .await?;
        }
        Ok(result)
    }

    async fn retry_or_review(
        &self,
        job: &VerificationJobRow,
        job_revision: i64,
        contribution: &ContributionRecord,
        failure_code: &str,
        now: f64,
        retry_after_seconds: Option<f64>,
    ) -> Result<VerificationOutcome, ContribError> {
        let received_at = contribution.result_received_at.unwrap_or(now);
        if job.attempt_count < MAX_AUTOMATIC_ATTEMPTS
            && now - received_at < MAX_AUTOMATIC_WINDOW_SECONDS
        {
            let delay = verification_retry_delay_seconds(job.attempt_count, retry_after_seconds);
            self.store
                .retry_verification(
                    &job.id,
                    &self.config.worker_id,
                    job_revision,
                    failure_code,
                    now + delay,
                    now,
                )
                .await?;
            return Ok(VerificationOutcome::RetryScheduled);
        }
        self.finish_without_candidate(job, job_revision, contribution, failure_code, now)
            .await
    }

    async fn finish_without_candidate(
        &self,
        job: &VerificationJobRow,
        job_revision: i64,
        contribution: &ContributionRecord,
        failure_code: &str,
        now: f64,
    ) -> Result<VerificationOutcome, ContribError> {
        let context = match self
            .identity
            .album_context(&contribution.local_album_id)
            .await
        {
            Some(context) if context.album.is_some() => context,
            _ => return Ok(VerificationOutcome::SubjectMissing),
        };
        let attempt = ContributionVerificationAttempt {
            id: uuid::Uuid::new_v4().to_string(),
            local_album_id: contribution.local_album_id.clone(),
            requested_by_user_id: job.requested_by_user_id.clone(),
            matcher_version: self.service.evidence().matcher_version(),
            state: "needs_review".to_string(),
            terminal_reason_code: Some(failure_code.to_string()),
            selected_candidate_key: None,
            candidate_count: 0,
            candidate_keys: Vec::new(),
            started_at: now,
            completed_at: now,
        };
        let expected_album_revision = context.album.as_ref().map(|a| a.row_revision).unwrap_or(0);
        self.store
            .finish_verification(
                &job.id,
                &self.config.worker_id,
                job_revision,
                contribution.row_revision,
                expected_album_revision,
                &attempt,
                VerificationOutcome::NeedsReview,
                Some(failure_code),
                &FinishIdentities::default(),
                now,
            )
            .await
    }
}

/// Shutdown flag channel: send `true` (or drop the sender) to stop the loop.
pub fn shutdown_channel() -> (
    tokio::sync::watch::Sender<bool>,
    tokio::sync::watch::Receiver<bool>,
) {
    tokio::sync::watch::channel(false)
}

/// Spawn the background verification loop. Each tick recovers expired leases
/// and drains every due job; any single-job error is logged and the loop
/// moves on. Returns the join handle; the integrator owns supervision and
/// must NOT wire this into `main.rs` from this slice.
pub fn spawn_verification_worker(
    worker: Arc<VerificationWorker>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let poll_interval = worker.config.poll_interval;
    tokio::spawn(async move {
        let mut shutting_down = *shutdown.borrow_and_update();
        while !shutting_down {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    match changed {
                        Ok(()) => shutting_down = *shutdown.borrow_and_update(),
                        // Sender dropped: treat as shutdown (v2 executor exit).
                        Err(_) => shutting_down = true,
                    }
                }
                () = tokio::time::sleep(poll_interval) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);
                    worker.recover(now).await;
                    loop {
                        match worker.run_once(now).await {
                            Ok(Some(_)) => {}
                            Ok(None) => break,
                            Err(error) => {
                                tracing::warn!(
                                    error = %error,
                                    "contribution verification job failed; continuing"
                                );
                                break;
                            }
                        }
                    }
                }
            }
        }
    })
}
