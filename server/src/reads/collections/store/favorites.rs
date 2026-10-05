//! Favorites over `library_user_favorites`.
//!
//! One row per (user, kind, item). A heart in the web UI, a Subsonic star
//! and a Jellyfin favorite all write the same row, so a favorite shows
//! everywhere (v2 `FavoritesService`). Library reads join the same table
//! for their favorite flags.

use std::collections::HashMap;

use rusqlite::params;
use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError, now_real, placeholders};

/// Kinds the table accepts (its CHECK constraint).
pub const KINDS: [&str; 3] = ["album", "artist", "track"];

/// One favorite row.
#[derive(Debug, Clone, PartialEq)]
pub struct FavoriteRow {
    /// Item kind.
    pub kind: String,
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
        let pool = self.db.pool()?;
        let rows = sqlx::query(
            "SELECT item_kind, item_id, display_name, created_at FROM library_user_favorites \
             WHERE user_id = ? AND (? IS NULL OR item_kind = ?) \
             ORDER BY created_at DESC, item_kind, item_id",
        )
        .bind(user_id)
        .bind(kind)
        .bind(kind)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("favorites.list", error))?;
        rows.iter()
            .map(|row| {
                Ok(FavoriteRow {
                    kind: row.try_get("item_kind")?,
                    item_id: row.try_get("item_id")?,
                    name: row.try_get("display_name")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(|error| StoreError::read("favorites.list", error))
    }

    /// Per-kind counts for one user.
    pub async fn counts(&self, user_id: &str) -> Result<HashMap<String, usize>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(
            "SELECT item_kind, COUNT(*) AS total FROM library_user_favorites \
             WHERE user_id = ? GROUP BY item_kind",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("favorites.counts", error))?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get::<String, _>("item_kind")?,
                    row.try_get::<i64, _>("total")?.max(0) as usize,
                ))
            })
            .collect::<Result<HashMap<_, _>, sqlx::Error>>()
            .map_err(|error| StoreError::read("favorites.counts", error))
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
        let pool = self.db.pool()?;
        // Chunked to stay under SQLite's bound-parameter limit.
        for chunk in ids.chunks(500) {
            let sql = format!(
                "SELECT item_id, created_at FROM library_user_favorites \
                 WHERE user_id = ? AND item_kind = ? AND item_id IN ({})",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query(&sql).bind(user_id).bind(kind);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(|error| StoreError::read("favorites.starred_at", error))?;
            for row in &rows {
                let id: String = row
                    .try_get("item_id")
                    .map_err(|error| StoreError::read("favorites.starred_at", error))?;
                let at: f64 = row
                    .try_get("created_at")
                    .map_err(|error| StoreError::read("favorites.starred_at", error))?;
                out.insert(id, at);
            }
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
        self.db
            .write("favorites.apply", move |tx| {
                let now = now_real();
                if add {
                    let mut stmt = tx.prepare(
                        "INSERT INTO library_user_favorites \
                         (user_id, item_kind, item_id, created_at, display_name) \
                         VALUES (?1, ?2, ?3, ?4, ?5) \
                         ON CONFLICT (user_id, item_kind, item_id) DO UPDATE SET \
                         display_name = COALESCE(excluded.display_name, display_name)",
                    )?;
                    for (kind, id, name) in &targets {
                        stmt.execute(params![user_id, kind, id, now, name])?;
                    }
                } else {
                    let mut stmt = tx.prepare(
                        "DELETE FROM library_user_favorites \
                         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3",
                    )?;
                    for (kind, id, _) in &targets {
                        stmt.execute(params![user_id, kind, id])?;
                    }
                }
                Ok(())
            })
            .await
    }
}
