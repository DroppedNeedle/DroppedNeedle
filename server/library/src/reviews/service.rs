//! The operations service's review calls and review jobs.

use crate::clock::LibraryClock as _;
use crate::identify::models::{AlbumIdentity, ReviewState};
use crate::jobs::OperationJob;
use crate::jobs::OperationState;
use crate::jobs::WorkState;
use crate::jobs::models::Control;
use crate::jobs::models::OperationError;
use crate::jobs::service::Operations;
use crate::jobs::{self, OperationKind};
use crate::preview::{self, TokenFault};
use crate::reasons;
use crate::reidentify::store;
use crate::reviews;
use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::db::{Lane, savepoint};

/// Most reviews one "retry all" or bulk action takes on.
pub const BULK_MAX: u32 = 5_000;

/// Reviews one write of a retry preview tries, so each write stays well
/// inside a request's write budget.
const PREVIEW_CHUNK: usize = 200;

/// How long a finished review job stays in the review page's list.
const REVIEW_JOB_SHOWN_SECS: f64 = 300.0;

impl Operations {
    /// One page of identification reviews. Blocking.
    pub fn reviews(
        &self,
        filter: &reviews::ReviewFilter,
    ) -> Result<reviews::ReviewPage, OperationError> {
        self.db.read_blocking("operations.reviews", |conn| {
            Ok::<_, OperationError>(reviews::list(conn, filter)?)
        })
    }

    /// One identification review. Blocking.
    pub fn review(&self, review_id: &str) -> Result<reviews::ReviewRow, OperationError> {
        self.db
            .read_blocking("operations.review", |conn| {
                Ok::<_, OperationError>(reviews::get(conn, review_id)?)
            })?
            .ok_or(OperationError::NotFound(reasons::REVIEW_NOT_FOUND))
    }

