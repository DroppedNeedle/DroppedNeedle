//! The edition choice as other parts of the server reach it.
//!
//! The album page routes (catalog and collections) are built before the
//! library, so they hold this seam and wiring plugs the library's
//! [`Operations`] in once it exists. Until then every call fails closed.

use std::future::Future;
use std::pin::Pin;

use super::models::{EditionChoice, OperationError};
use super::plan::EditionPlan;
use super::service::Operations;

/// Boxed future for dyn-compatible methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Choose an album's edition, or hand it back to automatic choice.
pub trait EditionChoices: Send + Sync {
    /// Make `release_mbid` the album's chosen edition.
    fn choose<'a>(
        &'a self,
        album_id: &'a str,
        release_mbid: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<EditionChoice, OperationError>>;

    /// "Let DroppedNeedle choose": back to automatic best fit.
    fn hand_back<'a>(
        &'a self,
        album_id: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), OperationError>>;
}

/// No library wired: every choice fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEditionChoices;

impl EditionChoices for NoEditionChoices {
    fn choose<'a>(
        &'a self,
        _album_id: &'a str,
        _release_mbid: &'a str,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<EditionChoice, OperationError>> {
        Box::pin(async { Err(OperationError::Store("the library is not wired".to_owned())) })
    }

    fn hand_back<'a>(
        &'a self,
        _album_id: &'a str,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), OperationError>> {
        Box::pin(async { Err(OperationError::Store("the library is not wired".to_owned())) })
    }
}

impl EditionChoices for Operations {
    fn choose<'a>(
        &'a self,
        album_id: &'a str,
        release_mbid: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<EditionChoice, OperationError>> {
        Box::pin(self.choose_edition(album_id, release_mbid, Some(user_id)))
    }

    fn hand_back<'a>(
        &'a self,
        album_id: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), OperationError>> {
        let (ops, album, user) = (self.clone(), album_id.to_owned(), user_id.to_owned());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || ops.hand_back_edition(&album, &user))
                .await
                .map_err(|error| OperationError::Store(error.to_string()))?
        })
    }
}

/// The plan for an album's chosen edition, looked up by release group:
/// the seam "get missing tracks" uses to fetch only what the edition
/// lacks. Acquisition is built after the library, so wiring plugs the
/// library's [`Operations`] in once both exist.
pub trait EditionPlans: Send + Sync {
    /// The plan for the library's copy of the release group (a chosen
    /// copy first, then the oldest), or `None` when the library holds no
    /// copy with an edition.
    fn group_plan<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<EditionPlan>, OperationError>>;
}

impl EditionPlans for Operations {
    fn group_plan<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<EditionPlan>, OperationError>> {
        Box::pin(async move {
            let Some(album_id) = self.album_for_group(release_group_mbid).await? else {
                return Ok(None);
            };
            self.edition_plan(&album_id, None).await.map(Some)
        })
    }
}
