//! Planner statistics: `ANALYZE` when the catalog has changed and once a day.
//!
//! Without `sqlite_stat1` the planner guesses, and it guesses badly on the
//! low-cardinality indexes the catalog has (`availability`, `kind`): it walks
//! them for full-table filters, which made text-search counts three times
//! slower. This follows Navidrome: `ANALYZE` one table at a time on the
//! background writer lane so other writes interleave (with a bounded
//! `analysis_limit` sample, skipping the FTS shadow tables), never
//! `PRAGMA optimize`.
//!
//! It runs when statistics are missing (first boot, and after migrations on
//! an upgraded database), when the streamable track count moved by a tenth
//! since the last run (a scan landed), or when the last run is a day old.
//! It never runs while the catalog holds fewer than [`MIN_TRACKS`]: stats
//! taken from a nearly empty catalog would steer a whole first scan onto
//! plans chosen for empty tables, until the next check ten minutes later.
//! The checkpoint loop asks [`AnalyzeService::run_if_due`] on every pass;
//! the check itself runs at most every [`CHECK_INTERVAL`].
//!
//! Reader connections load statistics with the schema and do not notice a
//! re-run on their own, so the last step bumps the schema cookie (a marker
//! table created and dropped), which makes every connection reload.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::clock::Clock;

use super::error::DbError;
use super::handle::Db;
use super::sql;
use super::statement::quoted_identifier;
use super::writer::Lane;

/// Statistics older than this are refreshed.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Least time between two staleness checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Share of the track count that must change to call the stats stale.
const CHURN_SHARE: f64 = 0.1;
/// Rows sampled per index by each ANALYZE step (`PRAGMA analysis_limit`).
const ANALYSIS_LIMIT: u32 = 1000;
/// Fewest streamable tracks before statistics are worth taking.
const MIN_TRACKS: i64 = 100;
/// Fewest changed tracks that count as churn (small libraries move a lot
/// in relative terms without the plans changing).
const CHURN_MIN_TRACKS: i64 = 100;

/// Runs `ANALYZE` on the writer lane when the statistics are stale.
#[derive(Clone)]
pub struct AnalyzeService {
    db: Db,
    clock: Arc<dyn Clock>,
    last_check: Arc<Mutex<Option<Instant>>>,
}

impl std::fmt::Debug for AnalyzeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalyzeService")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

/// What the last run recorded.
#[derive(Debug, Clone, Copy)]
struct LastRun {
    at_unix: i64,
    indexed_tracks: i64,
}

/// Whether statistics recorded at `last` are stale now.
fn stale(last: Option<LastRun>, stats_present: bool, indexed_tracks: i64, now_unix: i64) -> bool {
    if indexed_tracks < MIN_TRACKS {
        return false;
    }
    let Some(last) = last else {
        return true;
    };
    if !stats_present {
        return true;
    }
    if now_unix.saturating_sub(last.at_unix) >= MAX_AGE.as_secs() as i64 {
        return true;
    }
    let moved = (indexed_tracks - last.indexed_tracks).abs();
    moved >= CHURN_MIN_TRACKS && moved as f64 >= last.indexed_tracks.max(1) as f64 * CHURN_SHARE
}

impl AnalyzeService {
    /// Wire the service over the database: the sqlx reader pool for the
    /// staleness checks, the writer lane for the `ANALYZE` steps; `clock`
    /// dates each run.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
            last_check: Arc::new(Mutex::new(None)),
        }
    }

    /// Run `ANALYZE` when the statistics are stale. Checks at most every
    /// [`CHECK_INTERVAL`]; returns whether a run happened.
    ///
    /// Does nothing while the WAL hold is on: a refresh is a burst of writes
    /// into a WAL that cannot shrink. The skipped call does not count as a
    /// check, so the refresh comes with the first pass after the hold.
    pub async fn run_if_due(&self) -> Result<bool, DbError> {
        if self.db.lane().wal_hold().is_on() {
            return Ok(false);
        }
        {
            let mut last = self
                .last_check
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if last.is_some_and(|at| at.elapsed() < CHECK_INTERVAL) {
                return Ok(false);
            }
            *last = Some(Instant::now());
        }
        if !self.due().await? {
            return Ok(false);
        }
        self.run().await?;
        Ok(true)
    }

    async fn due(&self) -> Result<bool, DbError> {
        let last: Option<(i64, i64)> = sqlx::query_as(sql::LAST_ANALYZE.text())
            .fetch_optional(self.db.sqlx())
            .await?;
        let stats_present: bool = sqlx::query_scalar(sql::STATS_PRESENT.text())
            .fetch_one(self.db.sqlx())
            .await?;
        let indexed_tracks = self.indexed_tracks().await?;
        Ok(stale(
            last.map(|(at_unix, indexed_tracks)| LastRun {
                at_unix,
                indexed_tracks,
            }),
            stats_present,
            indexed_tracks,
            self.clock.now_unix(),
        ))
    }

    async fn indexed_tracks(&self) -> Result<i64, DbError> {
        Ok(sqlx::query_scalar(sql::INDEXED_TRACKS.text())
            .fetch_one(self.db.sqlx())
            .await?)
    }

    /// `ANALYZE` every table, one writer-lane step per table, then record
    /// the run and make reader connections reload the statistics.
    pub async fn run(&self) -> Result<(), DbError> {
        let started = Instant::now();
        let tables: Vec<String> = sqlx::query_scalar(sql::ANALYZED_TABLES.text())
            .fetch_all(self.db.sqlx())
            .await?;
        for table in tables {
            // A bounded sample per index keeps each step short on a large
            // catalog; the estimates stay good enough for plan choice.
            let statement = format!(
                "PRAGMA analysis_limit={ANALYSIS_LIMIT}; ANALYZE {}",
                quoted_identifier(&table)
            );
            self.db
                .write(Lane::Background, "db.analyze", move |tx| {
                    tx.execute_batch(&statement).map_err(DbError::from)
                })
                .await?;
        }
        let indexed_tracks = self.indexed_tracks().await?;
        let at = self.clock.now_unix();
        self.db
            .write(Lane::Background, "db.analyze.record", move |tx| {
                tx.execute(
                    sql::RECORD_ANALYZE.text(),
                    rusqlite::params![at, indexed_tracks],
                )?;
                // Bump the schema cookie so pooled readers reload the
                // statistics with the schema on their next statement.
                tx.execute_batch(
                    "CREATE TABLE db_planner_stats_reload (x); DROP TABLE db_planner_stats_reload;",
                )?;
                Ok::<_, DbError>(())
            })
            .await?;
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            indexed_tracks,
            "planner statistics refreshed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staleness_rules() {
        let day = MAX_AGE.as_secs() as i64;
        let last = Some(LastRun {
            at_unix: 1_000,
            indexed_tracks: 10_000,
        });
        assert!(stale(None, true, MIN_TRACKS, 1_000), "never ran");
        assert!(stale(last, false, 10_000, 1_000), "stats table gone");
        assert!(!stale(last, true, 10_500, 1_000 + day - 1), "fresh");
        assert!(stale(last, true, 10_000, 1_000 + day), "a day old");
        assert!(stale(last, true, 11_000, 1_000), "a scan added a tenth");
        let small = Some(LastRun {
            at_unix: 1_000,
            indexed_tracks: 10,
        });
        assert!(!stale(small, true, 109, 1_000), "small moves are noise");
    }
}
