//! Favorites: albums, artists and tracks by local id, strictly per user.
//! The same rows back Subsonic stars and Jellyfin favorites.

use std::collections::HashMap;

use super::CollectionsService;
use crate::reads::collections::db::epoch_from_real;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::{
    FavoriteCounts, FavoriteItem, FavoriteListResponse, FavoriteStatusResponse,
};
use crate::reads::collections::store::favorites::KINDS;

/// Validated kind or 400.
fn clean_kind(raw: &str) -> Result<&'static str, CollectionsError> {
    KINDS
        .iter()
        .find(|kind| **kind == raw)
        .copied()
        .ok_or_else(|| CollectionsError::invalid("Kind must be album, artist, or track"))
}

impl CollectionsService<'_> {
    /// The caller's favorites, optionally of one kind. Counts ignore the
    /// filter so one call fills every tab.
    pub async fn list_favorites(
        &self,
        user_id: &str,
        kind: Option<&str>,
    ) -> Result<FavoriteListResponse, CollectionsError> {
        let kind = kind.map(clean_kind).transpose()?;
        let stores = &self.state.stores;
        let items = stores
            .favorites
            .list(user_id, kind)
            .await?
            .into_iter()
            .map(|row| FavoriteItem {
                kind: row.kind,
                item_id: row.item_id,
                name: row.name,
                favorited_at: epoch_from_real(row.created_at),
            })
            .collect();
        let counts = stores.favorites.counts(user_id).await?;
        let count = |kind: &str| counts.get(kind).copied().unwrap_or(0);
        Ok(FavoriteListResponse {
            items,
            counts: FavoriteCounts {
                album: count("album"),
                artist: count("artist"),
                track: count("track"),
            },
        })
    }

    /// Favorite or unfavorite one item. Idempotent both ways.
    pub async fn set_favorite(
        &self,
        user_id: &str,
        kind: &str,
        item_id: &str,
        favorited: bool,
        name: Option<String>,
    ) -> Result<FavoriteStatusResponse, CollectionsError> {
        let kind = clean_kind(kind)?;
        self.set_favorites(
            user_id,
            &[(kind.to_owned(), item_id.to_owned(), name)],
            favorited,
        )
        .await?;
        Ok(FavoriteStatusResponse {
            kind: kind.to_owned(),
            item_id: item_id.to_owned(),
            favorited,
        })
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
