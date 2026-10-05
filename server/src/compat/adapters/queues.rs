//! Saved play queues and bookmarks over `compat_play_queues`,
//! `compat_play_queue_items` and `compat_bookmarks`.
//!
//! Both protocols share these rows per user, so a queue saved from one
//! client resumes in another, and both survive a restart (v2 parity).

use rusqlite::params;
use sqlx::Row as _;

use crate::compat::subsonic::store::{BookmarkRecord, QueueState};
use crate::reads::collections::db::{CollectionsDb, StoreError};

/// Durable per-user play queues and bookmarks.
#[derive(Debug, Clone)]
pub struct CompatQueues {
    db: CollectionsDb,
}

impl CompatQueues {
    /// Queues over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// Saved queue for a user (empty when never saved).
    pub async fn get_queue(&self, user_id: &str) -> Result<QueueState, StoreError> {
        let pool = self.db.pool()?;
        let head = sqlx::query(
            "SELECT current_index, position_ms, updated_at, changed_by_client \
             FROM compat_play_queues WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| StoreError::read("compat.queue", error))?;
        let Some(head) = head else {
            return Ok(QueueState::default());
        };
        let file_ids: Vec<String> = sqlx::query_scalar(
            "SELECT file_id FROM compat_play_queue_items WHERE user_id = ? ORDER BY item_index",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("compat.queue.items", error))?;
        let read = |error| StoreError::read("compat.queue", error);
        let current_index: Option<i64> = head.try_get("current_index").map_err(read)?;
        Ok(QueueState {
            file_ids,
            current_index: current_index.and_then(|index| usize::try_from(index).ok()),
            position_ms: head.try_get("position_ms").map_err(read)?,
            updated_at: head.try_get("updated_at").map_err(read)?,
            changed_by_client: head.try_get("changed_by_client").map_err(read)?,
        })
    }

    /// Replace a user's saved queue in one transaction.
    pub async fn replace_queue(
        &self,
        user_id: &str,
        file_ids: &[String],
        current_index: Option<usize>,
        position_ms: i64,
        changed_by_client: &str,
        now_unix: f64,
    ) -> Result<(), StoreError> {
        let (user_id, file_ids, client) = (
            user_id.to_owned(),
            file_ids.to_vec(),
            changed_by_client.to_owned(),
        );
        self.db
            .write("compat.queue.replace", move |tx| {
                tx.execute(
                    "INSERT INTO compat_play_queues (user_id, current_index, position_ms, \
                     updated_at, changed_by_client) VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (user_id) DO UPDATE SET current_index = excluded.current_index, \
                     position_ms = excluded.position_ms, updated_at = excluded.updated_at, \
                     changed_by_client = excluded.changed_by_client",
                    params![
                        user_id,
                        current_index.map(|index| index as i64),
                        position_ms,
                        now_unix,
                        client
                    ],
                )?;
                tx.execute(
                    "DELETE FROM compat_play_queue_items WHERE user_id = ?1",
                    params![user_id],
                )?;
                let mut insert = tx.prepare(
                    "INSERT INTO compat_play_queue_items (user_id, item_index, file_id) \
                     VALUES (?1, ?2, ?3)",
                )?;
                for (index, file_id) in file_ids.iter().enumerate() {
                    insert.execute(params![user_id, index as i64, file_id])?;
                }
                Ok(())
            })
            .await
    }

    /// Bookmarks for a user, most recently changed first.
    pub async fn list_bookmarks(&self, user_id: &str) -> Result<Vec<BookmarkRecord>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(
            "SELECT file_id, position_ms, comment, created_at, changed_at \
             FROM compat_bookmarks WHERE user_id = ? ORDER BY changed_at DESC, file_id",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("compat.bookmarks", error))?;
        rows.iter()
            .map(|row| {
                Ok(BookmarkRecord {
                    file_id: row.try_get("file_id")?,
                    position_ms: row.try_get("position_ms")?,
                    comment: row.try_get("comment")?,
                    created_at: row.try_get::<f64, _>("created_at")? as i64,
                    changed_at: row.try_get::<f64, _>("changed_at")? as i64,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(|error| StoreError::read("compat.bookmarks", error))
    }

    /// Create or update a bookmark.
    pub async fn upsert_bookmark(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: i64,
        comment: &str,
        now_unix: f64,
    ) -> Result<(), StoreError> {
        let (user_id, file_id, comment) =
            (user_id.to_owned(), file_id.to_owned(), comment.to_owned());
        self.db
            .write("compat.bookmark.upsert", move |tx| {
                tx.execute(
                    "INSERT INTO compat_bookmarks (user_id, file_id, position_ms, comment, \
                     created_at, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
                     ON CONFLICT (user_id, file_id) DO UPDATE SET \
                     position_ms = excluded.position_ms, comment = excluded.comment, \
                     changed_at = excluded.changed_at",
                    params![user_id, file_id, position_ms, comment, now_unix],
                )?;
                Ok(())
            })
            .await
    }

    /// Delete a bookmark.
    pub async fn delete_bookmark(&self, user_id: &str, file_id: &str) -> Result<(), StoreError> {
        let (user_id, file_id) = (user_id.to_owned(), file_id.to_owned());
        self.db
            .write("compat.bookmark.delete", move |tx| {
                tx.execute(
                    "DELETE FROM compat_bookmarks WHERE user_id = ?1 AND file_id = ?2",
                    params![user_id, file_id],
                )?;
                Ok(())
            })
            .await
    }
}
