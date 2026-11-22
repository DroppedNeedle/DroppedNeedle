//! The look before an automatic grab (search review). Before a queued
//! album's first attempt, the worker searches every source the way a
//! manual search does and records that search as a job. When the setting
//! allows and a candidate is recommended, the task is linked to it and
//! fetched like a hand pick on the next pass. Otherwise the task is parked
//! on the job and waits for its owner to pick one, or to turn them all
//! down. An upgrade never waits: it takes a recommended copy whatever the
//! setting says, and without one it settles on the next pass with the
//! library copy kept.
//!
//! When the edition is only a preference and no copy fits it, a Soulseek
//! folder that holds another edition of the album whole, by the ranking's
//! own check, is recommended too, and the task then prefers that edition,
//! still only as a preference. A pinned edition and an upgrade are judged
//! against their edition alone.
//!
//! When MusicBrainz did not answer for the edition's tracklist, no folder
//! can be judged. Unless every album waits for a pick anyway, a look that
//! would park then leaves the album queued and
//! looks again later, each wait as long as the time waited so far, for up
//! to a day after the album was queued; after that it parks as above. A
//! copy recommended without the tracklist is still taken.
//!
//! A look can take minutes (every source answers under its own time
//! limit), so it runs on its own task, never inside the worker pass: polls,
//! landings and the watchdog keep their cadence. At most
//! [`MAX_CONCURRENT_REVIEWS`] run at once; other albums wait their turn in
//! the queue.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use super::{DownloadWorker, Pass, SourceSet, in_try_order};
use crate::dispatch::DispatchOrigin;
use crate::downloads::TaskPlan;
use crate::downloads::store::TaskRow;
use crate::search_jobs::candidates::{Candidate, CandidateTier, Pin};
use crate::search_jobs::review::{ReviewMode, Verdict, verdict};
use crate::search_jobs::search_sources;
use crate::search_jobs::store::{JobPayload, JobStatus, JobStore, NewJob, ReviewOutcome};
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

/// The first wait before looking again at an album whose edition's
/// tracklist MusicBrainz did not answer for. Each wait after it is as long
/// as the time waited so far.
const FIRST_TRACKLIST_WAIT_SECONDS: f64 = 15.0 * 60.0;

/// How long after it was queued an album keeps waiting for its edition's
/// tracklist before it waits for a pick instead.
const TRACKLIST_WAIT_WINDOW_SECONDS: f64 = 24.0 * 3_600.0;

/// Tracklists one look reads for other editions of an album, at most,
/// whatever they answer.
const MAX_EDITION_READS: usize = 4;

/// A folder a look recommended for another edition of the album.
struct Fitted {
    /// Its place among the candidates.
    index: usize,
    /// The edition it holds whole.
    release_mbid: String,
    /// That edition's audio tracks.
    tracks: usize,
}

/// What one look decided.
enum Reviewed {
    /// The task is linked to a candidate or parked; the next pass sees it.
    Recorded,
    /// No look applies or nothing was found: search the usual way.
    Search,
    /// Nothing could be judged without the edition's tracklist: the task
    /// stays queued and is looked at again at this time (unix seconds).
    LookAgainAt(f64),
}

