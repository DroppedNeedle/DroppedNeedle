//! Star ratings: artists, albums and tracks by local id, one per user and
//! item. Subsonic apps set and read them.

use std::collections::HashMap;

use super::CollectionsService;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::store::ratings::{ItemRating, MAX_RATING, RatedKind};

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
