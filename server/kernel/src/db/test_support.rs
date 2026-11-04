//! The database's test knobs: handles on a test file with no runtime
//! behind them, patient lane limits, the WAL hold set by hand, shorter
//! limits for the factory and a provider cache in a scratch folder. They
//! sit inside `db/` because they reach the lane, the read pools and the
//! cache's own state.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use super::DbError;
use super::cache::CacheDb;
use super::factory::DbConfig;
use super::handle::Db;
use super::read_pool::ReadPools;
use super::scratch::ScratchDir;
use super::writer::{
    BACKGROUND_SOFT_BUDGET, Budget, FOREGROUND_SOFT_BUDGET, LaneLimits, WATCH_TICK, WalHold,
    WriteLane,
};

/// How long a test database waits for a turn, a connection or a write's
/// end before giving up: long enough that a loaded host only slows a test.
pub const TEST_PATIENCE: Duration = Duration::from_secs(60);

/// How often a scratch folder is looked at for the last handle on it.
const GONE_POLL: Duration = Duration::from_millis(100);

impl LaneLimits {
    /// Limits for a test database on a loaded host: a minute to wait for
    /// a turn and to run, so a starved test runs slowly instead of hearing
    /// `Busy` or a budget abort. A test that checks a limit sets its own.
    pub fn patient() -> Self {
        Self {
            foreground: Budget {
                warn: FOREGROUND_SOFT_BUDGET,
                abort: TEST_PATIENCE,
            },
            background: Budget {
                warn: BACKGROUND_SOFT_BUDGET,
                abort: TEST_PATIENCE,
            },
            foreground_wait: TEST_PATIENCE,
            watch_tick: WATCH_TICK,
        }
    }
}

impl WalHold {
    /// Set the hold by hand, where a test has no full WAL to show.
    pub fn force(&self, on: bool) {
        self.set(on);
    }
}

impl Db {
    /// A handle on an existing, migrated database file with no runtime
    /// behind it: the writer lane and both read pools, opened on the
    /// calling thread. It starts no watch, so a waiting write or read gets
    /// no `Busy` answer and a stuck write is not logged, and nothing holds
    /// bulk writes for the WAL. Writes run on [`LaneLimits::patient`], so a
    /// loaded host slows a test without aborting its writes.
    pub fn open_for_tests(path: &Path) -> Result<Self, DbError> {
        Self::on_test_lane(path, WriteLane::open_limited(path, LaneLimits::patient())?)
    }

    /// [`Db::open_for_tests`] with other lane time limits, so a test can
    /// run a write past its budget without waiting out the real one.
    pub fn open_for_tests_with(path: &Path, limits: LaneLimits) -> Result<Self, DbError> {
        Self::on_test_lane(path, WriteLane::open_limited(path, limits)?)
    }

    /// A private copy of the migrated template in a scratch folder named
    /// after `tag`, opened through [`Db::open_for_tests`]. The folder is
    /// removed once the last handle on it has closed.
    pub fn scratch(tag: &str) -> Result<Self, String> {
        let dir = ScratchDir::new(tag).map_err(|error| format!("scratch dir: {error}"))?;
        let path = dir.path().join("app.db");
        crate::test_support::template::copy_to(&path)?;
        let db = Self::open_for_tests(&path).map_err(|error| error.to_string())?;
        let reads = std::sync::Arc::downgrade(db.reads());
        std::thread::Builder::new()
            .name("droppedneedle-scratch-db".to_owned())
            .spawn(move || remove_when_gone(&reads, dir))
            .map_err(|error| format!("scratch folder keeper: {error}"))?;
        Ok(db)
    }

    /// A test handle over `lane` and both read pools.
    fn on_test_lane(path: &Path, lane: WriteLane) -> Result<Self, DbError> {
        let reads = ReadPools::open(
            path,
            lane.database(),
            super::MAX_READERS as usize,
            super::BUSY_TIMEOUT,
            super::ACQUIRE_TIMEOUT,
        )?;
        Ok(Self::new(lane, reads))
    }
}

impl DbConfig {
    /// Other lane time limits, so a test can reach them without waiting out
    /// the real ones.
    #[must_use]
    pub fn with_lane_limits(mut self, limits: LaneLimits) -> Self {
        self.lane_limits = limits;
        self
    }

    /// Lower water marks for the WAL hold, so a test fills the WAL with
    /// kilobytes.
    #[must_use]
    pub fn with_wal_water(mut self, high: i64, low: i64) -> Self {
        self.wal_water = (high, low);
        self
    }

    /// A shorter wait for a pooled connection, so a test reaches `Busy`
    /// without waiting out the real one.
    #[must_use]
    pub fn with_read_wait(mut self, wait: Duration) -> Self {
        self.read_wait = wait;
        self
    }
}

impl CacheDb {
    /// A cache in a scratch folder, removed once the cache's writer and
    /// readers have all closed: a thread holds the folder and looks every
    /// tenth of a second for the cache's shared state to be gone.
    pub fn scratch() -> Self {
        let dir = match ScratchDir::new("provider-cache") {
            Ok(dir) => dir,
            Err(error) => {
                tracing::warn!(%error, "no scratch folder for the provider cache");
                return Self::off(PathBuf::new());
            }
        };
        let cache = Self::open_blocking(dir.path(), &dir.path().join("library.db"));
        // The writer holds the shared state too, so it is gone only once
        // the last write has landed.
        let shared = std::sync::Arc::downgrade(&cache.inner.shared);
        let kept = std::thread::Builder::new()
            .name("droppedneedle-scratch-cache".to_owned())
            .spawn(move || remove_when_gone(&shared, dir));
        if let Err(error) = kept {
            tracing::warn!(%error, "the scratch cache's folder is left behind");
        }
        cache
    }
}

/// Hold `dir` until nothing holds `shared`, then remove it.
fn remove_when_gone<T>(shared: &Weak<T>, dir: ScratchDir) {
    while shared.strong_count() > 0 {
        std::thread::sleep(GONE_POLL);
    }
    drop(dir);
}