    /// Keep as tagged, dismiss, detach, exclude or restore from a review
    /// decided on `expected_revision`, then return the settled review. A
    /// review that changed since is refused with `STALE_REVISION`.
    /// Blocking.
    pub fn settle_review(
        &self,
        review_id: &str,
        action: reviews::ReviewAction,
        expected_revision: i64,
        actor: &str,
    ) -> Result<reviews::ReviewRow, OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.settle_review", |tx| {
                reviews::settle(
                    tx,
                    review_id,
                    action,
                    expected_revision,
                    actor,
                    self.clock.now_secs(),
                )
            })?;
        self.review(review_id)
    }

    /// Approve a pending review decided on `expected_revision` with one of
    /// its releases: the release becomes the album's match as the
    /// person's choice, and the review settles with it. Returns the
    /// settled review and the album's identity. Blocking.
    pub fn approve_review(
        &self,
        review_id: &str,
        expected_revision: i64,
        candidate_key: &str,
        actor: &str,
    ) -> Result<(reviews::ReviewRow, Option<AlbumIdentity>), OperationError> {
        let row = self.review(review_id)?;
        if row.row_revision != expected_revision {
            return Err(OperationError::Conflict(reasons::REVIEW_STALE));
        }
        if row.review.state != ReviewState::Pending {
            return Err(OperationError::Conflict(reasons::REVIEW_SETTLED));
        }
        if !row
            .review
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_key == candidate_key)
        {
            return Err(OperationError::NotFound(
                reasons::REVIEW_CANDIDATE_NOT_FOUND,
            ));
        }
        // The approval's own write settles the review only while it is
        // still at this revision, so a change since this read is refused
        // there.
        if !self.identify.approve_candidate_at(
            review_id,
            actor,
            candidate_key,
            Some(expected_revision),
        )? {
            return Err(OperationError::Conflict(reasons::REVIEW_STALE));
        }
        let settled = self.review(review_id)?;
        let identity = self.store.album_identity(&settled.review.local_album_id)?;
        Ok((settled, identity))
    }

    /// Identify a pending review's album again, decided on
    /// `expected_revision`. The job hands the album to identification,
    /// which settles it by its usual rules, so the review is dismissed in
    /// the same write. A review someone changed meanwhile, or an album
    /// that may not be retried, is refused and the review stays open. A
    /// request sent again with the same idempotency key answers the job
    /// the first one started, before the review's revision is checked.
    /// Blocking.
    pub fn retry_review(
        &self,
        review_id: &str,
        expected_revision: i64,
        actor: &str,
        idempotency_key: Option<String>,
        one_off_local_metadata: bool,
    ) -> Result<OperationJob, OperationError> {
        let row = self.review(review_id)?;
        let new = self.plain_reidentification(
            &row.review.local_album_id,
            actor,
            idempotency_key,
            one_off_local_metadata,
        );
        let job = self
            .db
            .write_blocking(Lane::Foreground, "operations.retry_review", |tx| {
                if let Some(job) = store::replayed_reidentification(tx, &new)? {
                    return Ok(job);
                }
                reviews::guard(
                    tx,
                    review_id,
                    expected_revision,
                    ReviewState::Pending,
                    reasons::REVIEW_SETTLED,
                )?;
                if !reviews::mark(
                    tx,
                    review_id,
                    "pending",
                    ReviewState::Dismissed,
                    actor,
                    self.clock.now_secs(),
                )? {
                    return Err(OperationError::Conflict(reasons::REVIEW_SETTLED));
                }
                store::create_reidentification(tx, &self.management, &new, self.clock.now_secs())
            })?;
        jobs::wake(&self.wakeups, OperationKind::ExplicitReidentification);
        Ok(job)
    }

    /// The folders a set of library folder and sub-folder rule ids names,
    /// picked the way a scan picks them: a rule inside a picked folder or
    /// rule adds nothing. An id the saved settings do not hold is refused.
    /// Blocking.
    pub fn folder_scopes(
        &self,
        scope_ids: &[String],
    ) -> Result<Vec<reviews::FolderScope>, OperationError> {
        let settings = self
            .signer
            .library
            .get()
            .map_err(|error| OperationError::Store(error.to_string()))?;
        let registry = crate::settings::registry_from(&settings);
        let scopes = crate::scan::selection::select_scopes(&registry, scope_ids)
            .map_err(|_| OperationError::Invalid(reasons::REVIEW_FILTER_INVALID))?;
        Ok(scopes
            .into_iter()
            .map(|scope| reviews::FolderScope {
                root_id: scope.root_id,
                relative_path: scope.relative_path,
            })
            .collect())
    }

    /// The pending reviews `filter` selects, up to [`BULK_MAX`], as the
    /// work of a retry.
    fn pending_retries(
        &self,
        one_off_local_metadata: bool,
        filter: &reviews::ReviewFilter,
    ) -> Result<Vec<reviews::ReviewWork>, OperationError> {
        let filter = reviews::ReviewFilter {
            states: vec![ReviewState::Pending],
            limit: BULK_MAX,
            offset: 0,
            ..filter.clone()
        };
        Ok(self
            .reviews(&filter)?
            .rows
            .into_iter()
            .map(|row| reviews::ReviewWork {
                review_id: row.review.id,
                album_id: row.review.local_album_id,
                revision: row.row_revision,
                candidate_key: None,
                confirm_local_metadata: one_off_local_metadata,
            })
            .collect())
    }

    /// Count what a retry of every pending review `filter` selects would
    /// start, and why the rest would not: each retry is tried and rolled
    /// back, so the counts are exactly what a run would do now. Blocking.
    pub fn preview_retry_pending(
        &self,
        actor: &str,
        one_off_local_metadata: bool,
        filter: &reviews::ReviewFilter,
    ) -> Result<reviews::Tally, OperationError> {
        let work = self.pending_retries(one_off_local_metadata, filter)?;
        let mut outcome = reviews::Tally::default();
        // Nothing commits, so a write per chunk keeps each one short.
        for chunk in work.chunks(PREVIEW_CHUNK) {
            let part = self.db.write_blocking(
                Lane::Foreground,
                "operations.preview_retry_pending",
                |tx| {
                    let now = self.clock.now_secs();
                    let mut outcome = reviews::Tally::default();
                    for item in chunk {
                        // Always rolled back.
                        let tried = savepoint(tx, "preview", || {
                            Err::<(), _>(reviews::retry(tx, &self.management, item, actor, now))
                        })?;
                        match tried.err().unwrap_or(Ok(())) {
                            Ok(()) => outcome.done += 1,
                            Err(
                                OperationError::NotFound(reason)
                                | OperationError::Invalid(reason)
                                | OperationError::Conflict(reason),
                            ) => outcome.skip(reason),
                            Err(other) => return Err(other),
                        }
                    }
                    Ok(outcome)
                },
            )?;
            outcome.merge(part);
        }
        Ok(outcome)
    }

    /// Start identification again for every pending review `filter`
    /// selects, up to [`BULK_MAX`], as one review job. Each review is
    /// checked as the job reaches it: albums that cannot start (excluded,
    /// no files, Local metadata without the confirmation or under
    /// management, an edition someone chose) keep their review and are
    /// counted as skipped. `None` when nothing is pending. Blocking.
    pub fn retry_pending_reviews(
        &self,
        actor: &str,
        one_off_local_metadata: bool,
        filter: &reviews::ReviewFilter,
    ) -> Result<Option<OperationJob>, OperationError> {
        let work = self.pending_retries(one_off_local_metadata, filter)?;
        self.queue_review_job(reviews::ReviewJobAction::Retry, actor, &work)
    }

    /// Queue a review job over `work`; `None` when there is none.
    fn queue_review_job(
        &self,
        action: reviews::ReviewJobAction,
        actor: &str,
        work: &[reviews::ReviewWork],
    ) -> Result<Option<OperationJob>, OperationError> {
        if work.is_empty() {
            return Ok(None);
        }
        let job_id = self.ids.new_id();
        let job =
            self.db
                .write_blocking(Lane::Foreground, "operations.queue_review_job", |tx| {
                    reviews::queue_job(tx, &job_id, action, actor, work, self.clock.now_secs())
                })?;
        jobs::wake(&self.wakeups, OperationKind::BulkReviewApply);
        Ok(Some(job))
    }

    /// What a bulk action would do to the reviews `filter` selects.
    fn plan_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
    ) -> Result<reviews::BulkPlan, OperationError> {
        let filter = reviews::ReviewFilter {
            limit: BULK_MAX,
            offset: 0,
            ..filter.clone()
        };
        let page = self.reviews(&filter)?;
        Ok(reviews::plan_bulk(
            &page.rows,
            page.total,
            action,
            candidate_key,
        ))
    }

    /// What a bulk action would do to the reviews `filter` selects: the
    /// reviews it changes, the ones it skips by reason, and the token
    /// `actor` applies it with. Blocking.
    pub fn preview_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
        actor: &str,
    ) -> Result<(reviews::BulkPlan, String), OperationError> {
        let plan = self.plan_bulk_reviews(action, filter, candidate_key)?;
        let token = preview::issue(
            &self.signer,
            preview::REVIEW_BULK,
            actor,
            &plan.material,
            self.clock.now_secs() as i64,
        );
        Ok((plan, token))
    }

    /// Apply a bulk action `actor` previewed, as one review job. Refused
    /// when the token is not theirs, is too old, or the selection changed
    /// since the preview. The job checks each review again at its
    /// previewed revision as it reaches it. Returns the job (`None` when
    /// the preview left nothing to change) and the reviews the preview
    /// skipped. Blocking.
    pub fn apply_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
        preview_token: &str,
        actor: &str,
    ) -> Result<(Option<OperationJob>, reviews::Skips), OperationError> {
        let plan = self.plan_bulk_reviews(action, filter, candidate_key)?;
        preview::verify(
            &self.signer,
            preview::REVIEW_BULK,
            actor,
            preview_token,
            &plan.material,
            self.clock.now_secs() as i64,
        )
        .map_err(|fault| {
            let reason = fault.reason(reasons::REVIEW_SELECTION_CHANGED);
            match fault {
                TokenFault::Invalid => OperationError::Invalid(reason),
                TokenFault::Expired | TokenFault::Stale => OperationError::Conflict(reason),
            }
        })?;
        let work: Vec<reviews::ReviewWork> = plan
            .items
            .into_iter()
            .map(|item| reviews::ReviewWork {
                review_id: item.review_id,
                album_id: item.album_id,
                revision: item.revision,
                candidate_key: item.candidate_key,
                confirm_local_metadata: false,
            })
            .collect();
        let job = self.queue_review_job(action.into(), actor, &work)?;
        Ok((job, plan.skipped))
    }

    /// The review jobs still to finish and those that ended in the last
    /// few minutes, newest first, each with its action and its skips by
    /// reason. Blocking.
    pub fn review_jobs(&self) -> Result<Vec<ReviewJob>, OperationError> {
        let since = self.clock.now_secs() - REVIEW_JOB_SHOWN_SECS;
        self.db.read_blocking("operations.review_jobs", |conn| {
            jobs::recent(conn, OperationKind::BulkReviewApply, since, 5)?
                .into_iter()
                .map(|job| {
                    Ok::<_, OperationError>(ReviewJob {
                        action: reviews::job_action(conn, &job.id)?,
                        skipped: reviews::job_skips(conn, &job.id)?,
                        job,
                    })
                })
                .collect()
        })
    }

    /// Pause, resume or stop review job `job_id` as it is now: a review
    /// job's counts move with every review, so a revision read a moment
    /// ago is already old. A stopped job stays stopped: its reviews that
    /// were not reached are still pending and can be changed again.
    /// Blocking.
    pub fn control_review_job(
        &self,
        job_id: &str,
        control: Control,
    ) -> Result<OperationJob, OperationError> {
        let job =
            self.db
                .write_blocking(Lane::Foreground, "operations.control_review_job", |tx| {
                    let current = jobs::job(tx, job_id)?
                        .filter(|job| job.kind == OperationKind::BulkReviewApply.as_str())
                        .ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
                    if control == Control::Resume && current.state == OperationState::Stopped {
                        return Err(OperationError::Conflict(reasons::REVIEW_JOB_STOPPED));
                    }
                    crate::jobs::store::request_control(
                        tx,
                        job_id,
                        control,
                        current.row_revision,
                        None,
                        self.clock.now_secs(),
                    )
                })?;
        jobs::wake(&self.wakeups, OperationKind::BulkReviewApply);
        Ok(job)
    }

    /// Hand the review job `owner` holds back to the queue, where it waits
    /// while a scan runs. Blocking.
    pub fn release_review_job(&self, owner: &str) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Background, "operations.release_review_job", |tx| {
                if let Some(job) = jobs::held(tx, OperationKind::BulkReviewApply, owner)? {
                    jobs::release(tx, &job.id, owner, self.clock.now_secs())?;
                }
                Ok::<_, OperationError>(())
            })
    }

    /// One review of the review job `owner` holds, or of the next queued
    /// one it claims. A pause or stop asked for since the last review is
    /// honoured first, so a job halts between reviews. Each review is
    /// settled under the rules and refusals of its own route, at the
    /// revision it was picked at; a refused one is skipped with its
    /// reason. When recording a review fails, the job goes back to the
    /// queue with that review pending, so it is done again. Blocking.
    pub fn run_review_step(&self, owner: &str) -> Result<ReviewStep, OperationError> {
        let kind = OperationKind::BulkReviewApply;
        let begun = self
            .db
            .write_blocking(Lane::Background, "operations.review_step", |tx| {
                let now = self.clock.now_secs();
                let job = match jobs::held(tx, kind, owner)? {
                    Some(job) => job,
                    None => match jobs::claim(tx, kind, owner, now)? {
                        Some(job) => job,
                        None => return Ok(Begun::Idle),
                    },
                };
                if let Some(halted) = jobs::checkpoint(tx, &job.id, owner, now)? {
                    return Ok(Begun::Ended(halted));
                }
                if !jobs::heartbeat(tx, &job.id, owner, now)? {
                    // The lease went elsewhere: that worker has the job.
                    return Ok(Begun::Idle);
                }
                let requester = match job.requested_by_user_id.as_deref() {
                    Some(user_id) => reviews::requester(tx, user_id)?,
                    None => None,
                };
                let Some(requester) = requester else {
                    // The person who asked was removed: nobody stands
                    // behind the rest of the change.
                    jobs::finish(
                        tx,
                        &job.id,
                        owner,
                        OperationState::Cancelled,
                        "STOPPED",
                        jobs::Tally::AsCounted,
                        now,
                    )?;
                    return Ok(Begun::Ended(crate::jobs::store::job_after(tx, &job.id)?));
                };
                // A review a failed step left running is taken again first.
                let ordinal = match jobs::running_work(tx, &job.id)? {
                    Some(ordinal) => Some(ordinal),
                    None => jobs::claim_work(tx, &job.id, now)?,
                };
                let Some(ordinal) = ordinal else {
                    jobs::finish(
                        tx,
                        &job.id,
                        owner,
                        OperationState::Succeeded,
                        reasons::REVIEW_JOB_DONE,
                        jobs::Tally::AsCounted,
                        now,
                    )?;
                    return Ok(Begun::Ended(crate::jobs::store::job_after(tx, &job.id)?));
                };
                let work = jobs::work_item(tx, &job.id, ordinal)?
                    .as_ref()
                    .and_then(reviews::work_of);
                Ok::<_, OperationError>(Begun::Review(Box::new(TakenReview {
                    job_id: job.id,
                    ordinal,
                    requester,
                    work,
                })))
            })?;
        let taken = match begun {
            Begun::Idle => return Ok(ReviewStep::Idle),
            Begun::Ended(job) => return Ok(ReviewStep::Ended(job)),
            Begun::Review(taken) => taken,
        };
        match self.settle_taken(&taken, owner) {
            Ok(job) => Ok(ReviewStep::Worked(job)),
            Err(error) => {
                tracing::info!(
                    job_id = taken.job_id,
                    %error,
                    "a review could not be recorded; the job goes back to the queue"
                );
                if let Err(release) = self.release_review_job(owner) {
                    tracing::warn!(job_id = taken.job_id, %release, "review job not released");
                }
                Err(error)
            }
        }
    }

    /// Settle the review a step took and record its outcome.
    fn settle_taken(
        &self,
        taken: &TakenReview,
        owner: &str,
    ) -> Result<OperationJob, OperationError> {
        let TakenReview {
            job_id,
            ordinal,
            requester,
            work,
        } = taken;
        let actor = requester.user_id();
        // Excluding and retrying are an admin's: a requester who is no
        // longer one stands behind none of the rest.
        let refused = match work {
            Some((action, _)) if action.needs_admin() && requester.role() != Role::Admin => Some(
                OperationError::Conflict(reasons::REVIEW_REQUESTER_NOT_ADMIN),
            ),
            _ => None,
        };
        // An accept seals through identification's own write, so it runs
        // before the outcome is recorded; the others commit with it.
        let accepted = match (&refused, work) {
            (None, Some((reviews::ReviewJobAction::Accept, review))) => {
                Some(self.accept_one(review, actor))
            }
            _ => None,
        };
        let accepted = match accepted {
            Some(Err(OperationError::Busy(cause))) => return Err(OperationError::Busy(cause)),
            other => other,
        };
        self.db
            .write_blocking(Lane::Background, "operations.review_step_done", |tx| {
                let now = self.clock.now_secs();
                if !jobs::holds(tx, job_id, owner)? {
                    return crate::jobs::store::job_after(tx, job_id);
                }
                let outcome = match (refused, work, accepted) {
                    (Some(refused), _, _) => Err(refused),
                    (None, None, _) => Err(OperationError::Invalid(reasons::REVIEW_ACTION_FAILED)),
                    (None, Some(_), Some(accepted)) => accepted,
                    (None, Some((action, review)), None) => savepoint(tx, "review_item", || {
                        self.settle_one(tx, *action, review, actor, now)
                    })?,
                };
                let (state, code) = match outcome {
                    Ok(()) => (WorkState::Succeeded, None),
                    Err(
                        OperationError::NotFound(reason)
                        | OperationError::Invalid(reason)
                        | OperationError::Conflict(reason),
                    ) => (WorkState::Skipped, Some(reason.code)),
                    Err(cause) => {
                        tracing::warn!(job_id, %cause, "review job could not settle a review");
                        (WorkState::Failed, Some(reasons::REVIEW_ACTION_FAILED.code))
                    }
                };
                jobs::finish_work(tx, job_id, *ordinal, state, code, None, now)?;
                crate::jobs::store::job_after(tx, job_id)
            })
    }

    /// Accept one review's release, unless a person pinned the album's
    /// edition since it was picked.
    fn accept_one(&self, review: &reviews::ReviewWork, actor: &str) -> Result<(), OperationError> {
        let Some(key) = review.candidate_key.as_deref() else {
            return Err(OperationError::Conflict(reasons::REVIEW_NO_CANDIDATE));
        };
        if self.review(&review.review_id)?.manual_identity {
            return Err(OperationError::Conflict(reasons::REVIEW_ALBUM_PINNED));
        }
        self.approve_review(&review.review_id, review.revision, key, actor)
            .map(drop)
    }

    /// Keep as tagged, exclude or retry one review, on `tx`.
    fn settle_one(
        &self,
        tx: &rusqlite::Transaction<'_>,
        action: reviews::ReviewJobAction,
        review: &reviews::ReviewWork,
        actor: &str,
        now: f64,
    ) -> Result<(), OperationError> {
        let settle = match action {
            reviews::ReviewJobAction::KeepTagged => reviews::ReviewAction::KeepTagged,
            reviews::ReviewJobAction::Exclude => reviews::ReviewAction::Exclude,
            reviews::ReviewJobAction::Retry => {
                return reviews::retry(tx, &self.management, review, actor, now);
            }
            reviews::ReviewJobAction::Accept => {
                return Err(OperationError::Invalid(reasons::REVIEW_ACTION_FAILED));
            }
        };
        reviews::settle(tx, &review.review_id, settle, review.revision, actor, now)
    }
}

/// A review job with what the review page shows about it.
#[derive(Debug, Clone)]
pub struct ReviewJob {
    pub job: OperationJob,
    /// `None` for a job whose work is gone.
    pub action: Option<reviews::ReviewJobAction>,
    /// Reviews it left alone, by reason.
    pub skipped: Vec<(reasons::Reason, u32)>,
}

/// What one step of the review jobs did.
#[derive(Debug, Clone)]
pub enum ReviewStep {
    /// No review job was due.
    Idle,
    /// One review was handled; the job goes on.
    Worked(OperationJob),
    /// The job finished, paused or stopped.
    Ended(OperationJob),
}

/// The first part of a review step, before its review is handled.
enum Begun {
    Idle,
    Ended(OperationJob),
    Review(Box<TakenReview>),
}

/// The review a step took, with who asked for the change.
struct TakenReview {
    job_id: String,
    ordinal: i64,
    /// The requester as the stored row has them now.
    requester: Principal,
    /// `None` when the item's input does not read.
    work: Option<(reviews::ReviewJobAction, reviews::ReviewWork)>,
}
