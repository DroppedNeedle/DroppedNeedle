//! The one place Discover's background work is queued from.
//!
//! Three calls, and every kind of work Discover runs in the background is
//! queued through them, never by a loop of its own:
//!
//! - [`Due::clock_pass`], from the 90-second demand tick: the kept facts'
//!   refresh whenever it has work and room in the day, the fresh releases'
//!   daily read when it is due ([`FreshFeedReads::due`]), a taste or pool
//!   build for each person seen in seven days who is due one, a weekly set
//!   for each person seen in thirty days whose week is due one, the
//!   ListenBrainz history work media decides is due
//!   ([`HistoryStore::tick`]), each person seen in seven days whose Daily
//!   Mixes are due, and once per UTC day each person's daily work. Nothing
//!   while the warm cycle is switched off (`DISCOVER_WARMER_ENABLED`).
//! - [`Due::visit`], where the page records a person's activity: the kept
//!   facts' refresh when it has work, the fresh releases' read when it is
//!   due, their ListenBrainz history
//!   ([`HistoryStore::visit`]), their taste or pool build or weekly set
//!   when one is due, their Daily Mixes when due, and that person's own
//!   work, whether the warm cycle runs or not.
//! - [`Due::queue_now`], inside another write: one kind for one key (a
//!   person's id, or the install's one key), due now.
//!
//! Today the daily work is the pick record's outcomes job, for each person
//! with rows in it. A person's job is queued at most once a UTC day; after
//! a restart it may run once more, which a job may always do.
//!
//! A person with a taste build is due their Daily Mixes ([`mixes::KIND`])
//! once a UTC day, and once more the day their first pool build lands, so
//! a new account's mixes take in its pool the day it is built.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobKind, JobQueue};
use droppedneedle_media::history::HistoryStore;
use rusqlite::Transaction;

