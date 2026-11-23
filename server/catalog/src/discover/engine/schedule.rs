//! Who is due a taste build, asked by [`Due`](crate::discover::due::Due)
//! on each pass of the clock and on each visit.
//!
//! A person is due one when they have none, when theirs is a day old, when
//! it is [`PLAY_GAP_MS`] old and they played something since, when their
//! imported history moved on since, or when it missed facts that have all
//! arrived since. So a person playing all evening is rebuilt now and then,
//! not on every pass. On the clock's pass only people seen in the last
//! seven days are asked about; a visit asks about the visitor. A person
//! queued in the last [`REQUEUE_MS`] is not queued again while their build
//! waits to run.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobQueue};
use rusqlite::params;

use super::{jobs, sql};
use crate::discover::facts::plan::SEEN_WINDOW_MS;
use crate::discover::facts::{FactKind, FactSource, ListSource};

/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// The least time between two queuings of one person's build.
pub const REQUEUE_MS: i64 = 15 * 60_000;
/// How old a build is before a play makes it due: page builds run every
/// twelve hours, so a few plays more change nothing sooner.
pub const PLAY_GAP_MS: i64 = 6 * 3_600_000;

/// When each person's build was last queued.
#[derive(Default)]
pub(crate) struct TasteDue {
    queued: Mutex<HashMap<String, i64>>,
}

impl TasteDue {
    fn queued(&self) -> MutexGuard<'_, HashMap<String, i64>> {
        // The map stays whole whatever panicked while the lock was held.
        self.queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The clock's pass at `now_ms`: everyone seen in the last seven days
    /// and due gets a build. A failure is logged and the next pass tries
    /// again.
    pub(crate) async fn pass(&self, db: &Db, queue: &JobQueue, now_ms: i64) {
        let seen = match droppedneedle_auth::sqlite::seen_since(db, now_ms - SEEN_WINDOW_MS).await {
            Ok(seen) => seen,
            Err(error) => {
                tracing::warn!(%error, "who was seen is unreadable; no taste built this pass");
                return;
            }
        };
        self.queue_due(db, queue, seen, now_ms).await;
    }

    /// `user_id`'s visit at `now_ms`: a build when they are due one. A
    /// failure is logged; the next visit or pass tries again.
    pub(crate) async fn visit(&self, db: &Db, queue: &JobQueue, user_id: &str, now_ms: i64) {
        self.queue_due(db, queue, vec![user_id.to_owned()], now_ms)
            .await;
    }

    async fn queue_due(&self, db: &Db, queue: &JobQueue, people: Vec<String>, now_ms: i64) {
        let people: Vec<String> = {
            let queued = self.queued();
            people
                .into_iter()
                .filter(|user| queued.get(user).is_none_or(|at| now_ms - at >= REQUEUE_MS))
                .collect()
        };
        if people.is_empty() {
            return;
        }
        let asked = match serde_json::to_string(&people) {
            Ok(asked) => asked,
            Err(error) => {
                tracing::warn!(%error, "who to ask about is unwritable; no taste built");
                return;
            }
        };
        let due: Result<Vec<String>, DbError> = db
            .read("discover.taste.due", move |conn| {
                conn.prepare_cached(sql::TASTE_DUE.text())?
                    .query_map(
                        params![
                            asked,
                            now_ms - DAY_MS,
                            FactKind::Neighbours,
                            FactSource::Labs,
                            FactKind::Listeners,
                            FactSource::ListenBrainz,
                            ListSource::Labs,
                            now_ms - PLAY_GAP_MS,
                            now_ms
                        ],
                        |row| row.get(0),
                    )?
                    .collect::<rusqlite::Result<Vec<String>>>()
                    .map_err(DbError::from)
            })
            .await;
        let due = match due {
            Ok(due) if due.is_empty() => return,
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(%error, "who is due a taste build is unreadable");
                return;
            }
        };
        let (queue, users) = (queue.clone(), due.clone());
        let queued = db
            .write(Lane::Background, "discover.due.taste", move |tx| {
                for user in &users {
                    queue.enqueue(tx, jobs::KIND, user, &())?;
                }
                Ok::<_, JobError>(())
            })
            .await;
        match queued {
            Ok(()) => {
                let mut marks = self.queued();
                marks.retain(|_, at| now_ms - *at < REQUEUE_MS);
                for user in due {
                    marks.insert(user, now_ms);
                }
            }
            Err(error) => tracing::warn!(%error, "taste builds not queued; trying next pass"),
        }
    }
}
