//! Verification: the job that confirms a MusicBrainz result.
//!
//! Ports v2 `LibraryContributionVerificationWorker`. A verification is one
//! [`VERIFY`] job on the jobs table, keyed by its contribution; the jobs
//! module claims, leases and recovers it, and runs [`VerificationWorker`]
//! as its handler. The handler re-reads the release from MusicBrainz on
//! the [`RequestPriority::BackgroundSync`] lane (never the user lane),
//! runs the identify-owned evidence decision, and writes the contribution
//! as linked or needs-review in the same write that completes the job. A
//! release MusicBrainz does not have yet is retried with backoff for a
//! while, then sent to review.
//!
//! The `library-contrib` loop does the hourly upkeep (expired provider
//! data, old records). Its first pass after a start also queues a job for
//! any verifying contribution left without one.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_kernel::jobs::{
    AlreadyRunning, BoxFuture, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};
use droppedneedle_providers::RequestPriority;
use rusqlite::Transaction;

use super::error::ContribError;
use super::models::*;
use super::rules::verification_retry_delay_seconds;
use super::seams::*;
use super::service::ContributionService;
use super::sqlite::SqliteContributions;
use super::transition::Finish;

pub const MAX_AUTOMATIC_WINDOW_SECONDS: f64 = 2.0 * 60.0 * 60.0;
pub const MAX_AUTOMATIC_ATTEMPTS: u32 = 10;
pub const CLEANUP_INTERVAL_SECONDS: f64 = 60.0 * 60.0;

/// The verification job kind; its key is the contribution id.
pub const VERIFY: JobKind = JobKind::new("library.contribution_verify");

/// How verification jobs run. The handler sends a job to review after
/// [`MAX_AUTOMATIC_ATTEMPTS`]; the module's own limit sits above it, so it
/// only ends a job that keeps timing out or crashing.
pub const SPEC: KindSpec = KindSpec::new(VERIFY).max_attempts(MAX_AUTOMATIC_ATTEMPTS + 2);

/// Finishes the claimed job inside the caller's write; `LeaseLost` rolls
/// that write back.
pub type Completion = Arc<dyn Fn(&Transaction) -> Result<(), JobError> + Send + Sync>;

/// One claimed verification.
#[derive(Clone)]
pub struct Claim {
    /// The contribution, the job's key.
    pub contribution_id: String,
    /// Which claim this is, from 1.
    pub attempt: u32,
    /// Completes the job in the write that records the outcome.
    pub complete: Completion,
}

/// What one verification run decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// The contribution's outcome.
    pub outcome: VerificationOutcome,
    /// The job's answer.
    pub job: Outcome,
}

impl Verdict {
    fn done(outcome: VerificationOutcome) -> Self {
        Self {
            outcome,
            job: Outcome::Done,
        }
    }
}

pub struct VerificationWorker {
    service: Arc<ContributionService>,
    musicbrainz: Arc<dyn MusicBrainzContrib>,
    store: Arc<SqliteContributions>,
}

impl VerificationWorker {
    pub fn new(
        service: Arc<ContributionService>,
        musicbrainz: Arc<dyn MusicBrainzContrib>,
    ) -> Self {
        let store = service.store().clone();
        Self {
            service,
            musicbrainz,
            store,
        }
    }

    /// The handler the jobs module runs for [`VERIFY`].
    pub fn handler(self: &Arc<Self>) -> Arc<dyn JobHandler> {
        Arc::new(VerificationHandler(Arc::clone(self)))
    }

    /// Queue a job for every verifying contribution without one: one a v2
    /// import carried, or one whose job failed for good. False, logged,
    /// when the write failed.
    pub async fn queue_missing(&self) -> bool {
        match self.store.queue_missing_verifications().await {
            Ok(0) => true,
            Ok(queued) => {
                tracing::info!(queued, "verifying contributions queued again");
                true
            }
            Err(error) => {
                tracing::warn!(%error, "verifying contributions not queued");
                false
            }
        }
    }

