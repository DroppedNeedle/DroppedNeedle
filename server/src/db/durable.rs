//! Durable-worker fabric: wakeup channels and the job registry.
//!
//! Four in-process channels (scan, identification, operation, contribution)
//! wake background workers; each request also bumps a sequence row in
//! `durable_work_wakeups` so restarts and the admin health endpoint can see
//! pending demand. The `durable_job_registry` names every background job,
//! durable or ephemeral, with its liveness. Workers consume sequence numbers
//! so a wakeup is never lost between request and pickup.
//!
//! Consumers live in `acquire`, `library`, and `jobs`; this module is the
//! fabric they share. All table writes go through the [`WriteLane`]; reads use the pool.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;
use tokio::sync::Notify;

use super::{
    error::DbError,
    writer::{Lane, OpError, WriteLane},
};

/// Background work channel. Matches the `durable_work_wakeups` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WakeupChannel {
    /// Library scanning.
    Scan,
    /// Release and track identification.
    Identification,
    /// Durable library operations.
    Operation,
    /// Library contributions.
    Contribution,
}

impl WakeupChannel {
    /// All four channels in registry order.
    pub const ALL: [WakeupChannel; 4] = [
        WakeupChannel::Scan,
        WakeupChannel::Identification,
        WakeupChannel::Operation,
        WakeupChannel::Contribution,
    ];

    /// Row key in `durable_work_wakeups`.
    pub fn as_str(self) -> &'static str {
        match self {
            WakeupChannel::Scan => "scan",
            WakeupChannel::Identification => "identification",
            WakeupChannel::Operation => "operation",
            WakeupChannel::Contribution => "contribution",
        }
    }

    /// Parse a row key back into a channel.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "scan" => Some(WakeupChannel::Scan),
            "identification" => Some(WakeupChannel::Identification),
            "operation" => Some(WakeupChannel::Operation),
            "contribution" => Some(WakeupChannel::Contribution),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            WakeupChannel::Scan => 0,
            WakeupChannel::Identification => 1,
            WakeupChannel::Operation => 2,
            WakeupChannel::Contribution => 3,
        }
    }
}

/// What kind of background job a registry row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// SQLite state machine with startup recovery.
    Durable,
    /// Maintenance loop, safe to drop on restart.
    Ephemeral,
}

impl JobKind {
    /// Row value in `durable_job_registry`.
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Durable => "durable",
            JobKind::Ephemeral => "ephemeral",
        }
    }

    /// Parse a row value back into a kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "durable" => Some(JobKind::Durable),
            "ephemeral" => Some(JobKind::Ephemeral),
            _ => None,
        }
    }
}

/// Liveness of a registry row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Registered, not currently running.
    Idle,
    /// Running now.
    Running,
    /// Stopped cleanly.
    Stopped,
    /// Stopped on failure; the domain tables hold the details.
    Failed,
}

impl JobState {
    /// Row value in `durable_job_registry`.
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Idle => "idle",
            JobState::Running => "running",
            JobState::Stopped => "stopped",
            JobState::Failed => "failed",
        }
    }

    /// Parse a row value back into a state.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(JobState::Idle),
            "running" => Some(JobState::Running),
            "stopped" => Some(JobState::Stopped),
            "failed" => Some(JobState::Failed),
            _ => None,
        }
    }
}

/// One registry row.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    /// Job name, the primary key.
    pub name: String,
    /// Durable or ephemeral.
    pub kind: JobKind,
    /// Channel that wakes this job, if any.
    pub wakeup_channel: Option<WakeupChannel>,
    /// Current liveness.
    pub state: JobState,
    /// Last heartbeat as unix seconds, if the job ever beat.
    pub last_heartbeat_at: Option<f64>,
    /// Last registry update as unix seconds.
    pub updated_at: f64,
}

/// Wakeup channels over the pool plus in-process signals. Cloneable handle.
#[derive(Clone, Debug)]
pub struct DurableWorkWakeups {
    pool: SqlitePool,
    signals: Arc<[tokio::sync::Notify; 4]>,
}

