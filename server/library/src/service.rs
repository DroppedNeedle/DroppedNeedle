//! The library's service error, and identification: requests, reviews
//! and the identify loop's turns.
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::ids::IdGenerator;

use super::clock::LibraryClock as _;
use super::identify::models::{AlbumIdentity, IdentifyJob, IdentifyKind};
use super::identify::service::IdentifyService;
use super::identify::sqlite::SqliteIdentifyStore;
use super::settings::LibrarySettings;

/// Why a library operation failed, independent of transport. The
/// HTTP layer maps each variant to a status.
#[derive(Debug)]
pub enum ServiceError {
    /// The named resource does not exist.
    NotFound,
    /// The request itself is invalid; the message is safe to show.
    InvalidInput { message: String },
    /// Valid input against the wrong state; the message is safe to show.
    Conflict { message: String },
    /// The database was too busy to answer in time, or is shutting down.
    /// Answered as the server-busy 503; the cause goes to the log.
    Busy { cause: String },
    /// A store or I/O fault. The cause goes to the log, never the caller.
    Internal { cause: String },
}

impl ServiceError {
    /// A store or I/O fault with its cause kept for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "not found"),
            Self::InvalidInput { message } | Self::Conflict { message } => write!(f, "{message}"),
            Self::Busy { cause } | Self::Internal { cause } => write!(f, "{cause}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// Identification requests and reviews, and the identify loop's turns.
#[derive(Clone)]
pub struct Identification {
    pub(crate) db: droppedneedle_kernel::db::Db,
    pub(crate) identify_store: Arc<SqliteIdentifyStore>,
    pub(crate) identify: Arc<IdentifyService>,
    pub(crate) settings: LibrarySettings,
    pub(crate) ids: Arc<dyn IdGenerator>,
    pub(crate) clock: Arc<dyn Clock>,
}

impl Identification {
    /// Queue an album for automatic management, as a settled match does.
    /// A failure only logs: the album is then managed by hand, never left
    /// half done. Blocking.
    pub fn queue_automatic_management(&self, album_id: &str) {
        super::management::automatic::queue_automatic_management(&self.db, &*self.clock, album_id);
    }

    /// Put back the identify jobs a previous run left claimed. A failure
    /// only logs: the jobs' leases run out and the queue takes them again.
    pub(crate) async fn recover(&self) {
        let identify = self.identify_store.clone();
        match tokio::task::spawn_blocking(move || identify.recover()).await {
            Ok(Ok(0)) => {}
            Ok(Ok(requeued)) => {
                tracing::info!(requeued, "identify jobs from the previous run requeued");
            }
            Ok(Err(error)) => tracing::warn!(%error, "identify jobs not requeued at startup"),
            Err(error) => tracing::warn!(%error, "identify job recovery did not run"),
        }
    }

    /// Enqueue one catalog album (a `local_albums.id`) for
    /// identification. An album the catalog does not hold is a 404; a
    /// live job on the same album revision comes back instead of a twin.
    pub fn enqueue_identify(
        &self,
        album_id: &str,
        kind: IdentifyKind,
        user_id: &str,
    ) -> Result<IdentifyJob, ServiceError> {
        if self.identify_store.album_facts(album_id)?.is_none() {
            return Err(ServiceError::NotFound);
        }
        let revision = self.identify_store.input_revision(album_id)?;
        let job_id = self.ids.new_id();
        Ok(self.identify.enqueue_album(
            &job_id,
            album_id,
            kind,
            &revision,
            Some(user_id),
            self.clock.now_ms_u64(),
            Lane::Foreground,
        )?)
    }

    /// Pending reviews for one album, oldest first.
    pub fn pending_reviews(
        &self,
        album_id: &str,
    ) -> Result<Vec<super::identify::models::ReviewItem>, ServiceError> {
        Ok(self.identify.pending_reviews(album_id)?)
    }

    /// Approve a pending review with the curator's chosen candidate,
    /// sealing a manual identity. Unknown reviews and unknown
    /// candidates are 404s; settled reviews are 409s.
    pub fn approve_review(
        &self,
        review_id: &str,
        user_id: &str,
        candidate_key: &str,
    ) -> Result<(super::identify::models::ReviewItem, Option<AlbumIdentity>), ServiceError> {
        let Some(review) = self.identify_store.get(review_id)? else {
            return Err(ServiceError::NotFound);
        };
        if review.state != super::identify::models::ReviewState::Pending {
            return Err(ServiceError::Conflict {
                message: "Review is already settled".to_owned(),
            });
        }
        if !review
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_key == candidate_key)
        {
            return Err(ServiceError::NotFound);
        }
        let approved = self
            .identify
            .approve_candidate(review_id, user_id, candidate_key)?;
        if !approved {
            return Err(ServiceError::Conflict {
                message: "Review could not be approved".to_owned(),
            });
        }
        let settled = self.identify_store.get(review_id)?.unwrap_or(review);
        let identity = self
            .identify_store
            .album_identity(&settled.local_album_id)?;
        Ok((settled, identity))
    }

    /// Reject a pending review: the album keeps its tags, nothing seals.
    pub fn reject_review(
        &self,
        review_id: &str,
        user_id: &str,
    ) -> Result<super::identify::models::ReviewItem, ServiceError> {
        let Some(review) = self.identify_store.get(review_id)? else {
            return Err(ServiceError::NotFound);
        };
        if review.state != super::identify::models::ReviewState::Pending {
            return Err(ServiceError::Conflict {
                message: "Review is already settled".to_owned(),
            });
        }
        if !self.identify.reject_candidates(review_id, user_id)? {
            return Err(ServiceError::Conflict {
                message: "Review could not be rejected".to_owned(),
            });
        }
        Ok(self.identify_store.get(review_id)?.unwrap_or(review))
    }
}
