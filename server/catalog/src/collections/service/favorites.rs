//! Favorites: albums, artists and tracks by local id, strictly per user.
//! The same rows back Subsonic stars and Jellyfin favorites.

use std::collections::HashMap;

use super::CollectionsService;
use crate::collections::error::CollectionsError;
use crate::collections::models::FavoriteStatusResponse;
use crate::collections::store::favorites::FavoriteKind;
use crate::local::stores::StoreError as LibraryStoreError;

fn library_error(error: LibraryStoreError) -> CollectionsError {
    match error {
        LibraryStoreError::Busy(operation) => CollectionsError::Busy { operation },
        LibraryStoreError::Internal(cause) => CollectionsError::internal(&cause),
    }
}

/// Validated kind or 400.
fn clean_kind(raw: &str) -> Result<&'static str, CollectionsError> {
    FavoriteKind::parse(raw)
        .map(FavoriteKind::as_str)
        .ok_or_else(|| CollectionsError::invalid("Kind must be album, artist, or track"))
}

impl CollectionsService<'_> {
    /// Favorite or unfavorite the item `item_id` names now, as Subsonic's
    /// `star` does: the old id of a merged album or artist, or of a track
    /// that continued into another, acts on the item that holds it. An id
    /// no live album or artist, or no streamable track, holds is not found,
    /// and nothing is written. Idempotent both ways.
    pub async fn set_favorite(
        &self,
        user_id: &str,
        kind: &str,
        item_id: &str,
        favorited: bool,
        name: Option<String>,
    ) -> Result<FavoriteStatusResponse, CollectionsError> {
        let kind = clean_kind(kind)?;
        let current = self.live_id(kind, item_id).await?;
        self.set_favorites(
            user_id,
            &[(kind.to_owned(), current.clone(), name)],
            favorited,
        )
        .await?;
        Ok(FavoriteStatusResponse {
            kind: kind.to_owned(),
            item_id: current,
            favorited,
        })
    }

    /// The id of the live album, artist or streamable track `item_id` names
    /// now, or not found.
    async fn live_id(&self, kind: &str, item_id: &str) -> Result<String, CollectionsError> {
        let items = &self.state.items;
        let current = items
            .current_id(kind, item_id)
            .await
            .map_err(library_error)?;
        // `current_id` names only a live album; artists and tracks are
        // checked below.
        let current = match (kind, current) {
            ("album", Some(current)) => return Ok(current),
            ("album", None) => return Err(CollectionsError::NotFound),
            (_, current) => current.unwrap_or_else(|| item_id.to_owned()),
        };
        let live = items
            .existing(kind, std::slice::from_ref(&current))
            .await
            .map_err(library_error)?;
        if live.contains(&current) {
            Ok(current)
        } else {
            Err(CollectionsError::NotFound)
        }
    }

    /// Favorite or unfavorite several `(kind, id, name)` items at once.
    pub async fn set_favorites(
        &self,
        user_id: &str,
        targets: &[(String, String, Option<String>)],
        favorited: bool,
    ) -> Result<(), CollectionsError> {
        for (kind, _, _) in targets {
            clean_kind(kind)?;
        }
        Ok(self
            .state
            .stores
            .favorites
            .apply(user_id, targets, favorited)
            .await?)
    }

    /// The caller's favorites of one kind as `(id, epoch seconds)`, newest
    /// first.
    pub async fn favorite_ids(
        &self,
        user_id: &str,
        kind: &str,
    ) -> Result<Vec<(String, i64)>, CollectionsError> {
        let kind = clean_kind(kind)?;
        Ok(self
            .state
            .stores
            .favorites
            .list(user_id, Some(kind))
            .await?
            .into_iter()
            .map(|row| (row.item_id, row.created_at as i64))
            .collect())
    }

    /// When each of `ids` was favorited, for the ones that are.
    pub async fn favorited_at(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, i64>, CollectionsError> {
        let kind = clean_kind(kind)?;
        Ok(self
            .state
            .stores
            .favorites
            .starred_at(user_id, kind, ids)
            .await?
            .into_iter()
            .map(|(id, at)| (id, at as i64))
            .collect())
    }
}
