//! The requests side of discovery batches: discover's "download this
//! whole shelf" files its albums here, through the same intake, quota,
//! approval and edition rules as any album request, and a batch removal
//! withdraws the owner's asks or (admins only) recycles what landed.

use futures_util::future::BoxFuture;

use super::error::{QUOTA_EXCEEDED, RequestsError, STORAGE_FULL};
use super::models::{BatchIntake, BatchItem, RequestKind};
use super::service::RequestsService;
use super::{RequestsState, ledger::RequestStatus};
use droppedneedle_auth::{Admin, Principal, Role};
use droppedneedle_catalog::reads::discover::batches::{BatchAlbum, BatchRequests};
use droppedneedle_catalog::reads::discover::ports::{BatchFailure, BatchRefusal, ProviderFailure};
use droppedneedle_library::mutations::{MutationError, Mutations};

/// Discovery batches over the request ledger and the library.
pub struct DiscoveryBatchRequests {
    requests: RequestsState,
    library: Option<Mutations>,
}

impl DiscoveryBatchRequests {
    /// File through `requests`; recycle landed albums through `library`
    /// (`None` leaves landed albums in place and counts them as kept).
    pub fn new(requests: RequestsState, library: Option<Mutations>) -> Self {
        Self { requests, library }
    }
}

/// A requests error as a batch failure: refusals keep their status, code
/// and details so the page shows what a single request would.
fn batch_failure(error: RequestsError) -> BatchFailure {
    match error {
        RequestsError::QuotaExceeded { message, details } => BatchFailure::Refused(BatchRefusal {
            status: 429,
            code: QUOTA_EXCEEDED,
            message,
            details: Some(details),
        }),
        RequestsError::StorageFull { message, details } => BatchFailure::Refused(BatchRefusal {
            status: 403,
            code: STORAGE_FULL,
            message,
            details: Some(details),
        }),
        RequestsError::InvalidInput { message } | RequestsError::Conflict { message } => {
            BatchFailure::Invalid(message)
        }
        other => BatchFailure::Provider(ProviderFailure::failed(format!(
            "batch request filing: {other:?}"
        ))),
    }
}

impl BatchRequests for DiscoveryBatchRequests {
    fn request_albums<'a>(
        &'a self,
        caller: &'a Principal,
        albums: Vec<BatchAlbum>,
    ) -> BoxFuture<'a, Result<(), BatchFailure>> {
        Box::pin(async move {
            let body = BatchIntake {
                items: albums
                    .into_iter()
                    .map(|album| BatchItem {
                        musicbrainz_id: album.release_group_mbid,
                        artist_name: Some(album.artist_name),
                        album_title: Some(album.album_title),
                        year: None,
                        artist_mbid: album.artist_mbid,
                    })
                    .collect(),
                monitor_artist: false,
                auto_download_artist: false,
            };
            let response = RequestsService::new(&self.requests)
                .request_batch(caller, &body)
                .await
                .map_err(batch_failure)?;
            if response.status == RequestStatus::Failed.as_str() && response.requested == 0 {
                return Err(BatchFailure::Invalid(
                    "None of these albums could be requested: each one needs an artist and an \
                     album name."
                        .to_owned(),
                ));
            }
            Ok(())
        })
    }

    fn withdraw_album<'a>(
        &'a self,
        owner_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            // Act as the owner with plain rights, whoever removes the
            // batch: the owner detaches from a shared request and only a
            // request that is theirs alone is cancelled.
            let owner = Principal::acting_for(owner_id, Role::User, None);
            let ids = [release_group_mbid.to_owned()];
            RequestsService::new(&self.requests)
                .cancel_batch(&owner, &ids, RequestKind::Album)
                .await
                .map(|outcome| outcome.cancelled > 0)
                .map_err(|error| format!("batch withdraw: {error:?}"))
        })
    }

    fn remove_album<'a>(
        &'a self,
        admin: &'a Admin,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let Some(library) = self.library.clone() else {
                return Ok(false);
            };
            let (album, actor_id) = (release_group_mbid.to_owned(), admin.user_id().to_owned());
            let removed = tokio::task::spawn_blocking(move || {
                library.remove_album(&album, true, true, &actor_id)
            })
            .await
            .map_err(|error| format!("batch album removal did not finish: {error}"))?;
            match removed {
                Ok(_) => Ok(true),
                Err(MutationError::NotFound(_)) => Ok(false),
                Err(error) => Err(format!("batch album removal: {error:?}")),
            }
        })
    }
}