impl DurableWorkWakeups {
    /// Wire the fabric over a migrated pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            signals: Arc::new([Notify::new(), Notify::new(), Notify::new(), Notify::new()]),
        }
    }

    /// Request work on a channel: bump the sequence row through the writer
    /// lane, then wake the sleeper. Returns the new sequence number.
    /// `notify_one` stores a permit when nobody is waiting, so a request
    /// that lands before `wait` still wakes it; the seq row is the real
    /// queue, so coalesced signals lose nothing. One worker per channel.
    pub async fn request(
        &self,
        lane: &WriteLane,
        lane_kind: Lane,
        channel: WakeupChannel,
    ) -> Result<i64, DbError> {
        let key = channel.as_str().to_owned();
        let seq = lane
            .write(lane_kind, "durable-wakeup-request", move |tx| {
                tx.execute(
                    "UPDATE durable_work_wakeups
                     SET seq = seq + 1, requested_at = ?1, updated_at = ?1
                     WHERE channel = ?2",
                    rusqlite::params![now_unix(), key],
                )?;
                let seq: i64 = tx.query_row(
                    "SELECT seq FROM durable_work_wakeups WHERE channel = ?1",
                    rusqlite::params![channel.as_str()],
                    |row| row.get(0),
                )?;
                Ok(seq)
            })
            .await
            .map_err(|error| map_missing_table(error, "durable_work_wakeups"))?;
        self.signals[channel.index()].notify_one();
        Ok(seq)
    }

    /// Sleep until `request` wakes this channel. The stored permit means a
    /// request issued before this call returns at once; callers still
    /// re-check `pending` on the way out and consume through `consume`.
    pub async fn wait(&self, channel: WakeupChannel) {
        self.signals[channel.index()].notified().await;
    }

    /// True when requested work is still unconsumed on this channel.
    pub async fn pending(&self, channel: WakeupChannel) -> Result<bool, DbError> {
        let row: (i64, i64) =
            sqlx::query_as("SELECT seq, consumed_seq FROM durable_work_wakeups WHERE channel = ?1")
                .bind(channel.as_str())
                .fetch_one(&self.pool)
                .await?;
        Ok(row.0 > row.1)
    }

    /// Mark work consumed through a sequence number. Monotonic: an older
    /// `through` never moves the marker back.
    pub async fn consume(
        &self,
        lane: &WriteLane,
        lane_kind: Lane,
        channel: WakeupChannel,
        through_seq: i64,
    ) -> Result<(), DbError> {
        let key = channel.as_str().to_owned();
        lane.write(lane_kind, "durable-wakeup-consume", move |tx| {
            tx.execute(
                "UPDATE durable_work_wakeups
                 SET consumed_seq = MAX(consumed_seq, ?1), updated_at = ?2
                 WHERE channel = ?3",
                rusqlite::params![through_seq, now_unix(), key],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| map_missing_table(error, "durable_work_wakeups"))?;
        Ok(())
    }

    /// Register a job, or refresh the registration if the name exists.
    pub async fn register_job(
        &self,
        lane: &WriteLane,
        name: &str,
        kind: JobKind,
        wakeup_channel: Option<WakeupChannel>,
    ) -> Result<(), DbError> {
        let name = name.to_owned();
        let kind_text = kind.as_str().to_owned();
        let channel_text = wakeup_channel.map(WakeupChannel::as_str).map(str::to_owned);
        lane.write(Lane::Background, "durable-job-register", move |tx| {
            tx.execute(
                "INSERT INTO durable_job_registry
                     (name, kind, wakeup_channel, state, updated_at)
                 VALUES (?1, ?2, ?3, 'idle', ?4)
                 ON CONFLICT (name) DO UPDATE SET
                     kind = excluded.kind,
                     wakeup_channel = excluded.wakeup_channel,
                     updated_at = excluded.updated_at",
                rusqlite::params![name, kind_text, channel_text, now_unix()],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| map_missing_table(error, "durable_job_registry"))?;
        Ok(())
    }

    /// Move a job to a new liveness state.
    pub async fn set_job_state(
        &self,
        lane: &WriteLane,
        name: &str,
        state: JobState,
    ) -> Result<(), DbError> {
        let name = name.to_owned();
        let state_text = state.as_str().to_owned();
        lane.write(Lane::Background, "durable-job-state", move |tx| {
            let changed = tx.execute(
                "UPDATE durable_job_registry
                 SET state = ?1, updated_at = ?2 WHERE name = ?3",
                rusqlite::params![state_text, now_unix(), name],
            )?;
            if changed == 0 {
                return Err(OpError::Abort("unknown job".to_owned()));
            }
            Ok(())
        })
        .await
        .map_err(|error| map_missing_table(error, "durable_job_registry"))?;
        Ok(())
    }

    /// Beat a job's heart without changing its state.
    pub async fn heartbeat(&self, lane: &WriteLane, name: &str) -> Result<(), DbError> {
        let name = name.to_owned();
        lane.write(Lane::Background, "durable-job-heartbeat", move |tx| {
            let changed = tx.execute(
                "UPDATE durable_job_registry
                 SET last_heartbeat_at = ?1, updated_at = ?1 WHERE name = ?2",
                rusqlite::params![now_unix(), name],
            )?;
            if changed == 0 {
                return Err(OpError::Abort("unknown job".to_owned()));
            }
            Ok(())
        })
        .await
        .map_err(|error| map_missing_table(error, "durable_job_registry"))?;
        Ok(())
    }

    /// Read one registry row, or `None` when the name is unregistered.
    pub async fn get_job(&self, name: &str) -> Result<Option<JobRecord>, DbError> {
        let row: Option<JobRow> = sqlx::query_as(
            "SELECT name, kind, wakeup_channel, state, last_heartbeat_at, updated_at
                 FROM durable_job_registry WHERE name = ?1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(decode_job))
    }

    /// Read the whole registry in name order.
    pub async fn list_jobs(&self) -> Result<Vec<JobRecord>, DbError> {
        let rows: Vec<JobRow> = sqlx::query_as(
            "SELECT name, kind, wakeup_channel, state, last_heartbeat_at, updated_at
                 FROM durable_job_registry ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(decode_job).collect())
    }
}

/// One raw registry row: name, kind, channel, state, heartbeat, updated.
type JobRow = (String, String, Option<String>, String, Option<f64>, f64);

/// Decode a registry row; CHECK constraints make `None` unreachable on a
/// migrated database, but a corrupt row decodes to nothing instead of
/// failing the whole listing.
fn decode_job(row: JobRow) -> Option<JobRecord> {
    let channel = match row.2.as_deref() {
        None => None,
        Some(text) => Some(WakeupChannel::parse(text)?),
    };
    Some(JobRecord {
        name: row.0,
        kind: JobKind::parse(&row.1)?,
        wakeup_channel: channel,
        state: JobState::parse(&row.3)?,
        last_heartbeat_at: row.4,
        updated_at: row.5,
    })
}

/// Current wall-clock as unix seconds for the REAL timestamp columns.
fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Reword a missing-table failure so an unmigrated database reads plainly.
fn map_missing_table(error: DbError, table: &str) -> DbError {
    match &error {
        DbError::WriteFailed { cause, .. } if cause.contains("no such table") => {
            DbError::WriteFailed {
                operation: format!("durable fabric needs migrated table {table}"),
                cause: cause.clone(),
            }
        }
        _ => error,
    }
}
