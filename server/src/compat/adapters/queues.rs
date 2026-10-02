//! Compat-owned play queues and bookmarks (per-user memory state; v3 has
//! no native store, mirroring the collections in-memory precedent).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::compat::subsonic::store::{BookmarkRecord, QueueState};

#[derive(Debug, Default)]
struct QueuesInner {
    queues: HashMap<String, QueueState>,
    bookmarks: HashMap<String, Vec<BookmarkRecord>>,
}

/// Per-user play queues and bookmarks.
#[derive(Debug, Clone, Default)]
pub struct CompatQueues {
    inner: Arc<Mutex<QueuesInner>>,
}

impl CompatQueues {
    /// Empty state.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueuesInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Saved queue for a user (empty when never saved).
    pub fn get_queue(&self, user_id: &str) -> QueueState {
        self.lock().queues.get(user_id).cloned().unwrap_or_default()
    }

    /// Replace a user's saved queue.
    pub fn replace_queue(
        &self,
        user_id: &str,
        file_ids: &[String],
        current_index: Option<usize>,
        position_ms: i64,
        changed_by_client: &str,
        now_unix: f64,
    ) {
        self.lock().queues.insert(
            user_id.to_owned(),
            QueueState {
                file_ids: file_ids.to_vec(),
                current_index,
                position_ms,
                updated_at: now_unix,
                changed_by_client: changed_by_client.to_owned(),
            },
        );
    }

    /// Bookmarks for a user.
    pub fn list_bookmarks(&self, user_id: &str) -> Vec<BookmarkRecord> {
        self.lock()
            .bookmarks
            .get(user_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Create or update a bookmark.
    pub fn upsert_bookmark(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: i64,
        comment: &str,
        now_unix: i64,
    ) {
        let mut guard = self.lock();
        let list = guard.bookmarks.entry(user_id.to_owned()).or_default();
        if let Some(existing) = list.iter_mut().find(|mark| mark.file_id == file_id) {
            existing.position_ms = position_ms;
            existing.comment = comment.to_owned();
            existing.changed_at = now_unix;
        } else {
            list.push(BookmarkRecord {
                file_id: file_id.to_owned(),
                position_ms,
                comment: comment.to_owned(),
                created_at: now_unix,
                changed_at: now_unix,
            });
        }
    }

    /// Delete a bookmark.
    pub fn delete_bookmark(&self, user_id: &str, file_id: &str) {
        if let Some(list) = self.lock().bookmarks.get_mut(user_id) {
            list.retain(|mark| mark.file_id != file_id);
        }
    }
}