    /// Drop expired provider data and old records. Each step that fails is
    /// logged, and the next pass tries again.
    pub async fn upkeep(&self, now: f64) {
        if let Err(error) = self.service.purge_expired_provider_data(now, 200).await {
            tracing::warn!(%error, "expired contribution provider data not purged");
        }
        if let Err(error) = self.store.clean_records(now).await {
            tracing::warn!(%error, "old contribution records not cleaned");
        }
    }

    /// Run one claimed job and turn its result into the job's answer. A
    /// busy database puts the job back without spending the attempt; any
    /// other error retries it after the kind's backoff.
    async fn answer(&self, job: JobCtx) -> Outcome {
        let ctx = job.clone();
        let claim = Claim {
            contribution_id: job.key().to_owned(),
            attempt: job.attempt(),
            complete: Arc::new(move |tx| ctx.complete(tx)),
        };
        match self.run_claimed(&claim, self.service.now()).await {
            Ok(verdict) => verdict.job,
            Err(ContribError::Busy(_)) => Outcome::Busy,
            Err(error) => {
                tracing::warn!(
                    code = error.code(),
                    %error,
                    attempt = claim.attempt,
                    "contribution verification failed; it runs again later"
                );
                Outcome::Retry {
                    code: error.code(),
                    at: None,
                }
            }
        }
    }

