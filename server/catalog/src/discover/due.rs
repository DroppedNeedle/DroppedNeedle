//! The one place Discover's background work for a person is queued from.
//!
//! Two calls, and every kind of work Discover runs for a person in the
//! background is queued through them, never by a loop of its own:
//!
//! - [`Due::clock_pass`], from the 90-second demand tick: once per UTC day,
//!   each person's daily work. Nothing while the warm cycle is switched
//!   off (`DISCOVER_WARMER_ENABLED`).
//! - [`Due::visit`], where the page records a person's activity: that
//!   person's own work, whether the warm cycle runs or not.
//!
//! Today that work is the pick record's outcomes job, for each person with
//! rows in it. A person's job is queued at most once a UTC day; after a
//! restart it may run once more, which a job may always do.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobQueue};

use crate::discover::record::outcomes;
use crate::discover::sql;

/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// The day the clock has passed before its first pass: none.
const NOT_YET: i64 = i64::MIN;

/// Queues each person's Discover background work.
pub struct Due {
    db: Db,
    queue: JobQueue,
    clock: Arc<dyn Clock>,
    enabled: bool,
    /// The UTC day the clock last passed; [`NOT_YET`] before the first.
    passed: Mutex<i64>,
    /// The UTC day each person's work was last queued.
    queued: Mutex<HashMap<String, i64>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The maps stay whole whatever panicked while the lock was held.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Due {
    /// Work queued on `queue`, written through `db`, dated by `clock`.
    /// `enabled` is the warm cycle's switch.
    pub fn new(db: Db, queue: JobQueue, clock: Arc<dyn Clock>, enabled: bool) -> Self {
        Self {
            db,
            queue,
            clock,
            enabled,
            passed: Mutex::new(NOT_YET),
            queued: Mutex::new(HashMap::new()),
        }
    }

    fn today(&self) -> i64 {
        self.clock.now_ms().div_euclid(DAY_MS)
    }

    /// The clock's pass: on the first pass of each UTC day, every person
    /// with rows in the pick record whose job was not queued today gets one,
    /// in one write. Nothing while the warm cycle is off. A failed write is
    /// logged and the next pass tries again.
    pub async fn clock_pass(&self) {
        if !self.enabled {
            return;
        }
        let today = self.today();
        if *lock(&self.passed) == today {
            return;
        }
        let done: Vec<String> = lock(&self.queued)
            .iter()
            .filter(|(_, day)| **day == today)
            .map(|(user, _)| user.clone())
            .collect();
        let queue = self.queue.clone();
        let queued = self
            .db
            .write(Lane::Background, "discover.due.clock", move |tx| {
                let users = tx
                    .prepare(sql::PICK_USERS.text())?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut queued = Vec::new();
                for user in users.into_iter().filter(|user| !done.contains(user)) {
                    queue.enqueue(tx, outcomes::KIND, &user, &())?;
                    queued.push(user);
                }
                Ok::<_, JobError>(queued)
            })
            .await;
        match queued {
            Ok(users) => {
                let mut marks = lock(&self.queued);
                marks.retain(|_, day| *day == today);
                for user in users {
                    marks.insert(user, today);
                }
                *lock(&self.passed) = today;
            }
            Err(error) => {
                tracing::warn!(%error, "discover's daily work not queued; trying next pass");
            }
        }
    }

    /// A person's visit: their outcomes job, when they have rows in the
    /// pick record and it was not queued today. Runs whether the warm
    /// cycle is on or not. A failed write is logged; the next visit or
    /// pass tries again.
    pub async fn visit(&self, user_id: &str) {
        let today = self.today();
        if lock(&self.queued).get(user_id) == Some(&today) {
            return;
        }
        let user = user_id.to_owned();
        let has_rows: Result<bool, DbError> = self
            .db
            .read("discover.due.has_picks", move |conn| {
                conn.query_row(sql::USER_HAS_PICKS.text(), [&user], |row| row.get(0))
                    .map_err(DbError::from)
            })
            .await;
        match has_rows {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(%error, "a visitor's picks unreadable; their work not queued");
                return;
            }
        }
        let (queue, user) = (self.queue.clone(), user_id.to_owned());
        let queued = self
            .db
            .write(Lane::Background, "discover.due.visit", move |tx| {
                queue.enqueue(tx, outcomes::KIND, &user, &())?;
                Ok::<_, JobError>(())
            })
            .await;
        match queued {
            Ok(()) => {
                lock(&self.queued).insert(user_id.to_owned(), today);
            }
            Err(error) => tracing::warn!(%error, "a visitor's discover work not queued"),
        }
    }
}
