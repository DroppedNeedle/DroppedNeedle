//! Edition pins: display lane only.
//!
//! A pin steers the soft display hint (`selected_release_mbid`): the pinned
//! release when set, else the album's own release. It never becomes catalog
//! identity; the pin store cannot write identity rows. Setting and clearing
//! are curator-gated (v2); reading is open to any authenticated user.
//! A pin must name a release of the album's release group that the library
//! knows; an album without a MusicBrainz identity cannot be pinned (404).

use super::CollectionsService;
use crate::reads::collections::auth::Principal;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::EditionPinResponse;

impl CollectionsService<'_> {
    /// The pin display for one album.
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
        let (selected, hint_source) = match (&album.pinned_release_mbid, &album.release_mbid) {
            (Some(pinned), _) => (Some(pinned.clone()), "pin"),
            (None, Some(own)) => (Some(own.clone()), "default"),
            (None, None) => (None, "none"),
        };
        Ok(EditionPinResponse {
            album_id: album_id.to_owned(),
            pinned_release_mbid: album.pinned_release_mbid,
            selected_release_mbid: selected,
            hint_source: hint_source.to_owned(),
        })
    }

    /// Pin one edition. Curator only.
    pub async fn set_edition_pin(
        &self,
        caller: &Principal,
        album_id: &str,
        release_mbid: &str,
    ) -> Result<EditionPinResponse, CollectionsError> {
        caller.require_curator()?;
        let pins = &self.state.stores.pins;
        let album = pins
            .album(album_id)
            .await?
            .ok_or(CollectionsError::NotFound)?;
        let group = album.release_group_mbid.ok_or(CollectionsError::NotFound)?;
        let known = pins.known_editions(&group).await?;
        if !known
            .iter()
            .any(|edition| edition.eq_ignore_ascii_case(release_mbid))
        {
            return Err(CollectionsError::invalid("Unknown edition for this album"));
        }
        pins.set(album_id, &group, release_mbid, &caller.user_id)
            .await?;
        self.edition_pin(album_id).await
    }

    /// Clear the pin. Curator only; clearing an unpinned album is a no-op.
    pub async fn clear_edition_pin(
        &self,
        caller: &Principal,
        album_id: &str,
    ) -> Result<EditionPinResponse, CollectionsError> {
        caller.require_curator()?;
        let pins = &self.state.stores.pins;
        pins.album(album_id)
            .await?
            .ok_or(CollectionsError::NotFound)?;
        pins.clear(album_id).await?;
        self.edition_pin(album_id).await
    }
}
