//! Who is due a taste build or a pool build, asked by
//! [`Due`](crate::discover::due::Due) on each pass of the clock and on each
//! visit.
//!
//! A person is due a taste build when they have none, when theirs is a day
//! old, when it is [`PLAY_GAP_MS`] old and they played something since,
//! when their imported history moved on since, or when it missed facts that
//! have all arrived since. So a person playing all evening is rebuilt now
//! and then, not on every pass. Each taste build queues a pool build as it
//! completes; a person is due one more when their pool was built waiting
//! for album facts and one of them has arrived since. On the clock's pass
//! only people seen in the last seven days are asked about; a visit asks
//! about the visitor. A person queued in the last [`REQUEUE_MS`] is not
//! queued again for that build while it waits to run.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobKind, JobQueue};
use rusqlite::params;

use super::{jobs, pool, sql};
use crate::discover::facts::plan::SEEN_WINDOW_MS;
use crate::discover::facts::{FactKind, FactSource, ListSource};

/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// The least time between two queuings of one person's build.
pub const REQUEUE_MS: i64 = 15 * 60_000;
/// How old a build is before a play makes it due: page builds run every
/// twelve hours, so a few plays more change nothing sooner.
pub const PLAY_GAP_MS: i64 = 6 * 3_600_000;

/// When each person's taste and pool builds were last queued here.
#[derive(Default)]
pub(crate) struct TasteDue {
    queued: Mutex<HashMap<String, i64>>,
    pools: Mutex<HashMap<String, i64>>,
}

fn lock(marks: &Mutex<HashMap<String, i64>>) -> MutexGuard<'_, HashMap<String, i64>> {
    // The map stays whole whatever panicked while the lock was held.
    marks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Of `people`, the ones not queued in `marks` in the last [`REQUEUE_MS`],
/// as the JSON list the due statements read; `None` when there are none.
fn unmarked(marks: &Mutex<HashMap<String, i64>>, people: &[String], now_ms: i64) -> Option<String> {
    let people: Vec<&String> = {
        let queued = lock(marks);
        people
            .iter()
            .filter(|user| queued.get(*user).is_none_or(|at| now_ms - at >= REQUEUE_MS))
            .collect()
    };
    if people.is_empty() {
        return None;
    }
    match serde_json::to_string(&people) {
        Ok(asked) => Some(asked),
        Err(error) => {
            tracing::warn!(%error, "who to ask about is unwritable; nothing queued");
            None
        }
    }
}

/// Queue `kind` for each of `due` in one write, and mark them in `marks`.
async fn enqueue(
    db: &Db,
    queue: &JobQueue,
    kind: JobKind,
    marks: &Mutex<HashMap<String, i64>>,
    due: Vec<String>,
    now_ms: i64,
) {
    if due.is_empty() {
        return;
    }
    let (queue, users) = (queue.clone(), due.clone());
    let queued = db
        .write(Lane::Background, "discover.due.builds", move |tx| {
            for user in &users {
                queue.enqueue(tx, kind, user, &())?;
            }
            Ok::<_, JobError>(())
        })
        .await;
    match queued {
        Ok(()) => {
            let mut marks = lock(marks);
            marks.retain(|_, at| now_ms - *at < REQUEUE_MS);
            for user in due {
                marks.insert(user, now_ms);
            }
        }
        Err(error) => {
            tracing::warn!(%error, kind = kind.name(), "builds not queued; trying next pass")
        }
    }
}

impl TasteDue {
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
        self.queue_due(db, queue, &seen, now_ms).await;
        self.queue_pools(db, queue, &seen, now_ms).await;
    }

    /// `user_id`'s visit at `now_ms`: a build when they are due one. A
    /// failure is logged; the next visit or pass tries again.
    pub(crate) async fn visit(&self, db: &Db, queue: &JobQueue, user_id: &str, now_ms: i64) {
        let people = [user_id.to_owned()];
        self.queue_due(db, queue, &people, now_ms).await;
        self.queue_pools(db, queue, &people, now_ms).await;
    }

    async fn queue_due(&self, db: &Db, queue: &JobQueue, people: &[String], now_ms: i64) {
        let Some(asked) = unmarked(&self.queued, people, now_ms) else {
            return;
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
        match due {
            Ok(due) => enqueue(db, queue, jobs::KIND, &self.queued, due, now_ms).await,
            Err(error) => tracing::warn!(%error, "who is due a taste build is unreadable"),
        }
    }

    /// The pool builds due among `people`: ones built waiting for album
    /// facts of which one has arrived since.
    async fn queue_pools(&self, db: &Db, queue: &JobQueue, people: &[String], now_ms: i64) {
        let Some(asked) = unmarked(&self.pools, people, now_ms) else {
            return;
        };
        let due: Result<Vec<String>, DbError> = db
            .read("discover.pool.due", move |conn| {
                conn.prepare_cached(sql::POOL_DUE.text())?
                    .query_map(
                        params![
                            asked,
                            FactKind::Albums,
                            FactSource::MusicBrainz,
                            FactKind::AlbumCounts,
                            FactSource::ListenBrainz
                        ],
                        |row| row.get(0),
                    )?
                    .collect::<rusqlite::Result<Vec<String>>>()
                    .map_err(DbError::from)
            })
            .await;
        match due {
            Ok(due) => enqueue(db, queue, pool::KIND, &self.pools, due, now_ms).await,
            Err(error) => tracing::warn!(%error, "who is due a pool build is unreadable"),
        }
    }
}
