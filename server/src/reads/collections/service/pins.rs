//! The per-album edition route (`/library/albums/{id}/edition-pin`).
//!
//! The album's identity row is its edition. Setting a "pin" chooses that
//! edition through the library's one edition operation (any release, even
//! one of another release group); clearing it hands the album back to
//! automatic best fit. Setting and clearing are curator-gated (v2);
//! reading is open to any authenticated user.

use super::CollectionsService;
use crate::library::operations::models::OperationError;
use crate::reads::collections::auth::Principal;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::EditionPinResponse;

/// An edition operation's refusal as a collections error, keeping its
/// sentence and what to do about it.
fn refused(error: OperationError) -> CollectionsError {
    let said = |reason: crate::library::operations::reasons::Reason| {
        format!("{} {}", reason.message, reason.action)
    };
    match error {
        OperationError::NotFound(_) => CollectionsError::NotFound,
        OperationError::Invalid(reason) => CollectionsError::invalid(&said(reason)),
        OperationError::Conflict(reason) => CollectionsError::Conflict {
            message: said(reason),
        },
        OperationError::Unavailable(cause) => CollectionsError::Conflict {
            message: format!(
                "MusicBrainz is not answering right now ({cause}). Try again in a few minutes."
            ),
        },
        OperationError::Store(cause) => CollectionsError::internal(&cause),
    }
}

impl CollectionsService<'_> {
    /// The album's edition and whether a person chose it.
    pub async fn edition_pin(
        &self,
        album_id: &str,
    ) -> Result<EditionPinResponse, CollectionsError> {
        let album = self
            .state
            .stores
            .pins
            .album(album_id)
            .await?
            .ok_or(CollectionsError::NotFound)?;
        let hint_source = match (&album.release_mbid, album.chosen) {
            (Some(_), true) => "pin",
            (Some(_), false) => "default",
            (None, _) => "none",
        };
        Ok(EditionPinResponse {
            album_id: album_id.to_owned(),
            pinned_release_mbid: album.release_mbid.clone().filter(|_| album.chosen),
            selected_release_mbid: album.release_mbid,
            hint_source: hint_source.to_owned(),
        })
    }

    /// Choose the album's edition. Curator only.
    pub async fn set_edition_pin(
        &self,
        caller: &Principal,
        album_id: &str,
        release_mbid: &str,
    ) -> Result<EditionPinResponse, CollectionsError> {
        caller.require_curator()?;
        self.state
            .editions
            .choose(album_id, release_mbid, &caller.user_id)
            .await
            .map_err(refused)?;
        self.edition_pin(album_id).await
    }

    /// Hand the album's edition back to automatic choice. Curator only.
    pub async fn clear_edition_pin(
        &self,
        caller: &Principal,
        album_id: &str,
    ) -> Result<EditionPinResponse, CollectionsError> {
        caller.require_curator()?;
        self.state
            .stores
            .pins
            .album(album_id)
            .await?
            .ok_or(CollectionsError::NotFound)?;
        self.state
            .editions
            .hand_back(album_id, &caller.user_id)
            .await
            .map_err(refused)?;
        self.edition_pin(album_id).await
    }
}
