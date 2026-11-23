//! What came of each pick, worked out once a day per person by one job
//! kind ([`KIND`], keyed by the user id), and the record's retention.
//!
//! The job reads only tables catalog may read: the person's
//! `request_history_requesters` rows (when they asked and where from), the
//! albums that own each release group (`owns_group!`, the oldest one that
//! came in once the pick was offered), the person's `library_play_history`
//! in each pick's first 90 days after arrival, and
//! `catalog_discover_feedback`. Catalog never calls acquisition: requests
//! are joined by SQL only.
//!
//! A request or an arrival, once seen, is kept: the row is a record of what
//! happened, and a request row can be replaced by someone else's later
//! ask. Plays and what the person said are worked out whole on every run,
//! so an undone answer or a removed play is gone from the row too.
//!
//! A run is a short write (requests and arrivals), one read on the long
//! pool (the plays, each pick's walk bounded by its window), and a short
//! write (plays, answers, retention) that completes the job. Retention: a
//! row never shown goes 30 days after it was last offered, a row shown
//! goes 13 months after it was last offered or shown, and a row with any
//! outcome (sampled, opened, requested, arrived, played, an answer, let go)
//! stays. A run is safe to repeat.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{CancelFlag, Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use rusqlite::Transaction;

use crate::discover::feedback::FeedbackTarget;
use crate::discover::sql;

/// The outcomes job kind.
pub const KIND: JobKind = JobKind::new("discover.outcomes");

/// How outcomes jobs run: two short writes and one read per person, done
/// rows kept two days since every person with picks gets one a day.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(2 * 24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// Plays counted in the first 30 days after arrival.
const PLAYS_30D_MS: i64 = 30 * DAY_MS;
/// Plays counted at all: the first 90 days after arrival.
const PLAYS_90D_MS: i64 = 90 * DAY_MS;
/// A row never shown goes this long after it was last offered.
pub const UNSHOWN_KEPT_MS: i64 = 30 * DAY_MS;
/// A row shown with no outcome goes this long, 13 months, after it was last
/// offered or shown.
pub const SHOWN_KEPT_MS: i64 = 396 * DAY_MS;

// `requested_at`, `requested_from`, `arrived_at`, `first_played_at`,
// `plays_30d`, `plays_90d`, `feedback_kind` and `feedback_at` are results
// computed by this job only, never kept by triggers or written elsewhere.

/// When `user_id`'s picks were asked for and arrived, on the caller's
/// transaction.
fn requests_and_arrivals_tx(tx: &Transaction, user_id: &str) -> rusqlite::Result<()> {
    tx.execute(sql::PICKS_REQUESTED.text(), rusqlite::params![user_id])?;
    tx.execute(sql::PICKS_ARRIVED.text(), rusqlite::params![user_id])?;
    Ok(())
}

/// `user_id`'s play counts as read, what they told Discover and the
/// retention at `now_ms`, on the caller's transaction. Answers how many rows
/// the retention removed.
fn plays_answers_retention_tx(
    tx: &Transaction,
    user_id: &str,
    plays: &[sql::PickPlaysColumns],
    now_ms: i64,
) -> Result<usize, JobError> {
    let played: Vec<&str> = plays
        .iter()
        .map(|row| row.release_group_mbid_lower.as_str())
        .collect();
    let played = serde_json::to_string(&played).map_err(|_| JobError::Payload)?;
    tx.execute(
        sql::PICK_PLAYS_CLEARED.text(),
        rusqlite::params![user_id, played],
    )?;
    let mut set = tx.prepare_cached(sql::PICK_PLAYS_SET.text())?;
    for row in plays {
        set.execute(rusqlite::params![
            user_id,
            row.release_group_mbid_lower,
            row.first_since,
            row.plays_30d,
            row.plays_90d
        ])?;
    }
    tx.execute(
        sql::PICKS_FEEDBACK.text(),
        rusqlite::params![user_id, FeedbackTarget::Album, FeedbackTarget::Artist],
    )?;
    Ok(tx.execute(
        sql::PICKS_RETAINED.text(),
        rusqlite::params![
            user_id,
            Millis(now_ms - UNSHOWN_KEPT_MS),
            Millis(now_ms - SHOWN_KEPT_MS)
        ],
    )?)
}

/// Each of `user_id`'s arrived picks that was played, with its first play
/// and counts, in one statement on the long pool.
async fn read_plays(db: &Db, user_id: &str) -> Result<Vec<sql::PickPlaysColumns>, DbError> {
    let (db, user_id) = (db.clone(), user_id.to_owned());
    tokio::task::spawn_blocking(move || {
        db.read_long("discover.outcomes.plays", &CancelFlag::never(), |conn| {
            conn.prepare_cached(sql::PICK_PLAYS.text())?
                .query_map(
                    rusqlite::params![user_id, PLAYS_90D_MS, PLAYS_30D_MS],
                    sql::PickPlaysColumns::read,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
    })
    .await
    .map_err(|_| DbError::ReadPanicked {
        operation: "discover.outcomes.plays".to_owned(),
    })?
}

/// Work out `user_id`'s outcomes at `now_ms` and apply the retention;
/// `complete` runs inside the last write. Answers how many rows went.
pub(crate) async fn run(
    db: &Db,
    user_id: &str,
    now_ms: i64,
    complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
) -> Result<usize, JobError> {
    let user = user_id.to_owned();
    db.write(Lane::Background, "discover.outcomes.asked", move |tx| {
        Ok::<_, JobError>(requests_and_arrivals_tx(tx, &user)?)
    })
    .await?;
    let plays = read_plays(db, user_id).await?;
    let user = user_id.to_owned();
    db.write(Lane::Background, "discover.outcomes", move |tx| {
        let removed = plays_answers_retention_tx(tx, &user, &plays, now_ms)?;
        complete(tx)?;
        Ok(removed)
    })
    .await
}

/// Runs [`KIND`] jobs: one person's outcomes, completed in the last write.
pub struct OutcomeJobs {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl OutcomeJobs {
    /// The handler, writing through `db` at `clock`'s time.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }
}

impl JobHandler for OutcomeJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let (db, clock) = (self.db.clone(), Arc::clone(&self.clock));
        Box::pin(async move {
            let user_id = job.key().to_owned();
            let now = clock.now_ms();
            match run(&db, &user_id, now, move |tx| job.complete(tx)).await {
                Ok(0) => Outcome::Done,
                Ok(removed) => {
                    tracing::debug!(removed, "old picks removed from the record");
                    Outcome::Done
                }
                Err(error) if error.is_busy() => Outcome::Busy,
                // Another worker holds the job: its run writes the rows.
                Err(JobError::LeaseLost) => Outcome::Done,
                Err(error) => {
                    tracing::warn!(%error, "pick outcomes not worked out; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_OUTCOMES_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}
