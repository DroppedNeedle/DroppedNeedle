//! Who is due a taste build, a pool build or a weekly set, asked by
//! [`Due`](crate::discover::due::Due) on each pass of the clock and on each
//! visit.
//!
//! A person is due a taste build when they have none, when theirs is a day
//! old, when it is [`PLAY_GAP_MS`] old and they played something since,
//! when their imported history moved on since, or when it missed facts that
//! have all arrived since. So a person playing all evening is rebuilt now
//! and then, not on every pass. Each taste build queues a pool build as it
//! completes; a person with a taste build is due one more when they have no
//! pool, when theirs was built before their latest taste build, or when it
//! was built waiting for album facts and one of them has arrived since. On
//! the clock's pass only people seen in the last seven days are asked
//! about; a visit asks about the visitor. A person queued in the last
//! [`REQUEUE_MS`] is not queued again for that build while it waits to run,
//! and no pool build is asked for while one is queued or running, nor for
//! a day after one failed.
//!
//! A person with a taste build whose week is due a set
//! ([`weekly::due_week`]) gets the weekly set job: on the clock's pass for
//! everyone seen in the last [`SET_SEEN_WINDOW_MS`], since the set reads
//! stored rows only, and on a visit for the visitor. While the week waits
//! for its set the job is asked again once their taste or pool is built
//! again or their first set's wait for their history runs out, and else
//! once a day. A visitor with no set yet has their first set's wait for
//! their history noted ([`weekly_store::note_history_wait`]).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobKind, JobQueue, State};
use rusqlite::params;

use super::weekly::{self, SetDay};
use super::{jobs, pool, sql, weekly_store};
use crate::discover::facts::plan::SEEN_WINDOW_MS;
use crate::discover::facts::{FactKind, FactSource, ListSource};

/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// The least time between two queuings of one person's build.
pub const REQUEUE_MS: i64 = 15 * 60_000;
/// How old a build is before a play makes it due: page builds run every
/// twelve hours, so a few plays more change nothing sooner.
pub const PLAY_GAP_MS: i64 = 6 * 3_600_000;
/// How lately a person was seen for the clock to build their weekly set.
pub const SET_SEEN_WINDOW_MS: i64 = 30 * DAY_MS;

/// When each person's weekly set was last asked for, and over what.
struct SetMark {
    /// The week it was asked for, its start.
    week: i64,
    /// Their taste build then, unix ms.
    taste: i64,
    /// Their pool build then, unix ms.
    pool: Option<i64>,
    /// When it was queued, unix ms.
    queued_at: i64,
}

/// When each person's taste and pool builds and weekly set were last
/// queued here.
#[derive(Default)]
pub(crate) struct TasteDue {
    queued: Mutex<HashMap<String, i64>>,
    pools: Mutex<HashMap<String, i64>>,
    sets: Mutex<HashMap<String, SetMark>>,
}

fn lock<T>(marks: &Mutex<HashMap<String, T>>) -> MutexGuard<'_, HashMap<String, T>> {
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

