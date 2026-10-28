//! The look before an automatic grab (search review). Before a queued
//! album's first attempt, the worker searches every source the way a
//! manual search does and records that search as a job. When the setting
//! allows and a candidate is recommended, the task is linked to it and
//! fetched like a hand pick on the next pass. Otherwise the task is parked
//! on the job and waits for its owner to pick one, or to turn them all
//! down.
//!
//! A look can take minutes (every source answers under its own time
//! limit), so it runs on its own task, never inside the worker pass: polls,
//! landings and the watchdog keep their cadence. At most
//! [`MAX_CONCURRENT_REVIEWS`] run at once; other albums wait their turn in
//! the queue.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{DownloadWorker, Pass, SourceSet, in_try_order};
use crate::downloads::store::TaskRow;
use crate::search_jobs::candidates::Pin;
use crate::search_jobs::review::{ReviewMode, Verdict, verdict};
use crate::search_jobs::search_sources;
use crate::search_jobs::store::{COMPLETED, JobPayload, JobStore, MATCHED, NewJob, ReviewOutcome};
use droppedneedle_kernel::events::{SearchJobUpdated, UserNotice, new_event_id};

/// Looks running at once.
pub(super) const MAX_CONCURRENT_REVIEWS: usize = 2;

/// Looks in flight, and tasks a look already finished for (they search
/// the usual way from then on).
#[derive(Default)]
pub(super) struct Reviews {
    running: Mutex<HashSet<String>>,
    pub(super) looked: Mutex<HashSet<String>>,
}

fn lock(set: &Mutex<HashSet<String>>) -> MutexGuard<'_, HashSet<String>> {
    match set.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// What one look decided.
enum Reviewed {
    /// The task is linked to a candidate or parked; the next pass sees it.
    Recorded,
    /// No look applies or nothing was found: search the usual way.
    Search,
}

impl DownloadWorker {
    /// Whether a queued task waits on a search for its owner's pick.
    pub(super) fn parked(task: &TaskRow) -> bool {
        task.search_job_id.is_some() && task.candidate_index.is_none()
    }

    /// Start the look before the first automatic attempt at an album, off
    /// the pass. Answers true while a look runs (or must wait for a free
    /// slot), so the pass leaves the task alone; false when the task
    /// should be searched the usual way now. Single tracks are left to the
    /// usual search, which fetches only the tracks needed from their
    /// album's folder.
    pub(super) fn start_review(self: &Arc<Self>, pass: &Pass, task: &TaskRow) -> bool {
        if pass.config.review == ReviewMode::Off
            || task.download_type == "track"
            || self.targets.is_none()
            || lock(&self.reviews.looked).contains(&task.id)
        {
            return false;
        }
        {
            let mut running = lock(&self.reviews.running);
            if running.contains(&task.id) {
                return true;
            }
            if running.len() >= MAX_CONCURRENT_REVIEWS {
                return true;
            }
            running.insert(task.id.clone());
        }
        let worker = Arc::clone(self);
        let task = task.clone();
        let mode = pass.config.review;
        let sources = pass.sources.clone();
        let order = pass.config.source_order.clone();
        tokio::spawn(async move {
            let outcome = worker.review(mode, &sources, &order, &task).await;
            if matches!(outcome, Reviewed::Search) {
                lock(&worker.reviews.looked).insert(task.id.clone());
            }
            lock(&worker.reviews.running).remove(&task.id);
            worker.wake();
        });
        true
    }

    /// Whether any look is still running (for tests and shutdown).
    pub fn reviews_running(&self) -> bool {
        !lock(&self.reviews.running).is_empty()
    }

    async fn review(
        &self,
        mode: ReviewMode,
        sources: &SourceSet,
        order: &[crate::downloads::source_tag::SourceTag],
        task: &TaskRow,
    ) -> Reviewed {
        let Some(targets) = &self.targets else {
            return Reviewed::Search;
        };
        let target = match targets.target(task).await {
            Ok(target) if target.track.is_none() => target,
            Ok(_) => return Reviewed::Search,
            Err(error) => {
                tracing::warn!(task_id = %task.id, ?error, "search review: target unavailable; searching as usual");
                return Reviewed::Search;
            }
        };
        let release_mbid = {
            let task_id = task.id.clone();
            match self
                .journal
                .read("downloads.review.details", move |store| {
                    store.task_details(&task_id)
                })
                .await
            {
                Ok(details) => details.release_mbid,
                Err(error) => {
                    tracing::warn!(task_id = %task.id, %error, "search review: task details unreadable");
                    None
                }
            }
        };
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let ordered: Vec<_> = in_try_order(sources, order).into_iter().cloned().collect();
        let (candidates, _failed) = search_sources(&ordered, &target, &job_id).await;
        let outcome = match verdict(mode, &candidates) {
            Verdict::Nothing => return Reviewed::Search,
            Verdict::Park => ReviewOutcome::Park,
            Verdict::Take(index) => {
                let candidate = &candidates[index];
                let username = match &candidate.pin {
                    Pin::Soulseek { username, .. } => Some(username.clone()),
                    Pin::Release { .. } => None,
                };
                ReviewOutcome::Take(
                    i64::try_from(index).unwrap_or(i64::MAX),
                    candidate.view.source.clone(),
                    username,
                )
            }
        };
        let count = candidates.len();
        let job = NewJob {
            id: job_id.clone(),
            user_id: task.user_id.clone(),
            artist_name: task.artist_name.clone(),
            album_title: task.album_title.clone(),
            year: target.year,
            release_group_mbid: (!task.release_group_mbid.is_empty())
                .then(|| task.release_group_mbid.clone()),
            release_mbid: release_mbid.clone(),
        };
        let payload = JobPayload {
            release_mbid,
            tracks_total: (!target.tracklist.is_empty()).then_some(target.tracklist.len()),
            candidates,
        };
        let store = JobStore::new(self.journal.db().clone());
        match store
            .record_review(job, payload, &task.id, outcome.clone())
            .await
        {
            // Cancelled or picked meanwhile: nothing to add.
            Ok(false) => return Reviewed::Recorded,
            Ok(true) => {}
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "search review could not be stored; searching as usual");
                return Reviewed::Search;
            }
        }
        match outcome {
            ReviewOutcome::Take(..) => {
                self.announce_review(&task.user_id, &job_id, MATCHED, count);
            }
            ReviewOutcome::Park => {
                tracing::info!(task_id = %task.id, job_id, candidates = count, "download waits for a pick");
                self.announce_review(&task.user_id, &job_id, COMPLETED, count);
            }
        }
        Reviewed::Recorded
    }

    fn announce_review(&self, user_id: &str, job_id: &str, status: &str, candidate_count: usize) {
        self.events.notify(
            user_id,
            UserNotice::SearchJobUpdated(SearchJobUpdated {
                event_id: new_event_id(),
                job_id: job_id.to_owned(),
                status: status.to_owned(),
                candidate_count,
            }),
        );
    }
}