use crate::discover::engine::schedule::TasteDue;
use crate::discover::facts::job::{self as facts, FactsDue};
use crate::discover::facts::plan::SEEN_WINDOW_MS;
use crate::discover::listening::fresh::{self, FreshFeedReads};
use crate::discover::mixes;
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
    facts: FactsDue,
    /// When the fresh releases' read is due.
    fresh: FreshFeedReads,
    /// When each person's taste build was last queued.
    taste: TasteDue,
    history: Arc<HistoryStore>,
    /// The UTC day the clock last passed; [`NOT_YET`] before the first.
    passed: Mutex<i64>,
    /// The UTC day each person's work was last queued.
    queued: Mutex<HashMap<String, i64>>,
    /// The UTC day each person's mixes were last queued, and whether they
    /// had a pool build then.
    mixes: Mutex<HashMap<String, (i64, bool)>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The maps stay whole whatever panicked while the lock was held.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Due {
    /// Work queued on `queue`, written through `db`, dated by `clock`.
    /// `enabled` is the warm cycle's switch; `facts` says when the kept
    /// facts' refresh has work; `history` queues the people's ListenBrainz
    /// history work; `fresh` says when the fresh releases are due a read.
    pub fn new(
        db: Db,
        queue: JobQueue,
        clock: Arc<dyn Clock>,
        enabled: bool,
        (facts, history, fresh): (FactsDue, Arc<HistoryStore>, FreshFeedReads),
    ) -> Self {
        Self {
            db,
            queue,
            clock,
            enabled,
            facts,
            fresh,
            taste: TasteDue::default(),
            history,
            passed: Mutex::new(NOT_YET),
            queued: Mutex::new(HashMap::new()),
            mixes: Mutex::new(HashMap::new()),
        }
    }

    fn today(&self) -> i64 {
        self.clock.now_ms().div_euclid(DAY_MS)
    }

    /// Queue `kind` for `key` (a person's id, or an install-wide job's one
    /// key), due now, on the caller's transaction: work a write makes due.
    /// A queued job of that key is joined, not doubled. Runs whether the
    /// warm cycle is on or not.
    ///
    /// # Errors
    ///
    /// The enqueue's failure, which fails the caller's write.
    pub fn queue_now(&self, tx: &Transaction, kind: JobKind, key: &str) -> Result<(), JobError> {
        self.queue.enqueue(tx, kind, key, &())?;
        Ok(())
    }

    /// The kept facts' refresh, when it has a fact to read and room in the
    /// day. A failed write is logged and the next tick tries again.
    async fn facts_pass(&self) {
        if !self.facts.waiting().await {
            return;
        }
        let queue = self.queue.clone();
        let queued = self
            .db
            .write(Lane::Background, "discover.due.facts", move |tx| {
                queue.enqueue(tx, facts::KIND, facts::KEY, &())?;
                Ok::<_, JobError>(())
            })
            .await;
        if let Err(error) = queued {
            tracing::warn!(%error, "the kept facts' refresh not queued; trying next pass");
        }
    }

    /// The fresh releases' read, when it is due. A failed write is logged
    /// and the next tick tries again.
    async fn fresh_pass(&self) {
        if !self.fresh.due().await {
            return;
        }
        let queue = self.queue.clone();
        let queued = self
            .db
            .write(Lane::Background, "discover.due.fresh", move |tx| {
                queue.enqueue(tx, fresh::KIND, fresh::KEY, &())?;
                Ok::<_, JobError>(())
            })
            .await;
        if let Err(error) = queued {
            tracing::warn!(%error, "the fresh releases' read not queued; trying next pass");
        }
    }

    /// The Daily Mixes due among `people`: each with a taste build whose
    /// mixes were not queued here today, or were queued today before their
    /// first pool build. One write queues them all; a failure is logged and
    /// the next pass or visit tries again.
    async fn mixes_pass(&self, people: Vec<String>) {
        if people.is_empty() {
            return;
        }
        let asked = match serde_json::to_string(&people) {
            Ok(asked) => asked,
            Err(error) => {
                tracing::warn!(%error, "who to ask about is unwritable; no mixes queued");
                return;
            }
        };
        let rows: Result<Vec<sql::MixDue>, DbError> = self
            .db
            .read("discover.mixes.due", move |conn| {
                conn.prepare_cached(sql::MIXES_DUE.text())?
                    .query_map([asked], sql::MixDue::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "who is due their mixes is unreadable");
                return;
            }
        };
        let today = self.today();
        let due: Vec<(String, bool)> = {
            let marks = lock(&self.mixes);
            rows.into_iter()
                .filter(|row| {
                    marks
                        .get(&row.user_id)
                        .is_none_or(|(day, had_pool)| *day != today || (!had_pool && row.has_pool))
                })
                .map(|row| (row.user_id, row.has_pool))
                .collect()
        };
        if due.is_empty() {
            return;
        }
        let queue = self.queue.clone();
        let users: Vec<String> = due.iter().map(|(user, _)| user.clone()).collect();
        let queued = self
            .db
            .write(Lane::Background, "discover.due.mixes", move |tx| {
                for user in &users {
                    queue.enqueue(tx, mixes::KIND, user, &())?;
                }
                Ok::<_, JobError>(())
            })
            .await;
        match queued {
            Ok(()) => {
                let mut marks = lock(&self.mixes);
                marks.retain(|_, (day, _)| *day == today);
                for (user, has_pool) in due {
                    marks.insert(user, (today, has_pool));
                }
            }
            Err(error) => tracing::warn!(%error, "mixes not queued; trying next pass"),
        }
    }

    /// The clock's pass: the kept facts' refresh when it has work, the
    /// fresh releases' read when it is due, the taste builds due, the
    /// history work that is due, the mixes due, then, on the first pass of
    /// each UTC day, every person with rows in the pick record whose job was
    /// not queued today gets one, in one write. Nothing while the warm cycle is off.
    /// A failed write is logged and the next pass tries again.
    pub async fn clock_pass(&self) {
        if !self.enabled {
            return;
        }
        self.facts_pass().await;
        self.fresh_pass().await;
        self.taste
            .pass(&self.db, &self.queue, self.clock.now_ms())
            .await;
        self.history.tick().await;
        match droppedneedle_auth::sqlite::seen_since(&self.db, self.clock.now_ms() - SEEN_WINDOW_MS)
            .await
        {
            Ok(seen) => self.mixes_pass(seen).await,
            Err(error) => tracing::warn!(%error, "who was seen is unreadable; no mixes this pass"),
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

    /// A person's visit: the kept facts' refresh when it has work, the
    /// fresh releases' read when it is due, their ListenBrainz history,
    /// their taste or pool build or weekly set when one is due (a person
    /// with no picks yet included), their mixes when due, and their
    /// outcomes job, when they have
    /// rows in the pick record and it was not queued today. Runs whether
    /// the warm cycle is on or not. A failed write is logged; the next
    /// visit or pass tries again.
    pub async fn visit(&self, user_id: &str) {
        self.facts_pass().await;
        self.fresh_pass().await;
        // The history first: a first visit starts the import, which the
        // person's first set then waits on.
        self.history.visit(user_id).await;
        self.taste
            .visit(&self.db, &self.queue, user_id, self.clock.now_ms())
            .await;
        self.mixes_pass(vec![user_id.to_owned()]).await;
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
