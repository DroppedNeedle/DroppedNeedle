//! The refresh job and the hourly tick that queues it.
//!
//! One job kind keyed by person and day, so each person is refreshed at most
//! once a day by the tick; turning lists on rotation on queues one at once.
//! The tick looks only at people seen in the last 14 days whose rows were
//! not all looked at today, so an idle server writes nothing.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};

use droppedneedle_kernel::jobs::queue::{
    JobCtx, JobError, JobHandler, JobKind, JobQueue, KindSpec, Outcome,
};
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};

use super::{DAY_MS, refresh, store};
use crate::collections::db::StoreError;
use crate::collections::state::CollectionsState;

/// The refresh job kind.
pub const REFRESH: JobKind = JobKind::new("playlist.rotation");

/// How refresh jobs run: reads and one write on this database, no outside
/// call, so a few tries a minute apart are enough.
pub const SPEC: KindSpec = KindSpec::new(REFRESH)
    .max_attempts(5)
    .backoff(Duration::from_secs(60), Duration::from_secs(3600))
    .run_timeout(Duration::from_secs(10 * 60))
    .keep_done(Duration::from_secs(2 * 24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// The tick's name in the jobs view.
pub const TICK_NAME: &str = "catalog-rotation-due";

/// Who counts as active: seen in the last 14 days.
const SEEN_WINDOW_MS: i64 = 14 * DAY_MS;

/// What one refresh job carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshJob {
    /// The person whose lists it refreshes.
    pub user_id: String,
}

/// The job key for one person on the day `now_ms` falls in (UTC).
pub fn refresh_key(user_id: &str, now_ms: i64) -> String {
    let day = jiff::Timestamp::from_millisecond(now_ms)
        .map(|at| at.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
        .unwrap_or_else(|_| now_ms.div_euclid(DAY_MS).to_string());
    format!("{user_id}:{day}")
}

/// Queue a refresh of `user_id` today, on the caller's transaction.
pub(crate) fn queue_refresh(
    queue: &JobQueue,
    tx: &Transaction,
    user_id: &str,
    now_ms: i64,
) -> Result<(), JobError> {
    let job = RefreshJob {
        user_id: user_id.to_owned(),
    };
    queue.enqueue(tx, REFRESH, &refresh_key(user_id, now_ms), &job)?;
    Ok(())
}

/// Runs refresh jobs.
#[derive(Clone)]
pub struct RefreshJobs {
    state: CollectionsState,
}

impl RefreshJobs {
    /// The handler over the collections state.
    pub fn new(state: CollectionsState) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

impl JobHandler for RefreshJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let state = self.state.clone();
        Box::pin(async move {
            let Ok(RefreshJob { user_id }) = job.payload::<RefreshJob>() else {
                return Outcome::Fail {
                    code: droppedneedle_kernel::jobs::queue::PAYLOAD_INVALID,
                };
            };
            let now = state.db.now_ms();
            match refresh(&state, &user_id, now, move |tx| Ok(job.complete(tx)?)).await {
                Ok(written) => {
                    if written != store::Written::default() {
                        tracing::info!(
                            created = written.created,
                            replaced = written.replaced,
                            dropped = written.dropped,
                            "lists on rotation refreshed"
                        );
                    }
                    Outcome::Done
                }
                Err(StoreError::Busy(_)) => Outcome::Busy,
                Err(error) => {
                    tracing::warn!(%error, "lists on rotation not refreshed; trying again");
                    Outcome::Retry {
                        code: "ROTATION_REFRESH_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}

/// Start the hourly tick on `supervisor`. Its first look comes about a
/// minute after start: a restart queues nobody already looked at today, so
/// waiting longer would only starve a server that restarts often.
pub fn start(state: &CollectionsState, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        TICK_NAME,
        Schedule::new(Duration::from_secs(3600))
            .with_initial_delay(Duration::from_secs(60))
            .with_jitter(Duration::from_secs(60))
            .with_jittered_start()
            .with_backoff(Duration::from_secs(60), Duration::from_secs(3600)),
        DueTick {
            state: state.clone(),
        },
    )
}

struct DueTick {
    state: CollectionsState,
}

impl Tick for DueTick {
    /// A failed read or write is logged and the next hour tries again.
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        let now = self.state.db.now_ms();
        let db = self.state.db.database();
        let seen = match droppedneedle_auth::sqlite::seen_since(db, now - SEEN_WINDOW_MS).await {
            Ok(seen) => seen,
            Err(error) => {
                tracing::warn!(%error, "who was seen is unreadable; no lists on rotation queued");
                return Ok(Next::Continue);
            }
        };
        if seen.is_empty() {
            return Ok(Next::Continue);
        }
        let today = now.div_euclid(DAY_MS) * DAY_MS;
        let due = match store::due(&self.state.db, seen, today).await {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(%error, "who is due lists on rotation is unreadable");
                return Ok(Next::Continue);
            }
        };
        if due.is_empty() {
            return Ok(Next::Continue);
        }
        let queue = self.state.jobs.clone();
        let queued = self
            .state
            .db
            .write_background("rotation.queue", move |tx| {
                for user_id in &due {
                    queue_refresh(&queue, tx, user_id, now)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = queued {
            tracing::warn!(%error, "lists on rotation not queued; trying next hour");
        }
        Ok(Next::Continue)
    }
}
