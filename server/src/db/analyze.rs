//! Planner statistics: `ANALYZE` when the catalog has changed and once a day.
//!
//! Without `sqlite_stat1` the planner guesses, and it guesses badly on the
//! low-cardinality indexes the catalog has (`availability`, `kind`): it walks
//! them for full-table filters, which made text-search counts three times
//! slower. This follows Navidrome: a full `ANALYZE`, run one table at a time
//! on the background writer lane so other writes interleave, never
//! `PRAGMA optimize` (its partial analysis misjudges exactly those indexes).
//!
//! It runs when statistics are missing (first boot, and after migrations on
//! an upgraded database), when the streamable track count moved by a tenth
//! since the last run (a scan landed), or when the last run is a day old.
//! The checkpoint loop asks [`AnalyzeService::run_if_due`] on every pass;
//! the check itself runs at most every [`CHECK_INTERVAL`].
//!
//! Reader connections load statistics with the schema and do not notice a
//! re-run on their own, so the last step bumps the schema cookie (a marker
//! table created and dropped), which makes every connection reload.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use super::error::DbError;
use super::writer::{Lane, OpError, WriteLane};

/// Statistics older than this are refreshed.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Least time between two staleness checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Share of the track count that must change to call the stats stale.
const CHURN_SHARE: f64 = 0.1;
/// Fewest changed tracks that count as churn (small libraries move a lot
/// in relative terms without the plans changing).
const CHURN_MIN_TRACKS: i64 = 100;

/// Runs `ANALYZE` on the writer lane when the statistics are stale.
#[derive(Clone, Debug)]
pub struct AnalyzeService {
    lane: WriteLane,
    pool: SqlitePool,
    last_check: Arc<Mutex<Option<Instant>>>,
}

/// What the last run recorded.
#[derive(Debug, Clone, Copy)]
struct LastRun {
    at_unix: i64,
    indexed_tracks: i64,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Whether statistics recorded at `last` are stale now.
fn stale(last: Option<LastRun>, stats_present: bool, indexed_tracks: i64, now_unix: i64) -> bool {
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
    /// Wire the service over the reader pool (staleness checks) and the
    /// writer lane (the `ANALYZE` steps).
    pub fn new(lane: WriteLane, pool: SqlitePool) -> Self {
        Self {
            lane,
            pool,
            last_check: Arc::new(Mutex::new(None)),
        }
    }

    /// Run `ANALYZE` when the statistics are stale. Checks at most every
    /// [`CHECK_INTERVAL`]; returns whether a run happened.
    pub async fn run_if_due(&self) -> Result<bool, DbError> {
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
        let last: Option<(i64, i64)> = sqlx::query_as(
            "SELECT analyzed_at, indexed_tracks FROM db_planner_stats WHERE singleton = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let stats_present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'sqlite_stat1')",
        )
        .fetch_one(&self.pool)
        .await?;
        let indexed_tracks = self.indexed_tracks().await?;
        Ok(stale(
            last.map(|(at_unix, indexed_tracks)| LastRun {
                at_unix,
                indexed_tracks,
            }),
            stats_present,
            indexed_tracks,
            now_unix(),
        ))
    }

    async fn indexed_tracks(&self) -> Result<i64, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT COALESCE(SUM(indexed_tracks), 0) FROM library_track_format_stats",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    /// `ANALYZE` every table, one writer-lane step per table, then record
    /// the run and make reader connections reload the statistics.
    pub async fn run(&self) -> Result<(), DbError> {
        let started = Instant::now();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_schema WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        for table in tables {
            let statement = format!("ANALYZE \"{}\"", table.replace('"', "\"\""));
            self.lane
                .write(Lane::Background, "db.analyze", move |tx| {
                    tx.execute_batch(&statement).map_err(OpError::from)
                })
                .await?;
        }
        let indexed_tracks = self.indexed_tracks().await?;
        let at = now_unix();
        self.lane
            .write(Lane::Background, "db.analyze.record", move |tx| {
                tx.execute(
                    "INSERT INTO db_planner_stats (singleton, analyzed_at, indexed_tracks) \
                     VALUES (1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET \
                     analyzed_at = excluded.analyzed_at, indexed_tracks = excluded.indexed_tracks",
                    rusqlite::params![at, indexed_tracks],
                )?;
                // Bump the schema cookie so pooled readers reload the
                // statistics with the schema on their next statement.
                tx.execute_batch(
                    "CREATE TABLE db_planner_stats_reload (x); DROP TABLE db_planner_stats_reload;",
                )?;
                Ok(())
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
        assert!(stale(None, true, 0, 1_000), "never ran");
        assert!(stale(last, false, 10_000, 1_000), "stats table gone");
        assert!(!stale(last, true, 10_500, 1_000 + day - 1), "fresh");
        assert!(stale(last, true, 10_000, 1_000 + day), "a day old");
        assert!(stale(last, true, 11_000, 1_000), "a scan added a tenth");
        let small = Some(LastRun {
            at_unix: 1_000,
            indexed_tracks: 10,
        });
        assert!(!stale(small, true, 60, 1_000), "small moves are noise");
    }
}
