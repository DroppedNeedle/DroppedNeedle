//! What the identify service and its SQLite store (`sqlite`) share:
//! release recall helpers, the store error, approvals and job landings.

use super::models::{AlbumIdentity, IdentifyJob, JobState, TrackIdentity};
use super::sqlite::SqliteIdentifyStore;
use crate::library::matching::Release;

/// Release documents count as fresh for recall this long; older ones are
/// fetched again (and kept regardless while an identity names them).
pub const RELEASE_FRESH_SECS: u64 = 7 * 24 * 3600;

/// Keep every candidate document the store does not hold fresh, on a
/// blocking thread: the stores are synchronous SQLite.
pub async fn keep_releases(store: &std::sync::Arc<SqliteIdentifyStore>, releases: Vec<Release>) {
    if releases.is_empty() {
        return;
    }
    let store = store.clone();
    let kept = tokio::task::spawn_blocking(move || {
        for release in &releases {
            if store
                .release(&release.id, Some(RELEASE_FRESH_SECS))
                .is_none()
            {
                store.save_release(release);
            }
        }
    })
    .await;
    if let Err(error) = kept {
        tracing::warn!(%error, "keeping release documents failed");
    }
}

/// A stored document fresh enough for recall, read on a blocking thread.
pub async fn fresh_release(
    store: &std::sync::Arc<SqliteIdentifyStore>,
    release_mbid: &str,
) -> Option<Release> {
    let store = store.clone();
    let release_mbid = release_mbid.to_owned();
    match tokio::task::spawn_blocking(move || {
        store.release(&release_mbid, Some(RELEASE_FRESH_SECS))
    })
    .await
    {
        Ok(found) => found,
        Err(error) => {
            tracing::warn!(%error, "reading a release document failed");
            None
        }
    }
}

/// A durable store could not complete a write. The cause is for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    pub cause: String,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "identify store failed: {}", self.cause)
    }
}

impl std::error::Error for StoreError {}

/// A curator's approval: the review it settles and the identities it
/// seals. The album identity's row revision is the store's to set.
#[derive(Debug, Clone)]
pub struct Approval {
    pub review_id: String,
    pub by_user_id: String,
    pub candidate_key: String,
    pub album: AlbumIdentity,
    pub tracks: Vec<TrackIdentity>,
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
