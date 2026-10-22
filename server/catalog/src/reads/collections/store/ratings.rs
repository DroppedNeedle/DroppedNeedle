//! Star ratings over `library_user_ratings`.
//!
//! One row per (user, kind, item), rating 1 to 5. A Subsonic app sets
//! them; clearing a rating deletes the row. Reads give the caller's own
//! rating beside the mean of every user's.

use std::collections::HashMap;

use rusqlite::params;
use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError, placeholders};

/// What a rating is on: the table's `item_kind` values.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RatedKind {
    /// A local artist.
    Artist,
    /// A local album.
    Album,
    /// A local track.
    Track,
}

impl RatedKind {
    /// The column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Track => "track",
        }
    }
}

/// The highest rating; 0 clears one.
pub const MAX_RATING: u8 = 5;

/// One item's ratings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemRating {
    /// The caller's rating, None when they have not rated it.
    pub own: Option<u8>,
    /// The mean of every user's rating.
    pub average: f64,
}

/// The ratings store.
#[derive(Clone, Debug)]
pub struct RatingStore {
    db: CollectionsDb,
}

impl RatingStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// Set the user's rating of one item, or clear it with 0. Callers keep
    /// `rating` at most [`MAX_RATING`].
    pub async fn set(
        &self,
        user_id: &str,
        kind: RatedKind,
        item_id: &str,
        rating: u8,
    ) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        let item_id = item_id.to_owned();
        let now_ms = self.db.now_ms();
        self.db
            .write("ratings.set", move |tx| {
                if rating == 0 {
                    tx.execute(
                        "DELETE FROM library_user_ratings \
                         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                        params![user_id, kind.as_str(), item_id],
                    )?;
                } else {
                    tx.execute(
                        "INSERT INTO library_user_ratings \
                         (user_id, item_kind, item_id, rating, rated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT (user_id, item_kind, item_id) DO UPDATE SET \
                         rating = excluded.rating, rated_at = excluded.rated_at",
                        params![user_id, kind.as_str(), item_id, rating, now_ms],
                    )?;
                }
                Ok(())
            })
            .await
    }

    /// The ratings of each of `ids` that anyone has rated, with the
    /// user's own, in one read per 500 ids.
    pub async fn ratings(
        &self,
        user_id: &str,
        kind: RatedKind,
        ids: &[String],
    ) -> Result<HashMap<String, ItemRating>, StoreError> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let pool = self.db.pool()?;
        // Chunked to stay under SQLite's bound-parameter limit.
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT item_id, MAX(CASE WHEN user_id = ? THEN rating END) AS own, \
                 AVG(rating) AS average FROM library_user_ratings \
                 WHERE item_kind = ? AND item_id IN ({}) GROUP BY item_id",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query(&sql).bind(user_id).bind(kind.as_str());
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(|error| StoreError::read("ratings.ratings", error))?;
            for row in &rows {
                let read = || -> Result<(String, ItemRating), sqlx::Error> {
                    let own: Option<i64> = row.try_get("own")?;
                    Ok((
                        row.try_get("item_id")?,
                        ItemRating {
                            own: own.and_then(|own| u8::try_from(own).ok()),
                            average: row.try_get("average")?,
                        },
                    ))
                };
                let (id, rating) =
                    read().map_err(|error| StoreError::read("ratings.ratings", error))?;
                out.insert(id, rating);
            }
        }
        Ok(out)
    }

    /// The ids of one kind the user rated, highest first, then the most
    /// recently rated.
    pub async fn rated_ids(
        &self,
        user_id: &str,
        kind: RatedKind,
    ) -> Result<Vec<String>, StoreError> {
        let pool = self.db.pool()?;
        sqlx::query_scalar(
            "SELECT item_id FROM library_user_ratings WHERE user_id = ? AND item_kind = ? \
             ORDER BY rating DESC, rated_at DESC, item_id",
        )
        .bind(user_id)
        .bind(kind.as_str())
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("ratings.rated_ids", error))
    }
}
