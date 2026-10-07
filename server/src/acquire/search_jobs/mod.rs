//! Manual album searches: a person searches every download source for one
//! album, looks over the candidates, and picks the one to download.
//!
//! Starting a search records a job and runs it in the background. Every
//! source turned on is asked at once; each ranks its own results the way
//! the automatic path does (Soulseek folders against the chosen edition's
//! tracklist), and one source failing only drops its group. The job then
//! holds the ranked list. Picking one queues a download linked to it: the
//! worker fetches exactly that candidate first and falls back to the
//! normal search on failover. "None of these" puts the album on the
//! wanted watchlist instead; cancel just closes the job. Each change is
//! announced to the owner as `search_job_updated` on the event stream.
//!
//! Ownership follows v2: a job belongs to the person who started it.
//! Only they may view, pick or cancel it; an admin may also dismiss it.

pub mod candidates;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use self::candidates::{Candidate, Pin};
use self::store::{
    CANCELLED, COMPLETED, FAILED, JobPayload, JobRow, JobStore, MATCHED, NewJob, PickRefusal,
    PickedTask, SEARCHING,
};
use super::db::now_epoch;
use super::dispatch::Journal;
use super::downloads::store::{NewTask, TaskDetails, TaskRow};
use super::edition::library_holds;
use super::requests::auth::Principal;
use super::requests::dispatch::DispatchOrigin;
use super::requests::error::RequestsError;
use super::requests::ledger::{WATCH_DORMANT, WATCH_FULFILLED, WATCH_STOPPED, WantedWatch};
use super::requests::quota::QuotaLedger;
use super::requests::sqlite::WantedStore;
use super::target::{SearchTarget, Targets};
use super::worker::DownloadWorker;
use crate::events::{EventSink, SearchJobUpdated, UserNotice, new_event_id};

/// How long one source may take to answer a manual search.
const SOURCE_TIMEOUT: Duration = Duration::from_secs(150);

/// Why a search came back empty or could not run: a stable code, what
/// happened and what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchReason {
    /// No source is turned on.
    NoSources,
    /// Every source failed to answer.
    SourcesFailed,
    /// The server restarted while the search ran.
    Interrupted,
    /// The search ran and no source had the album.
    NothingFound,
    /// The job's candidate list is from an older version and cannot be
    /// picked from.
    Expired,
}

impl SearchReason {
    /// Stable code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NoSources => "no_sources",
            Self::SourcesFailed => "sources_failed",
            Self::Interrupted => "interrupted",
            Self::NothingFound => "nothing_found",
            Self::Expired => "expired",
        }
    }

    /// The reason for a stored code.
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::NoSources,
            Self::SourcesFailed,
            Self::Interrupted,
            Self::NothingFound,
            Self::Expired,
        ]
        .into_iter()
        .find(|reason| reason.code() == code)
    }

    /// What happened, in one sentence.
    pub fn text(self) -> &'static str {
        match self {
            Self::NoSources => "No download source is turned on.",
            Self::SourcesFailed => "None of your download sources answered the search.",
            Self::Interrupted => "The search stopped because DroppedNeedle restarted.",
            Self::NothingFound => "No source has this album right now.",
            Self::Expired => "This search is from an older version of DroppedNeedle.",
        }
    }

    /// What the person can do about it.
    pub fn action(self) -> &'static str {
        match self {
            Self::NoSources => "Turn on slskd, SABnzbd or a plugin source in Settings, Downloads.",
            Self::SourcesFailed => {
                "Check that your download clients are running and reachable, then search again."
            }
            Self::Interrupted | Self::Expired => "Search again.",
            Self::NothingFound => {
                "Try again later, or choose \"None of these\" to have DroppedNeedle keep watching for it."
            }
        }
    }
}

/// Why a search action was refused.
#[derive(Debug)]
pub enum SearchJobError {
    /// No such job.
    NotFound,
    /// Someone else's job.
    Forbidden,
    /// The input was wrong; the text says how.
    Invalid(String),
    /// The job or the album is in a state that refuses this; the text
    /// says why and what to do.
    Conflict(String),
    /// A request rule refused it (storage limit).
    Refused(RequestsError),
    /// Our side failed.
    Internal(String),
}

