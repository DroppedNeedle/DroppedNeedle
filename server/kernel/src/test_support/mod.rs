//! What tests need from the kernel and production never uses: a clock that
//! moves only when told, a kick that does nothing, the migrated template,
//! scratch settings stores, the statement registry and the knobs tests turn
//! on the jobs table and the precache watchdog. The database's own test
//! knobs sit in [`crate::db::test_support`], beside what they reach.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

pub mod template;

use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
use std::time::Duration;

use rusqlite::Transaction;

use crate::clock::Clock;
use crate::db::statement::StatementFile;
use crate::db::{Db, DbError, DurableLane, Lane};
use crate::jobs::BoxFuture;
use crate::jobs::discovery_demand::DemandTick;
use crate::jobs::events_kick::EventsKick;
use crate::jobs::events_watcher::WallTime;
use crate::jobs::precache::PrecacheLimits;
use crate::jobs::queue::lease::{self, LeaseStatements, Owner};
use crate::jobs::queue::{Claimed, JobError, JobQueue, KindSpec};
use crate::mount::{Postures, Routes};
use crate::runtime_config::Crypto;
use crate::runtime_config::store::{Change, ConfigStore, decode_text};
use crate::runtime_config::{ConfigError, LegacyConfig};

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::db::sql::registered(),
        crate::db::cache::sql::registered(),
        crate::jobs::queue::sql::registered(),
        crate::events::sql::registered(),
        crate::jobs::sql::registered(),
        crate::runtime_config::sql::registered(),
    ]
}

/// A clock that moves only when a test sets it.
#[derive(Debug, Default, Clone)]
pub struct ManualClock {
    now_ms: Arc<AtomicI64>,
}

impl ManualClock {
    /// A clock reading `now_ms`.
    pub fn at(now_ms: i64) -> Self {
        Self {
            now_ms: Arc::new(AtomicI64::new(now_ms)),
        }
    }

