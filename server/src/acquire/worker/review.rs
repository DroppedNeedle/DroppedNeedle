//! The look before an automatic grab (search review). Before a queued
//! album's first attempt, the worker searches every source the way a
//! manual search does and records that search as a job. When the setting
//! allows and a candidate is recommended, the task is linked to it and
//! fetched like a hand pick. Otherwise the task is parked on the job and
//! waits for its owner to pick one, or to turn them all down.

use super::{DownloadWorker, Pass};
use crate::acquire::downloads::store::TaskRow;
use crate::acquire::search_jobs::candidates::Pin;
use crate::acquire::search_jobs::review::{ReviewMode, Verdict, verdict};
use crate::acquire::search_jobs::search_sources;
use crate::acquire::search_jobs::store::{
    COMPLETED, JobPayload, JobStore, MATCHED, NewJob, ReviewOutcome,
};
use crate::events::{SearchJobUpdated, UserNotice, new_event_id};

/// What the look decided for one task.
pub(super) enum Reviewed {
    /// Fetch this candidate; the task row carries the link now.
    Take(Box<TaskRow>),
    /// The task waits for a pick, or moved meanwhile: leave it.
    Wait,
    /// No look applies or nothing was found: search the usual way.
    Search,
}

impl DownloadWorker {
    /// Whether a queued task waits on a search for its owner's pick.
    pub(super) fn parked(task: &TaskRow) -> bool {
        task.search_job_id.is_some() && task.candidate_index.is_none()
    }

    /// Look before the first automatic attempt at an album. Single tracks
    /// are left to the usual search, which fetches only the tracks needed
    /// from their album's folder.
    pub(super) async fn review(&self, pass: &Pass, task: &TaskRow) -> Reviewed {
        if pass.config.review == ReviewMode::Off || task.download_type == "track" {
            return Reviewed::Search;
        }
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
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let sources = pass.ordered_sources();
        let (candidates, _failed) = search_sources(&sources, &target, &job_id).await;
        let outcome = match verdict(pass.config.review, &candidates) {
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
            release_mbid: None,
        };
        let payload = JobPayload {
            release_mbid: None,
            tracks_total: (!target.tracklist.is_empty()).then_some(target.tracklist.len()),
            candidates,
        };
        let store = JobStore::new(self.journal.db().clone());
        match store
            .record_review(job, payload, &task.id, outcome.clone())
            .await
        {
            Ok(true) => {}
            Ok(false) => return Reviewed::Wait,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "search review could not be stored; searching as usual");
                return Reviewed::Search;
            }
        }
        match outcome {
            ReviewOutcome::Take(index, source, username) => {
                self.announce_review(&task.user_id, &job_id, MATCHED, count);
                let mut linked = task.clone();
                linked.search_job_id = Some(job_id);
                linked.candidate_index = Some(index);
                linked.source = source;
                linked.source_username = username;
                Reviewed::Take(Box::new(linked))
            }
            ReviewOutcome::Park => {
                tracing::info!(task_id = %task.id, job_id, candidates = count, "download waits for a pick");
                self.announce_review(&task.user_id, &job_id, COMPLETED, count);
                Reviewed::Wait
            }
        }
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