/// What a person asked to search for.
#[derive(Debug, Clone)]
pub struct AlbumSearch {
    pub artist_name: String,
    pub album_title: String,
    pub year: Option<i32>,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
}

/// What starting a search did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// The search runs as this job.
    Searching(String),
    /// The library already holds the album; nothing was searched.
    AlreadyInLibrary,
}

/// One job as its owner sees it.
#[derive(Debug, Clone)]
pub struct JobView {
    pub row: JobRow,
    /// `None` when the stored list could not be read (an older job).
    pub payload: Option<JobPayload>,
    /// The download a pick started, if any.
    pub task_id: Option<String>,
}

impl JobView {
    /// Why the job sits where it does, when that needs explaining.
    pub fn reason(&self) -> Option<SearchReason> {
        if let Some(code) = self.row.error_message.as_deref() {
            return SearchReason::from_code(code);
        }
        match (&self.payload, self.row.status.as_str()) {
            (None, COMPLETED) => Some(SearchReason::Expired),
            (Some(payload), COMPLETED) if payload.candidates.is_empty() => {
                Some(SearchReason::NothingFound)
            }
            _ => None,
        }
    }
}

/// Manual album searches.
pub struct SearchJobs {
    store: JobStore,
    worker: Arc<DownloadWorker>,
    targets: Arc<Targets>,
    quota: Arc<QuotaLedger>,
    wanted: WantedStore,
    events: EventSink,
    /// Searches running in this process, to cancel them and to tell an
    /// interrupted job from a running one.
    running: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

fn normalise_mbid(value: Option<String>) -> Option<String> {
    value
        .map(|mbid| mbid.trim().to_ascii_lowercase())
        .filter(|mbid| !mbid.is_empty())
}

impl SearchJobs {
    /// Searches over the download worker's live sources.
    pub fn new(
        store: JobStore,
        worker: Arc<DownloadWorker>,
        targets: Arc<Targets>,
        quota: Arc<QuotaLedger>,
        events: EventSink,
    ) -> Self {
        let wanted = WantedStore::new(store.db().clone());
        Self {
            store,
            worker,
            targets,
            quota,
            wanted,
            events,
            running: Mutex::new(HashMap::new()),
        }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<String, tokio::task::AbortHandle>> {
        match self.running.lock() {
            Ok(running) => running,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn announce(&self, user_id: &str, job_id: &str, status: &str, candidate_count: usize) {
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

    /// Start a search. An album the library already holds is not searched
    /// for (v2's `already_in_library` answer).
    pub async fn start(
        self: &Arc<Self>,
        principal: &Principal,
        ask: AlbumSearch,
    ) -> Result<StartOutcome, SearchJobError> {
        let artist_name = ask.artist_name.trim().to_owned();
        let album_title = ask.album_title.trim().to_owned();
        if artist_name.is_empty() || album_title.is_empty() {
            return Err(SearchJobError::Invalid(
                "Give the artist and the album title to search for.".to_owned(),
            ));
        }
        if self.worker.sources_now().is_empty() {
            let reason = SearchReason::NoSources;
            return Err(SearchJobError::Conflict(format!(
                "{} {}",
                reason.text(),
                reason.action()
            )));
        }
        let release_group_mbid = normalise_mbid(ask.release_group_mbid);
        if let Some(group) = release_group_mbid.as_deref() {
            let held = library_holds(self.store.db().pool(), group)
                .await
                .map_err(|error| SearchJobError::Internal(error.to_string()))?;
            if held {
                return Ok(StartOutcome::AlreadyInLibrary);
            }
        }
        let job = NewJob {
            id: uuid::Uuid::new_v4().simple().to_string(),
            user_id: principal.user_id.clone(),
            artist_name,
            album_title,
            year: ask.year,
            release_group_mbid,
            release_mbid: normalise_mbid(ask.release_mbid),
        };
        self.store
            .insert(job.clone())
            .await
            .map_err(SearchJobError::Internal)?;
        let job_id = job.id.clone();
        {
            // Registered under the lock, so the run's own removal at the
            // end always finds it.
            let mut running = self.running();
            let handle = tokio::spawn(Arc::clone(self).run(job));
            running.insert(job_id.clone(), handle.abort_handle());
        }
        self.announce(&principal.user_id, &job_id, SEARCHING, 0);
        Ok(StartOutcome::Searching(job_id))
    }

    /// Ask every source, store the ranked candidates, announce.
    async fn run(self: Arc<Self>, job: NewJob) {
        let tracklist = match job.release_mbid.as_deref() {
            Some(release) => self.targets.edition_tracklist(release).await,
            None => Vec::new(),
        };
        let target = SearchTarget {
            artist: job.artist_name.clone(),
            album_title: job.album_title.clone(),
            year: job.year,
            tracklist,
            wanted: Vec::new(),
            track: None,
        };
        let sources = self.worker.sources_now();
        let searches = sources.iter().map(|source| {
            let target = &target;
            async move {
                let found =
                    tokio::time::timeout(SOURCE_TIMEOUT, source.search_candidates(target)).await;
                (source.journal_source().to_owned(), found)
            }
        });
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut failed = 0;
        // Sources come back in the configured try order, so the list is
        // grouped Soulseek, Usenet, plugins (or as configured).
        for (source, found) in futures_util::future::join_all(searches).await {
            match found {
                Ok(Ok(found)) => candidates.extend(found),
                Ok(Err(error)) => {
                    failed += 1;
                    tracing::warn!(job_id = %job.id, source, %error, "manual search: source failed");
                }
                Err(_) => {
                    failed += 1;
                    tracing::warn!(job_id = %job.id, source, "manual search: source timed out");
                }
            }
        }
        for (index, candidate) in candidates.iter_mut().enumerate() {
            candidate.view.candidate_index = index;
        }
        let count = candidates.len();
        let (status, reason) = if count == 0 && failed > 0 && failed == sources.len() {
            (FAILED, Some(SearchReason::SourcesFailed.code()))
        } else {
            (COMPLETED, None)
        };
        let payload = JobPayload {
            release_mbid: job.release_mbid.clone(),
            tracks_total: (!target.tracklist.is_empty()).then_some(target.tracklist.len()),
            candidates,
        };
        match self.store.finish(&job.id, status, payload, reason).await {
            Ok(true) => self.announce(&job.user_id, &job.id, status, count),
            // Cancelled while it ran: leave it cancelled.
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(job_id = %job.id, %error, "manual search result could not be stored");
            }
        }
        self.running().remove(&job.id);
    }

    /// Load a job the principal may act on.
    async fn owned(
        &self,
        principal: &Principal,
        job_id: &str,
        admin_ok: bool,
    ) -> Result<JobRow, SearchJobError> {
        let row = self
            .store
            .get(job_id)
            .await
            .map_err(SearchJobError::Internal)?
            .ok_or(SearchJobError::NotFound)?;
        let admin = admin_ok && principal.role.is_admin();
        if row.user_id != principal.user_id && !admin {
            return Err(SearchJobError::Forbidden);
        }
        Ok(row)
    }

    /// One job, with its candidates.
    pub async fn get(
        &self,
        principal: &Principal,
        job_id: &str,
    ) -> Result<JobView, SearchJobError> {
        let mut row = self.owned(principal, job_id, false).await?;
        if row.status == SEARCHING && !self.running().contains_key(job_id) {
            // Nothing runs it any more: the server restarted mid-search.
            let reason = SearchReason::Interrupted.code();
            self.store
                .transition(job_id, FAILED, &[SEARCHING], Some(reason))
                .await
                .map_err(SearchJobError::Internal)?;
            row.status = FAILED.to_owned();
            row.error_message = Some(reason.to_owned());
        }
        let payload = JobPayload::decode(&row.blob);
        let task_id = self
            .store
            .picked_task(job_id)
            .await
            .map_err(SearchJobError::Internal)?;
        Ok(JobView {
            row,
            payload,
            task_id,
        })
    }

    /// Download one candidate. Answers the new download task's id.
    pub async fn pick(
        &self,
        principal: &Principal,
        job_id: &str,
        candidate_index: usize,
    ) -> Result<String, SearchJobError> {
        let row = self.owned(principal, job_id, false).await?;
        if row.status != COMPLETED {
            return Err(SearchJobError::Conflict(not_open(&row.status)));
        }
        let payload = JobPayload::decode(&row.blob).ok_or_else(|| {
            let reason = SearchReason::Expired;
            SearchJobError::Conflict(format!("{} {}", reason.text(), reason.action()))
        })?;
        let candidate = payload
            .candidates
            .get(candidate_index)
            .ok_or_else(|| {
                SearchJobError::Invalid(format!(
                    "This search has no candidate number {candidate_index}."
                ))
            })?
            .clone();
        let source = candidate.view.source.clone();
        if !self
            .worker
            .sources_now()
            .iter()
            .any(|live| live.journal_source() == source)
        {
            return Err(SearchJobError::Conflict(format!(
                "{} is turned off now. Turn it back on in Settings, or pick a candidate from another source.",
                source_label(&source)
            )));
        }
        self.quota
            .check_storage_admission(&row.user_id, principal.role, DispatchOrigin::User)
            .await
            .map_err(SearchJobError::Refused)?;
        let task_id = uuid::Uuid::new_v4().simple().to_string();
        let picked = PickedTask {
            task: NewTask {
                id: task_id.clone(),
                user_id: row.user_id.clone(),
                artist_name: row.artist_name.clone(),
                album_title: row.album_title.clone(),
                release_group_mbid: row.release_group_mbid.clone().unwrap_or_default(),
                origin: "user".to_owned(),
                retry_count: 0,
            },
            details: TaskDetails {
                release_mbid: payload.release_mbid.clone(),
                year: row.year,
                ..TaskDetails::default()
            },
            username: match &candidate.pin {
                Pin::Soulseek { username, .. } => Some(username.clone()),
                Pin::Release { .. } => None,
            },
            source,
            candidate_index: i64::try_from(candidate_index).unwrap_or(i64::MAX),
        };
        match self
            .store
            .pick(job_id, picked)
            .await
            .map_err(SearchJobError::Internal)?
        {
            Ok(()) => {
                self.worker.wake();
                self.announce(&row.user_id, job_id, MATCHED, payload.candidates.len());
                Ok(task_id)
            }
            Err(PickRefusal::NotOpen(status)) => Err(SearchJobError::Conflict(not_open(&status))),
            Err(PickRefusal::AlbumBusy) => Err(SearchJobError::Conflict(
                "This album is already downloading. Cancel that download first if you want this source instead."
                    .to_owned(),
            )),
        }
    }

    /// Close a job without downloading anything.
    pub async fn cancel(&self, principal: &Principal, job_id: &str) -> Result<(), SearchJobError> {
        let row = self.owned(principal, job_id, false).await?;
        self.close(&row).await
    }

    async fn close(&self, row: &JobRow) -> Result<(), SearchJobError> {
        match row.status.as_str() {
            MATCHED => {
                return Err(SearchJobError::Conflict(
                    "A download already started from this search. Cancel the download instead."
                        .to_owned(),
                ));
            }
            CANCELLED => return Ok(()),
            _ => {}
        }
        if let Some(handle) = self.running().remove(&row.id) {
            handle.abort();
        }
        self.store
            .transition(&row.id, CANCELLED, &[SEARCHING, COMPLETED, FAILED], None)
            .await
            .map_err(SearchJobError::Internal)?;
        self.announce(&row.user_id, &row.id, CANCELLED, 0);
        Ok(())
    }

    /// "None of these, keep watching": close the job, remember every
    /// candidate as turned down, and put the album on the wanted
    /// watchlist (or wake its watch). Answers the watch's state.
    pub async fn dismiss(
        &self,
        principal: &Principal,
        job_id: &str,
    ) -> Result<String, SearchJobError> {
        let row = self.owned(principal, job_id, true).await?;
        let Some(group) = row.release_group_mbid.clone() else {
            return Err(SearchJobError::Invalid(
                "This search isn't tied to an album, so it can't be watched.".to_owned(),
            ));
        };
        self.close(&row).await?;
        let refused = |error: RequestsError| SearchJobError::Refused(error);
        let now = now_epoch();
        let seen: Vec<(String, String)> = JobPayload::decode(&row.blob)
            .map(|payload| {
                payload
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.view.source.clone(), candidate.seen_identity()))
                    .collect()
            })
            .unwrap_or_default();
        if !seen.is_empty() {
            self.wanted
                .add_seen(&group, seen, now)
                .await
                .map_err(refused)?;
        }
        let existing = self.wanted.get(&group).await.map_err(refused)?;
        match existing.as_ref().map(|watch| watch.state.as_str()) {
            None | Some(WATCH_FULFILLED) => {
                let at = i64::try_from(now).unwrap_or(i64::MAX);
                let next = crate::acquire::flows::loops::interval_seconds(None, 0, at);
                self.wanted
                    .enrol(WantedWatch {
                        key: group.clone(),
                        user_id: row.user_id.clone(),
                        user_name: None,
                        artist_name: row.artist_name.clone(),
                        album_title: row.album_title.clone(),
                        artist_mbid: None,
                        year: row.year,
                        cover_url: None,
                        kind: "missing".to_owned(),
                        state: String::new(),
                        created_at: now,
                        first_release_date: None,
                        check_count: 0,
                        quiet_streak: 0,
                        next_check_at: now.saturating_add(u64::try_from(next).unwrap_or(0)),
                        new_candidate_count: 0,
                    })
                    .await
                    .map_err(refused)?;
            }
            Some(WATCH_DORMANT | WATCH_STOPPED) => {
                // The person just asked: wake it whoever's watch it is.
                self.wanted
                    .resume(&group, &row.user_id, true, now)
                    .await
                    .map_err(refused)?;
            }
            Some(_) => {}
        }
        let state = self
            .wanted
            .get(&group)
            .await
            .map_err(refused)?
            .map(|watch| watch.state)
            .unwrap_or_else(|| "watching".to_owned());
        tracing::info!(job_id, release_group_mbid = %group, "manual search dismissed; album watched");
        Ok(state)
    }
}

/// Why a job can no longer be picked from.
fn not_open(status: &str) -> String {
    match status {
        SEARCHING => "The search is still running. Wait for it to finish, then pick.".to_owned(),
        MATCHED => "A download already started from this search.".to_owned(),
        CANCELLED => "This search was closed. Start a new search to pick again.".to_owned(),
        _ => "This search did not finish. Start a new search.".to_owned(),
    }
}

/// A source tag as people know it.
fn source_label(source: &str) -> String {
    match source {
        "soulseek" => "Soulseek".to_owned(),
        "usenet" => "Usenet".to_owned(),
        other => other.strip_prefix("plugin:").unwrap_or(other).to_owned(),
    }
}

/// The candidate a person picked for this task, when `source` is about to
/// make its first attempt at it. Anything unreadable is logged and the
/// source searches as usual.
pub async fn pinned(
    journal: &Journal,
    task: &TaskRow,
    source: &str,
    source_index: i64,
) -> Option<Pin> {
    if source_index != 0 {
        return None;
    }
    let job_id = task.search_job_id.as_deref()?;
    let index = usize::try_from(task.candidate_index?).ok()?;
    let blob: Option<String> = match sqlx::query_scalar(
        "SELECT candidates_blob FROM search_jobs WHERE id = ?1",
    )
    .bind(job_id)
    .fetch_optional(journal.db().pool())
    .await
    {
        Ok(blob) => blob,
        Err(error) => {
            tracing::warn!(task_id = %task.id, %error, "picked candidate unreadable; searching instead");
            return None;
        }
    };
    let Some(payload) = blob.as_deref().and_then(JobPayload::decode) else {
        tracing::warn!(task_id = %task.id, job_id, "picked candidate's search is gone; searching instead");
        return None;
    };
    payload
        .candidates
        .into_iter()
        .nth(index)
        .filter(|candidate| candidate.view.source == source)
        .map(|candidate| candidate.pin)
}