    /// Set the time.
    pub fn set(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Move the time by `by_ms`.
    pub fn advance(&self, by_ms: i64) {
        self.now_ms.fetch_add(by_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// No warm cycle, for test states.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDemand;

impl DemandTick for NoDemand {
    fn tick(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// A kick that does nothing, for test states.
#[derive(Debug, Default)]
pub struct NoopKick;

impl EventsKick for NoopKick {
    fn kick(&self) {}
}

impl Routes<()> {
    /// Every group in one router with no layers, for tests that mount one
    /// area alone.
    pub fn into_router(self) -> axum::Router {
        let Postures {
            public,
            user,
            curator,
            admin,
            wrapped_key,
            upgrade_session,
        } = self.into_postures();
        public
            .merge(user)
            .merge(curator)
            .merge(admin)
            .merge(wrapped_key)
            .merge(upgrade_session)
            .into()
    }
}

impl ConfigStore {
    /// A store for tests over `settings.db` in `dir`: a fresh migrated copy,
    /// or the one an earlier call left there, so a test can open it again.
    /// A `config.json` the test wrote in `dir` is copied in as at a first
    /// start, which is how a test stores a section exactly as written.
    pub fn scratch(dir: &std::path::Path, crypto: Crypto) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let path = dir.join("settings.db");
        if !path.exists() {
            template::copy_to(&path)?;
        }
        let db = Db::open_for_tests(&path).map_err(|error| error.to_string())?;
        Self::open_blocking(
            db,
            crypto,
            Arc::new(crate::clock::SystemClock),
            &LegacyConfig::at(dir, false),
        )
        .map_err(|error| error.to_string())
    }

    /// Store `text` as one section, undecoded, for tests of a row that does
    /// not read.
    pub fn replace_stored_for_test(&self, key: &str, text: &str) -> Result<(), ConfigError> {
        let value = decode_text(text)?;
        self.change(DurableLane::Foreground, "config.test_replace", key, |_| {
            Ok::<_, ConfigError>(Change::Write(value, ()))
        })
    }

    /// The stored text of one section, for tests that check what reaches
    /// the database.
    pub fn stored_text(&self, key: &str) -> Option<String> {
        self.entry(key)
            .and_then(|entry| serde_json::to_string(&entry.value).ok())
    }
}

impl PrecacheLimits {
    /// Override the watchdog tick (tests run it fast).
    #[must_use]
    pub fn with_watchdog_tick(mut self, tick: Duration) -> Self {
        self.watchdog_tick = tick;
        self
    }
}

impl WallTime {
    /// Midnight `days` after the epoch.
    pub fn day(days_since_epoch: i64) -> Self {
        Self {
            days_since_epoch,
            minutes_since_midnight: 0,
        }
    }
}

impl JobQueue {
    /// Recover expired leases and claim one due job, on the caller's
    /// transaction, for a test that stands in for a worker.
    pub fn claim_in(&self, tx: &Transaction, spec: &KindSpec) -> Result<Option<Claimed>, JobError> {
        crate::jobs::queue::claim_on(tx, spec, &self.owners.mint(), self.now())
    }

    /// Requeue rows of a kind whose lease ran out, as a runner pass does.
    pub async fn recover_expired(&self, spec: &KindSpec) -> Result<u64, JobError> {
        let spec = *spec;
        let now = self.now();
        self.db
            .write(Lane::Background, "jobs-recover", move |tx| {
                crate::jobs::queue::recover(tx, &spec, false, now)
            })
            .await
    }
}

/// Why the conformance check's one write ends: a broken promise, the end
/// of the walk, or a failure of the table itself.
enum Conformance {
    Broke(String),
    Done,
    Job(JobError),
}

impl std::fmt::Display for Conformance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Broke(message) => f.write_str(message),
            Self::Done => f.write_str("the check ran to its end"),
            Self::Job(error) => error.fmt(f),
        }
    }
}

impl From<DbError> for Conformance {
    fn from(error: DbError) -> Self {
        Self::Job(JobError::Db(error))
    }
}

impl From<JobError> for Conformance {
    fn from(error: JobError) -> Self {
        Self::Job(error)
    }
}

impl From<rusqlite::Error> for Conformance {
    fn from(error: rusqlite::Error) -> Self {
        Self::Job(JobError::from(error))
    }
}

/// Check a table's five statements keep the lease promises: a stale owner's
/// heartbeat and release change nothing, a lease that ran out is requeued,
/// and every held row is requeued at start. `seed` adds one claimable row
/// of `scope` and returns its id. The steps run in one write on `db`, which
/// rolls back.
pub async fn check_conformance<F>(
    db: &Db,
    statements: LeaseStatements,
    scope: &'static str,
    seed: F,
) -> Result<(), String>
where
    F: FnOnce(&Transaction) -> Result<i64, rusqlite::Error> + Send + 'static,
{
    const LEASE: i64 = 60_000;
    let broke = |message: String| Conformance::Broke(message);
    let owners = Owner::new();
    let (first, stale) = (owners.mint(), owners.mint());
    let now = 1_000_000_i64;
    let ended = db
        .write(Lane::Background, "lease-conformance", move |tx| {
            let seeded = seed(tx)?;
            let claimed = lease::claim(tx, &statements, scope, &first, now, now + LEASE)?;
            if claimed != Some(seeded) {
                return Err(broke(format!("claim took {claimed:?}, seeded {seeded}")));
            }
            if lease::heartbeat(tx, &statements, seeded, &stale, now, now + LEASE)? {
                return Err(broke(
                    "a stale owner's heartbeat changed the row".to_owned(),
                ));
            }
            if lease::release(tx, &statements, seeded, &stale, now)? {
                return Err(broke("a stale owner's release changed the row".to_owned()));
            }
            if !lease::heartbeat(tx, &statements, seeded, &first, now, now + LEASE)? {
                return Err(broke("the owner's heartbeat changed nothing".to_owned()));
            }
            if lease::recover_expired(tx, &statements, scope, now + LEASE - 1)? != 0 {
                return Err(broke("a live lease was recovered".to_owned()));
            }
            if lease::recover_expired(tx, &statements, scope, now + LEASE)? != 1 {
                return Err(broke("a lease that ran out was not recovered".to_owned()));
            }
            let again = lease::claim(tx, &statements, scope, &stale, now + LEASE, now + 2 * LEASE)?;
            if again != Some(seeded) {
                return Err(broke(format!(
                    "the recovered row was not claimable: {again:?}"
                )));
            }
            if lease::recover_at_start(tx, &statements, scope, now + LEASE)? != 1 {
                return Err(broke("start recovery left a held row".to_owned()));
            }
            Err::<(), _>(Conformance::Done)
        })
        .await;
    match ended {
        Ok(()) | Err(Conformance::Done) => Ok(()),
        Err(Conformance::Broke(message)) => Err(message),
        Err(Conformance::Job(error)) => Err(error.to_string()),
    }
}
