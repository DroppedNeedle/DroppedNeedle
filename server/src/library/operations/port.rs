//! The edition choice as other parts of the server reach it.
//!
//! The album page routes (catalog and collections) are built before the
//! library, so they hold this seam and wiring plugs the library's
//! [`Operations`] in once it exists. Until then every call fails closed.

use std::future::Future;
use std::pin::Pin;

use super::models::{EditionChoice, OperationError};
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
