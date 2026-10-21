//! What the identify service and its SQLite store (`sqlite`) share:
//! release recall helpers, the store error, approvals and job landings.

use droppedneedle_kernel::db::{DbError, Lane};

use super::models::{AlbumIdentity, IdentifyJob, JobState};
use super::sqlite::SqliteIdentifyStore;
use crate::matching::Release;

/// Release documents count as fresh for recall this long; older ones are
/// fetched again (and kept regardless while an identity names them).
pub const RELEASE_FRESH_SECS: u64 = 7 * 24 * 3600;

/// Keep every candidate document the store does not hold fresh, on a
/// blocking thread. A fresh document that does not yet answer to a merged
/// id the fetched one came through learns that id, so a lookup by it
/// finds the document. `lane` is the caller's: a request that recalls
/// writes on the request lane.
pub async fn keep_releases(
    store: &std::sync::Arc<SqliteIdentifyStore>,
    releases: Vec<Release>,
    lane: Lane,
) -> Result<(), StoreError> {
    if releases.is_empty() {
        return Ok(());
    }
    let store = store.clone();
    tokio::task::spawn_blocking(move || {
        for release in &releases {
            match store.release(&release.id, Some(RELEASE_FRESH_SECS))? {
                None => store.save_release(release, lane)?,
                Some(stored) => {
                    if release.old_ids.iter().any(|old| !stored.answers_to(old)) {
                        store.keep_old_ids(release, lane)?;
                    }
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|error| StoreError::Failed(format!("keeping release documents: {error}")))?
}

/// A stored document fresh enough for recall, read on a blocking thread.
pub async fn fresh_release(
    store: &std::sync::Arc<SqliteIdentifyStore>,
    release_mbid: &str,
) -> Result<Option<Release>, StoreError> {
    let store = store.clone();
    let release_mbid = release_mbid.to_owned();
    tokio::task::spawn_blocking(move || store.release(&release_mbid, Some(RELEASE_FRESH_SECS)))
        .await
        .map_err(|error| StoreError::Failed(format!("reading a release document: {error}")))?
}

/// The identify store could not answer. Nothing it was asked to write
/// landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The database was busy: a full queue, a turn or a read that waited
    /// too long, or the file lock. The cause is for the log.
    Busy(String),
    /// The database is shutting down.
    Closed,
    /// Anything else: a statement, a value that does not decode, a time
    /// budget. The cause is for the log.
    Failed(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(cause) => write!(f, "identify store busy: {cause}"),
            Self::Closed => write!(f, "identify store closed: the database is shutting down"),
            Self::Failed(cause) => write!(f, "identify store failed: {cause}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<DbError> for StoreError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { .. } => Self::Busy(error.to_string()),
            DbError::LaneClosed => Self::Closed,
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::from(error).into()
    }
}

impl From<StoreError> for crate::service::ServiceError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(_) | StoreError::Closed => Self::Busy {
                cause: error.to_string(),
            },
            StoreError::Failed(_) => Self::Internal {
                cause: error.to_string(),
            },
        }
    }
}

impl From<StoreError> for crate::operations::models::OperationError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(_) | StoreError::Closed => Self::Busy(error.to_string()),
            StoreError::Failed(_) => Self::Store(error.to_string()),
        }
    }
}

impl From<StoreError> for crate::management::models::ManagementError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(_) | StoreError::Closed => Self::Busy(error.to_string()),
            StoreError::Failed(_) => Self::Store(error.to_string()),
        }
    }
}

/// A curator's approval: the review it settles and the album match it
/// seals. The album identity's row revision is the store's to set; the
/// files are placed on the release afterwards, as on any chosen edition.
#[derive(Debug, Clone)]
pub struct Approval {
    pub review_id: String,
    pub by_user_id: String,
    pub candidate_key: String,
    pub album: AlbumIdentity,
}

/// States a claimed job can land in after one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptLanding {
    Done,
    Deferred,
    Attention,
    Failed,
}

pub fn land_job(
    job: &mut IdentifyJob,
    landing: AttemptLanding,
    now_ms: u64,
    failure: Option<&str>,
) {
    match landing {
        AttemptLanding::Done => {
            job.state = JobState::Succeeded;
            job.failure_code = None;
        }
        AttemptLanding::Failed => {
            job.state = JobState::Failed;
            job.failure_code = failure.map(str::to_owned);
        }
        AttemptLanding::Attention => {
            job.state = JobState::Attention;
            job.failure_code = failure.map(str::to_owned);
        }
        AttemptLanding::Deferred => {
            job.attempts += 1;
            job.failure_code = failure.map(str::to_owned);
            if super::queue::terminally_deferred(job.attempts) {
                job.state = JobState::Attention;
            } else {
                job.state = JobState::Deferred;
                job.not_before_ms = now_ms + super::queue::backoff_secs(job.attempts) * 1000;
            }
        }
    }
}
