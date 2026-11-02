//! Favorites over `library_user_favorites`.
//!
//! One row per (user, kind, item). A heart in the web UI, a Subsonic star
//! and a Jellyfin favorite all write the same row, so a favorite shows
//! everywhere (v2 `FavoritesService`). Library reads join the same table
//! for their favorite flags.

use std::collections::HashMap;

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::{params, params_from_iter};

use super::super::db::{CollectionsDb, StoreError};
use super::super::sql;

/// Kinds the table accepts (its CHECK constraint).
pub const KINDS: [&str; 3] = ["album", "artist", "track"];

/// One favorite row.
#[derive(Debug, Clone, PartialEq)]
pub struct FavoriteRow {
    /// Item id (a local library id).
    pub item_id: String,
    /// Display name saved with the favorite, if any.
    pub name: Option<String>,
    /// When it was favorited, fractional epoch seconds.
    pub created_at: f64,
}

/// The favorites store.
#[derive(Clone, Debug)]
pub struct FavoriteStore {
    db: CollectionsDb,
}

impl FavoriteStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// A user's favorites, optionally of one kind, newest first.
    pub async fn list(
        &self,
        user_id: &str,
        kind: Option<&str>,
    ) -> Result<Vec<FavoriteRow>, StoreError> {
        let user_id = user_id.to_owned();
        let kind = kind.map(str::to_owned);
        self.db
            .read("favorites.list", move |conn| {
                let rows = conn
                    .prepare(sql::FAVORITES.text())?
                    .query_map(params![user_id, kind], |row| {
                        let row = sql::FavoriteColumns::read(row)?;
                        Ok(FavoriteRow {
                            item_id: row.item_id,
                            name: row.display_name,
                            created_at: row.created_at,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// When each of `ids` was favorited, for the ones that are.
    pub async fn starred_at(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, f64>, StoreError> {
        let mut out = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        // Chunked to stay under SQLite's bound-parameter limit.
        for chunk in ids.chunks(500) {
            let mut binds = vec![user_id.to_owned(), kind.to_owned()];
            binds.extend_from_slice(chunk);
            let text = sql::starred_at(ListLen::new(chunk.len()));
            let rows = self
                .db
                .read("favorites.starred_at", move |conn| {
                    let rows = conn
                        .prepare(&text)?
                        .query_map(params_from_iter(binds), |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(rows)
                })
                .await?;
            out.extend(rows);
        }
        Ok(out)
    }

    /// Favorite or unfavorite items in one transaction. Favoriting keeps
    /// an existing row's date; a new name replaces the saved one.
    pub async fn apply(
        &self,
        user_id: &str,
        targets: &[(String, String, Option<String>)],
        add: bool,
    ) -> Result<(), StoreError> {
        if targets.is_empty() {
            return Ok(());
        }
        let user_id = user_id.to_owned();
        let targets = targets.to_vec();
        let now = self.db.now_real();
        self.db
            .write("favorites.apply", move |tx| {
                if add {
                    let mut favorite = tx.prepare(sql::ADD_FAVORITE.text())?;
                    let mut named = tx.prepare(sql::NAME_FAVORITE.text())?;
                    for (kind, id, name) in &targets {
                        favorite.execute(params![user_id, kind, id, now])?;
                        if let Some(name) = name {
                            named.execute(params![user_id, kind, id, name])?;
                        }
                    }
                } else {
                    // Names go first: the cascade needs foreign keys on,
                    // and this keeps the rows clean either way.
                    let mut unnamed = tx.prepare(sql::UNNAME_FAVORITE.text())?;
                    let mut stmt = tx.prepare(sql::REMOVE_FAVORITE.text())?;
                    for (kind, id, _) in &targets {
                        unnamed.execute(params![user_id, kind, id])?;
                        stmt.execute(params![user_id, kind, id])?;
                    }
                }
                Ok(())
            })
            .await
    }
}
