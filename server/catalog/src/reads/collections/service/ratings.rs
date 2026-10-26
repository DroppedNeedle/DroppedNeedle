//! Star ratings: artists, albums and tracks by local id, one per user and
//! item. Subsonic apps and the web app set and read the same rows.

use std::collections::HashMap;

use super::CollectionsService;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::{
    RatingLookupBody, RatingLookupResponse, RatingStatusResponse,
};
use crate::reads::collections::store::ratings::{ItemRating, MAX_RATING, RatedKind};
use crate::reads::library::stores::StoreError as LibraryStoreError;

/// Most ids of one kind a lookup accepts.
const LOOKUP_MAX_IDS: usize = 1000;

/// Validated kind or 400.
fn rated_kind(raw: &str) -> Result<RatedKind, CollectionsError> {
    match raw {
        "album" => Ok(RatedKind::Album),
        "artist" => Ok(RatedKind::Artist),
        "track" => Ok(RatedKind::Track),
        _ => Err(CollectionsError::invalid(
            "Kind must be album, artist, or track",
        )),
    }
}

fn library_error(error: LibraryStoreError) -> CollectionsError {
    match error {
        LibraryStoreError::Busy(operation) => CollectionsError::Busy { operation },
        LibraryStoreError::Internal(cause) => CollectionsError::internal(&cause),
    }
}

impl CollectionsService<'_> {
    /// Set the caller's rating of one item, 1 to 5, or clear it with 0.
    pub async fn set_rating(
        &self,
        user_id: &str,
        kind: RatedKind,
        item_id: &str,
        rating: u8,
    ) -> Result<(), CollectionsError> {
        if rating > MAX_RATING {
            return Err(CollectionsError::invalid("Rating must be 0 to 5"));
        }
        Ok(self
            .state
            .stores
            .ratings
            .set(user_id, kind, item_id, rating)
            .await?)
    }

    /// Rate the item `item_id` names now, as Subsonic's `setRating` does:
    /// the old id of a merged album or artist, or of a track that continued
    /// into another, rates the item that holds it. An id no live album or
    /// artist, or no streamable track, holds is not found, and nothing is
    /// written. A live album without files counts: its web page keeps its
    /// rating.
    pub async fn rate(
        &self,
        user_id: &str,
        kind: &str,
        item_id: &str,
        rating: u8,
    ) -> Result<RatingStatusResponse, CollectionsError> {
        let kind = rated_kind(kind)?;
        if rating > MAX_RATING {
            return Err(CollectionsError::invalid("Rating must be 0 to 5"));
        }
        let items = self
            .state
            .items
            .as_ref()
            .ok_or_else(|| CollectionsError::internal(&"the local catalog is not wired"))?;
        let current = items
            .current_id(kind.as_str(), item_id)
            .await
            .map_err(library_error)?;
        let current = match (kind, current) {
            // `current_id` names only a live album, with files or without.
            (RatedKind::Album, Some(current)) => current,
            (RatedKind::Album, None) => return Err(CollectionsError::NotFound),
            (_, current) => {
                let current = current.unwrap_or_else(|| item_id.to_owned());
                let live = items
                    .existing(kind.as_str(), std::slice::from_ref(&current))
                    .await
                    .map_err(library_error)?;
                if !live.contains(&current) {
                    return Err(CollectionsError::NotFound);
                }
                current
            }
        };
        self.set_rating(user_id, kind, &current, rating).await?;
        Ok(RatingStatusResponse {
            kind,
            item_id: current,
            rating,
        })
    }

    /// The caller's own ratings of the given ids, only those they rated.
    /// Ids are read as given: a page holds current ids once it refreshes.
    pub async fn rating_lookup(
        &self,
        user_id: &str,
        body: &RatingLookupBody,
    ) -> Result<RatingLookupResponse, CollectionsError> {
        let lists = [&body.album_ids, &body.track_ids, &body.artist_ids];
        if lists.iter().any(|ids| ids.len() > LOOKUP_MAX_IDS) {
            return Err(CollectionsError::invalid("At most 1000 ids of each kind"));
        }
        let own = |ratings: HashMap<String, ItemRating>| -> HashMap<String, u8> {
            ratings
                .into_iter()
                .filter_map(|(id, rating)| rating.own.map(|own| (id, own)))
                .collect()
        };
        Ok(RatingLookupResponse {
            albums: own(self
                .ratings(user_id, RatedKind::Album, &body.album_ids)
                .await?),
            tracks: own(self
                .ratings(user_id, RatedKind::Track, &body.track_ids)
                .await?),
            artists: own(self
                .ratings(user_id, RatedKind::Artist, &body.artist_ids)
                .await?),
        })
    }

    /// The ratings of each of `ids` anyone has rated: the caller's own and
    /// the mean of everyone's.
    pub async fn ratings(
        &self,
        user_id: &str,
        kind: RatedKind,
        ids: &[String],
    ) -> Result<HashMap<String, ItemRating>, CollectionsError> {
        Ok(self
            .state
            .stores
            .ratings
            .ratings(user_id, kind, ids)
            .await?)
    }

    /// The ids of one kind the caller rated, highest first.
    pub async fn rated_ids(
        &self,
        user_id: &str,
        kind: RatedKind,
    ) -> Result<Vec<String>, CollectionsError> {
        Ok(self.state.stores.ratings.rated_ids(user_id, kind).await?)
    }
}