/// When to look again at an album queued at `queued_at` whose edition's
/// tracklist could not be read at `now`: after as long again as it has
/// waited, at least the first wait, and never past the window's end.
/// `None` once the window is over.
fn next_look(queued_at: f64, now: f64) -> Option<f64> {
    let waited = (now - queued_at).max(0.0);
    if waited >= TRACKLIST_WAIT_WINDOW_SECONDS {
        return None;
    }
    let next = now + waited.max(FIRST_TRACKLIST_WAIT_SECONDS);
    Some(next.min(queued_at + TRACKLIST_WAIT_WINDOW_SECONDS))
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
            match worker.review(mode, &sources, &order, &task).await {
                Reviewed::Search => {
                    lock(&worker.reviews.looked).insert(task.id.clone());
                }
                Reviewed::LookAgainAt(at) => worker.wait_until(&task.id, at),
                Reviewed::Recorded => {}
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
        let target = match self.targets.target(task).await {
            Ok(target) if target.track.is_none() => target,
            Ok(_) => return Reviewed::Search,
            Err(error) => {
                tracing::warn!(task_id = %task.id, ?error, "search review: target unavailable; searching as usual");
                return Reviewed::Search;
            }
        };
        let (release_mbid, plan) = {
            let task_id = task.id.clone();
            match self
                .journal
                .read("downloads.review.details", move |store| {
                    Ok((store.task_details(&task_id)?, store.task_plan(&task_id)?))
                })
                .await
            {
                Ok((details, plan)) => (details.release_mbid, Some(plan)),
                Err(error) => {
                    tracing::warn!(task_id = %task.id, %error, "search review: task details unreadable");
                    (None, None)
                }
            }
        };
        let job_id = uuid::Uuid::new_v4().simple().to_string();
        let ordered: Vec<_> = in_try_order(sources, order).into_iter().cloned().collect();
        let (mut candidates, _failed) = search_sources(&ordered, &target, &job_id).await;
        let upgrade = task.origin == DispatchOrigin::Upgrade;
        // "Always wait for my pick" covers requests and wanted albums: an
        // upgrade that waits costs nothing but a growing queue.
        let mode = if upgrade && mode == ReviewMode::Always {
            ReviewMode::WhenUnsure
        } else {
            mode
        };
        // The edition is only a preference and no copy fits it: a folder
        // that holds another edition of the album whole proves that
        // edition just as well.
        let fitted = match &plan {
            Some(plan)
                if mode == ReviewMode::WhenUnsure
                    && target.release_preferred
                    && target.wanted.is_empty()
                    && !upgrade =>
            {
                self.fit_other_edition(task, plan, &mut candidates).await
            }
            _ => None,
        };
        let outcome = match verdict(mode, &candidates) {
            Verdict::Nothing => return Reviewed::Search,
            // MusicBrainz did not answer for the tracklist, so no folder
            // could be judged: the album waits and is looked at again,
            // each wait longer, before it waits for a pick.
            Verdict::Park
                if target.tracklist_unread && mode == ReviewMode::WhenUnsure && !upgrade =>
            {
                let now = self.now_unix_f64();
                match next_look(task.created_at, now) {
                    Some(at) => {
                        tracing::info!(
                            task_id = %task.id,
                            candidates = candidates.len(),
                            wait_minutes = ((at - now) / 60.0).round(),
                            "search review: the edition's tracklist could not be read; looking again later"
                        );
                        return Reviewed::LookAgainAt(at);
                    }
                    None => ReviewOutcome::Park,
                }
            }
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
        // A folder taken for the edition it fits names that edition, still
        // only a preference, which the task's plan then keeps.
        let payload = match (&outcome, fitted) {
            (ReviewOutcome::Take(index, ..), Some(fit))
                if usize::try_from(*index).ok() == Some(fit.index) =>
            {
                JobPayload {
                    release_mbid: Some(fit.release_mbid),
                    release_preferred: true,
                    tracks_total: Some(fit.tracks),
                    candidates,
                }
            }
            _ => JobPayload {
                release_mbid,
                release_preferred: false,
                tracks_total: (!target.tracklist.is_empty()).then_some(target.tracklist.len()),
                candidates,
            },
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
                self.announce_review(&task.user_id, &job_id, JobStatus::Matched, count);
            }
            ReviewOutcome::Park => {
                let what = if upgrade {
                    "upgrade found no better copy"
                } else {
                    "download waits for a pick"
                };
                tracing::info!(task_id = %task.id, job_id, candidates = count, "{what}");
                self.announce_review(&task.user_id, &job_id, JobStatus::Completed, count);
            }
        }
        Reviewed::Recorded
    }

    /// For an album whose edition is only a preference and that no copy
    /// fits: look for a Soulseek folder that holds another edition of the
    /// album whole. Only while nothing is recommended, and only folders
    /// possible for how they fit the preferred edition are judged. The
    /// editions read are the album's others whose track count is a judged
    /// folder's audio count, in the album page's order, at most
    /// [`MAX_EDITION_READS`] of them, whatever each answers. A read that
    /// fails ends the look there: what could not be read is never
    /// evidence. The first folder that fits is recommended.
    async fn fit_other_edition(
        &self,
        task: &TaskRow,
        plan: &TaskPlan,
        candidates: &mut [Candidate],
    ) -> Option<Fitted> {
        let group = task.release_group_mbid.trim();
        if group.is_empty()
            || candidates
                .iter()
                .any(|candidate| candidate.view.tier == CandidateTier::Recommended)
        {
            return None;
        }
        let counts: BTreeSet<usize> = candidates
            .iter()
            .filter(|candidate| candidate.unfit_for_its_edition())
            .map(Candidate::listed_audio)
            .filter(|count| *count > 0)
            .collect();
        if counts.is_empty() {
            return None;
        }
        let editions = match self.targets.group_editions(group).await {
            Ok(editions) => editions,
            Err(error) => {
                tracing::info!(task_id = %task.id, %error, "search review: the album's editions could not be read; no other edition tried");
                return None;
            }
        };
        let others = editions
            .into_iter()
            .filter(|(release, count)| counts.contains(count) && !plan.is_for(release))
            .take(MAX_EDITION_READS);
        for (release, _) in others {
            let tracklist = match self.targets.catalog_tracklist(&release).await {
                Ok(Some(tracklist)) if !tracklist.is_empty() => tracklist,
                Ok(_) => continue,
                Err(error) => {
                    tracing::info!(task_id = %task.id, release_mbid = %release, %error, "search review: another edition's tracklist could not be read; no further edition tried");
                    return None;
                }
            };
            for (index, candidate) in candidates.iter_mut().enumerate() {
                if candidate.unfit_for_its_edition() && candidate.fit_edition(&tracklist) {
                    tracing::info!(
                        task_id = %task.id,
                        release_mbid = %release,
                        "search review: a folder holds another edition of the album whole"
                    );
                    return Some(Fitted {
                        index,
                        release_mbid: release,
                        tracks: tracklist.len(),
                    });
                }
            }
        }
        None
    }

    fn announce_review(
        &self,
        user_id: &str,
        job_id: &str,
        status: JobStatus,
        candidate_count: usize,
    ) {
        self.events.notify(
            user_id,
            UserNotice::SearchJobUpdated(SearchJobUpdated {
                event_id: new_event_id(),
                job_id: job_id.to_owned(),
                status: status.as_str().to_owned(),
                candidate_count,
            }),
        );
    }
}
