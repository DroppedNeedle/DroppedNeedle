//! Star ratings over `library_user_ratings`.
//!
//! One row per (user, kind, item), rating 1 to 5. A Subsonic app sets
//! them; clearing a rating deletes the row. Reads give the caller's own
//! rating beside the mean of every user's.

use std::collections::HashMap;

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::{params, params_from_iter};

use super::super::db::{CollectionsDb, StoreError};
use super::super::sql;

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
                        sql::CLEAR_RATING.text(),
                        params![user_id, kind.as_str(), item_id],
                    )?;
                } else {
                    tx.execute(
                        sql::SET_RATING.text(),
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
        // Chunked to stay under SQLite's bound-parameter limit.
        for chunk in ids.chunks(500) {
            let mut binds = vec![user_id.to_owned(), kind.as_str().to_owned()];
            binds.extend_from_slice(chunk);
            let text = sql::ratings(ListLen::new(chunk.len()));
            let rows = self
                .db
                .read("ratings.ratings", move |conn| {
                    let rows = conn
                        .prepare(&text)?
                        .query_map(params_from_iter(binds), |row| {
                            let own: Option<i64> = row.get(1)?;
                            Ok((
                                row.get::<_, String>(0)?,
                                ItemRating {
                                    own: own.and_then(|own| u8::try_from(own).ok()),
                                    average: row.get(2)?,
                                },
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(rows)
                })
                .await?;
            out.extend(rows);
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
        let user_id = user_id.to_owned();
        self.db
            .read("ratings.rated_ids", move |conn| {
                let ids = conn
                    .prepare(sql::RATED_IDS.text())?
                    .query_map(params![user_id, kind.as_str()], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(ids)
            })
            .await
    }
}