/// Queue `kind` for each of `users` in one write; whether it was written.
/// A failure is logged and the next pass tries again.
async fn enqueue_all(db: &Db, queue: &JobQueue, kind: JobKind, users: Vec<String>) -> bool {
    let queue = queue.clone();
    let queued = db
        .write(Lane::Background, "discover.due.builds", move |tx| {
            for user in &users {
                queue.enqueue(tx, kind, user, &())?;
            }
            Ok::<_, JobError>(())
        })
        .await;
    match queued {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, kind = kind.name(), "builds not queued; trying next pass");
            false
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
    if due.is_empty() || !enqueue_all(db, queue, kind, due.clone()).await {
        return;
    }
    let mut marks = lock(marks);
    marks.retain(|_, at| now_ms - *at < REQUEUE_MS);
    for user in due {
        marks.insert(user, now_ms);
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
        let seen =
            match droppedneedle_auth::sqlite::seen_since(db, now_ms - SET_SEEN_WINDOW_MS).await {
                Ok(seen) => seen,
                Err(error) => {
                    tracing::warn!(%error, "who was seen is unreadable; no weekly set this pass");
                    return;
                }
            };
        self.queue_sets(db, queue, &seen, now_ms, false).await;
    }

    /// `user_id`'s visit at `now_ms`: a build or a set when they are due
    /// one. A failure is logged; the next visit or pass tries again.
    pub(crate) async fn visit(&self, db: &Db, queue: &JobQueue, user_id: &str, now_ms: i64) {
        let people = [user_id.to_owned()];
        self.queue_due(db, queue, &people, now_ms).await;
        self.queue_pools(db, queue, &people, now_ms).await;
        self.queue_sets(db, queue, &people, now_ms, true).await;
    }

    /// The weekly sets due among `people`: a person with a taste build
    /// whose week is due a set, asked again only once what it is built
    /// from changed (the week, their taste or pool build, their first set's
    /// wait for their history running out) or a day after it was last
    /// asked for. On a `visit`, a person with no set yet has their wait for
    /// their history noted first, taste build or not.
    async fn queue_sets(
        &self,
        db: &Db,
        queue: &JobQueue,
        people: &[String],
        now_ms: i64,
        visit: bool,
    ) {
        if people.is_empty() {
            return;
        }
        let asked = match serde_json::to_string(people) {
            Ok(asked) => asked,
            Err(error) => {
                tracing::warn!(%error, "who to ask about is unwritable; no set queued");
                return;
            }
        };
        let rows: Result<Vec<sql::WeekDue>, DbError> = db
            .read("discover.weekly.due", move |conn| {
                conn.prepare_cached(sql::WEEK_DUE.text())?
                    .query_map([asked], sql::WeekDue::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "who is due a weekly set is unreadable");
                return;
            }
        };
        let mut due: Vec<(String, SetMark)> = Vec::new();
        let mut first = Vec::new();
        {
            let marks = lock(&self.sets);
            for row in rows {
                let day = row.set_day.unwrap_or(SetDay::DEFAULT);
                let last = row.last_week_at.map(|at| at.0);
                let Some(week) = weekly::due_week(now_ms, day, last) else {
                    continue;
                };
                if visit && last.is_none() {
                    first.push(row.user_id.clone());
                }
                let Some(taste) = row.taste_built_at.map(|at| at.0) else {
                    continue;
                };
                let mark = SetMark {
                    week,
                    taste,
                    pool: row.pool_built_at.map(|at| at.0),
                    queued_at: now_ms,
                };
                let waited_out = |queued_at: i64| {
                    row.wait_from_at
                        .map(|from| from.0 + weekly::HISTORY_WAIT_MS)
                        .is_some_and(|end| queued_at < end && end <= now_ms)
                };
                let fresh = marks.get(&row.user_id).is_none_or(|before| {
                    (before.week, before.taste, before.pool) != (mark.week, mark.taste, mark.pool)
                        || now_ms - before.queued_at >= DAY_MS
                        || waited_out(before.queued_at)
                });
                if fresh {
                    due.push((row.user_id, mark));
                }
            }
        }
        for user in &first {
            if let Err(error) = weekly_store::note_history_wait(db, user, now_ms).await {
                tracing::warn!(%error, "a first set's wait for history not noted");
            }
        }
        if due.is_empty() {
            return;
        }
        let users: Vec<String> = due.iter().map(|(user, _)| user.clone()).collect();
        if enqueue_all(db, queue, weekly::KIND, users).await {
            let mut marks = lock(&self.sets);
            marks.retain(|_, mark| now_ms - mark.queued_at < DAY_MS);
            marks.extend(due);
        }
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

    /// The pool builds due among `people` (see [`sql::POOL_DUE`]), leaving
    /// out anyone whose pool build is queued or running, or failed in the
    /// last day: the queue's own retries already ran, and a build that
    /// keeps failing is not asked for on every pass.
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
        let due = match due {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(%error, "who is due a pool build is unreadable");
                return;
            }
        };
        let mut free = Vec::with_capacity(due.len());
        for user in due {
            match queue.latest(pool::KIND, &user).await {
                Ok(Some(job))
                    if matches!(job.state, State::Queued | State::Running)
                        || (job.state == State::Failed
                            && job.finished_at.is_some_and(|at| now_ms - at < DAY_MS)) => {}
                Ok(_) => free.push(user),
                Err(error) => {
                    tracing::warn!(%error, "a pool build's job unreadable; not queued this pass");
                }
            }
        }
        enqueue(db, queue, pool::KIND, &self.pools, free, now_ms).await;
    }
}
