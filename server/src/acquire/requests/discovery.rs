//! The requests side of discovery batches: discover's "download this
//! whole shelf" files its albums here, through the same intake, quota,
//! approval and edition rules as any album request, and a batch removal
//! withdraws the owner's asks or (admins only) recycles what landed.

use futures_util::future::BoxFuture;

use super::auth::{Principal, Role};
use super::error::{QUOTA_EXCEEDED, RequestsError, STORAGE_FULL};
use super::models::{BatchIntake, BatchItem, RequestKind};
use super::service::RequestsService;
use super::{RequestsState, ledger::STATUS_FAILED};
use crate::library::mutations::{MutationError, Mutations};
use crate::library::wiring::LibrarySetup;
use crate::reads::discover::adapters::batches::{BatchAlbum, BatchCaller, BatchRequests};
use crate::reads::discover::ports::{BatchFailure, BatchRefusal, ProviderFailure};
use droppedneedle_auth::users::roles::Role as UserRole;

/// Discovery batches over the request ledger and the library.
pub struct DiscoveryBatchRequests {
    requests: RequestsState,
    library: Option<LibrarySetup>,
}

impl DiscoveryBatchRequests {
    /// File through `requests`; recycle landed albums through `library`
    /// (`None` leaves landed albums in place and counts them as kept).
    pub fn new(requests: RequestsState, library: Option<LibrarySetup>) -> Self {
        Self { requests, library }
    }
}

fn principal(caller: &BatchCaller) -> Principal {
    Principal {
        user_id: caller.user_id.clone(),
        username: caller.username.clone(),
        role: match caller.role {
            UserRole::User => Role::User,
            UserRole::Trusted => Role::Trusted,
            UserRole::Admin => Role::Admin,
        },
    }
}

/// A requests error as a batch failure: refusals keep their status, code
/// and details so the page shows what a single request would.
fn batch_failure(error: RequestsError) -> BatchFailure {
    match error {
        RequestsError::QuotaExceeded { message, details } => BatchFailure::Refused(BatchRefusal {
            status: 429,
            code: QUOTA_EXCEEDED.to_owned(),
            message,
            details: Some(details),
        }),
        RequestsError::StorageFull { message, details } => BatchFailure::Refused(BatchRefusal {
            status: 403,
            code: STORAGE_FULL.to_owned(),
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
        caller: &'a BatchCaller,
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
                .request_batch(&principal(caller), &body)
                .await
                .map_err(batch_failure)?;
            if response.status == STATUS_FAILED && response.requested == 0 {
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
            let owner = Principal {
                user_id: owner_id.to_owned(),
                username: None,
                role: Role::User,
            };
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
        caller: &'a BatchCaller,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            if caller.role != UserRole::Admin {
                return Err("only admins recycle batch albums".to_owned());
            }
            let Some(library) = self.library.clone() else {
                return Ok(false);
            };
            let hook = library.removal_hook.get().cloned();
            let (album, actor_id) = (release_group_mbid.to_owned(), caller.user_id.clone());
            let removed = tokio::task::spawn_blocking(move || {
                Mutations::new(&library).remove_album(&album, true, &actor_id)
            })
            .await
            .map_err(|error| format!("batch album removal did not finish: {error}"))?;
            match removed {
                Ok(removed) => {
                    // The removal stands; download and wanted cleanup follow it.
                    if let (Some(hook), Some(group)) = (hook, removed.release_group_mbid.as_deref())
                    {
                        hook.album_removed(group, true).await;
                    }
                    Ok(true)
                }
                Err(MutationError::NotFound(_)) => Ok(false),
                Err(error) => Err(format!("batch album removal: {error:?}")),
            }
        })
    }
}
