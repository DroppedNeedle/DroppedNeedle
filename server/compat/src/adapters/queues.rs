//! Saved play queues and bookmarks over `compat_play_queues`,
//! `compat_play_queue_items` and `compat_bookmarks`.
//!
//! Both protocols share these rows per user, so a queue saved from one
//! client resumes in another, and both survive a restart (v2 parity).

use rusqlite::{OptionalExtension as _, params};

use super::sql;
use crate::subsonic::store::{BookmarkRecord, QueueState};
use droppedneedle_catalog::collections::db::{CollectionsDb, StoreError};

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

    /// Saved queue for a user (empty when never saved). The head and its
    /// items are one answer, so they share one read.
    pub async fn get_queue(&self, user_id: &str) -> Result<QueueState, StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .read("compat.queue", move |conn| {
                let head = conn
                    .query_row(sql::QUEUE_HEAD.text(), [&user_id], sql::QueueHead::read)
                    .optional()?;
                let Some(head) = head else {
                    return Ok(QueueState::default());
                };
                let file_ids = conn
                    .prepare(sql::QUEUE_ITEMS.text())?
                    .query_map([&user_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(QueueState {
                    file_ids,
                    current_index: head
                        .current_index
                        .and_then(|index| usize::try_from(index).ok()),
                    position_ms: head.position_ms,
                    updated_at: head.updated_at,
                    changed_by_client: head.changed_by_client,
                })
            })
            .await
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
                    sql::PUT_QUEUE.text(),
                    params![
                        user_id,
                        current_index.map(|index| index as i64),
                        position_ms,
                        now_unix,
                        client
                    ],
                )?;
                tx.execute(sql::CLEAR_QUEUE_ITEMS.text(), params![user_id])?;
                let mut insert = tx.prepare(sql::ADD_QUEUE_ITEM.text())?;
                for (index, file_id) in file_ids.iter().enumerate() {
                    insert.execute(params![user_id, index as i64, file_id])?;
                }
                Ok(())
            })
            .await
    }

    /// Bookmarks for a user, most recently changed first.
    pub async fn list_bookmarks(&self, user_id: &str) -> Result<Vec<BookmarkRecord>, StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .read("compat.bookmarks", move |conn| {
                let rows = conn
                    .prepare(sql::BOOKMARKS.text())?
                    .query_map([&user_id], |row| {
                        let row = sql::BookmarkColumns::read(row)?;
                        Ok(BookmarkRecord {
                            file_id: row.file_id,
                            position_ms: row.position_ms,
                            comment: row.comment,
                            created_at: row.created_at as i64,
                            changed_at: row.changed_at as i64,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
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
                    sql::PUT_BOOKMARK.text(),
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
                tx.execute(sql::DELETE_BOOKMARK.text(), params![user_id, file_id])?;
                Ok(())
            })
            .await
    }
}