    /// Claim the next due job and run it, as the runner would, for tests.
    /// The claim's lease ends at once, so a job a retry left running is
    /// taken back by the next call once the clock reaches its retry time:
    /// the runner's put-back is the jobs module's, tested there.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn run_once(&self) -> Result<Option<Verdict>, ContribError> {
        let queue = self.store.queue().clone();
        let spec = SPEC.lease(Duration::from_millis(1));
        let claimed = {
            let queue = queue.clone();
            self.store
                .db()
                .write(Lane::Background, "contrib.test_claim", move |tx| {
                    queue.claim_in(tx, &spec)
                })
                .await?
        };
        let Some(claimed) = claimed else {
            return Ok(None);
        };
        let lease = claimed.lease.clone();
        let complete: Completion = {
            let queue = queue.clone();
            let lease = lease.clone();
            Arc::new(move |tx| queue.complete(tx, &lease))
        };
        let claim = Claim {
            contribution_id: claimed.key,
            attempt: claimed.attempt,
            complete: complete.clone(),
        };
        let verdict = self.run_claimed(&claim, self.service.now()).await?;
        if verdict.job == Outcome::Done {
            self.store
                .db()
                .write(Lane::Background, "contrib.test_done", move |tx| {
                    complete(tx).map_err(ContribError::from)
                })
                .await?;
        }
        Ok(Some(verdict))
    }

    pub async fn run_claimed(&self, job: &Claim, now: f64) -> Result<Verdict, ContribError> {
        let contribution = match self
            .service
            .get_as(&job.contribution_id, Lane::Background)
            .await
        {
            Ok(contribution) => contribution,
            Err(ContribError::ContributionNotFound | ContribError::AlbumNotFound) => {
                return Ok(Verdict::done(VerificationOutcome::SubjectMissing));
            }
            Err(other) => return Err(other),
        };
        if contribution.state != ContributionState::Verifying
            || contribution.result_release_mbid.is_none()
        {
            return Ok(Verdict::done(VerificationOutcome::NoLongerVerifying));
        }
        let release_mbid = contribution.result_release_mbid.clone().unwrap_or_default();
        // Background priority: verification never jumps the user queue.
        let verified = match self
            .musicbrainz
            .get_release_for_verification(&release_mbid, RequestPriority::BackgroundSync, true)
            .await
        {
            Ok(verified) => verified,
            // Deterministic payload-shape failure: review immediately, no
            // retry, breaker untouched (as v2 decided).
            Err(ProviderFailure::Unmappable) => {
                return self
                    .finish_without_candidate(job, &contribution, UNMAPPABLE_PROVIDER_PAYLOAD, now)
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
                    .retry_or_review(job, &contribution, FAILURE_MB_UNAVAILABLE, now, retry_after)
                    .await;
            }
        };
        let Some(verified) = verified else {
            return self
                .retry_or_review(job, &contribution, FAILURE_MB_NOT_PROPAGATED, now, None)
                .await;
        };
        if verified.release_mbid != release_mbid {
            return self
                .finish_without_candidate(
                    job,
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
            contribution.updated_by_user_id.as_deref(),
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
            tracks: if identified {
                decision.tracks.clone()
            } else {
                Vec::new()
            },
        };
        let expected_album_revision = context.album.as_ref().map(|a| a.row_revision).unwrap_or(0);
        let finish = Finish {
            expected_row_revision: contribution.row_revision,
            expected_album_revision,
            attempt,
            outcome,
            failure_code,
            identities,
            policy_revision: self.store.input_revisions(&context.tracks).2,
        };
        let (result, invalidation) = self
            .store
            .finish_verification(&job.contribution_id, job.complete.clone(), finish, now)
            .await?;
        if let Some(invalidation) = &invalidation {
            self.service
                .after_linked(&contribution, invalidation, now)
                .await?;
        }
        Ok(Verdict::done(result))
    }

    async fn retry_or_review(
        &self,
        job: &Claim,
        contribution: &ContributionRecord,
        failure_code: &'static str,
        now: f64,
        retry_after_seconds: Option<f64>,
    ) -> Result<Verdict, ContribError> {
        let received_at = contribution.result_received_at.unwrap_or(now);
        if job.attempt < MAX_AUTOMATIC_ATTEMPTS && now - received_at < MAX_AUTOMATIC_WINDOW_SECONDS
        {
            let delay = verification_retry_delay_seconds(job.attempt, retry_after_seconds);
            let at = ((now + delay) * 1000.0).round() as i64;
            return Ok(Verdict {
                outcome: VerificationOutcome::RetryScheduled,
                job: Outcome::Retry {
                    code: failure_code,
                    at: Some(at),
                },
            });
        }
        self.finish_without_candidate(job, contribution, failure_code, now)
            .await
    }

    async fn finish_without_candidate(
        &self,
        job: &Claim,
        contribution: &ContributionRecord,
        failure_code: &str,
        now: f64,
    ) -> Result<Verdict, ContribError> {
        let context = match self
            .store
            .album_context(&contribution.local_album_id)
            .await?
        {
            Some(context) if context.album.is_some() => context,
            _ => return Ok(Verdict::done(VerificationOutcome::SubjectMissing)),
        };
        let attempt = ContributionVerificationAttempt {
            id: uuid::Uuid::new_v4().to_string(),
            local_album_id: contribution.local_album_id.clone(),
            requested_by_user_id: contribution.updated_by_user_id.clone(),
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
        let finish = Finish {
            expected_row_revision: contribution.row_revision,
            expected_album_revision,
            attempt,
            outcome: VerificationOutcome::NeedsReview,
            failure_code: Some(failure_code.to_owned()),
            identities: FinishIdentities::default(),
            policy_revision: self.store.input_revisions(&context.tracks).2,
        };
        let (result, _) = self
            .store
            .finish_verification(&job.contribution_id, job.complete.clone(), finish, now)
            .await?;
        Ok(Verdict::done(result))
    }
}

/// [`VerificationWorker`] as the jobs module runs it.
struct VerificationHandler(Arc<VerificationWorker>);

impl JobHandler for VerificationHandler {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let worker = Arc::clone(&self.0);
        Box::pin(async move { worker.answer(job).await })
    }
}

/// The contribution upkeep loop's name on the supervisor.
pub const UPKEEP_JOB: &str = "library-contrib";

/// Start the upkeep loop on `supervisor`: a pass at start, then hourly.
/// Missing verification jobs are queued on the first pass that can write
/// them, so a job that failed for good is retried once per start.
pub fn start_upkeep(
    supervisor: &Supervisor,
    worker: Arc<VerificationWorker>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        UPKEEP_JOB,
        Schedule::new(Duration::from_secs_f64(CLEANUP_INTERVAL_SECONDS)),
        UpkeepTick {
            worker,
            queued: false,
        },
    )
}

struct UpkeepTick {
    worker: Arc<VerificationWorker>,
    queued: bool,
}

impl Tick for UpkeepTick {
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        if !self.queued {
            self.queued = self.worker.queue_missing().await;
        }
        let now = self.worker.service.now();
        self.worker.upkeep(now).await;
        Ok(Next::Continue)
    }
}
