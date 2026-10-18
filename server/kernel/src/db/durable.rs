//! The job registry's rows.
//!
//! `durable_job_registry` names every background job, durable or ephemeral,
//! with its liveness and last heartbeat, for the admin queue view. The live
//! handles sit in `jobs::registry`; this module reads and writes the rows
//! behind them. Writes go through the writer lane of a [`Db`]; reads use the
//! pool.

use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::SqlitePool;

use super::{error::DbError, handle::Db, writer::Lane};

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
    /// Current liveness.
    pub state: JobState,
    /// Last heartbeat as unix seconds, if the job ever beat.
    pub last_heartbeat_at: Option<f64>,
    /// Last registry update as unix seconds.
    pub updated_at: f64,
}

/// The registry rows over the pool. Cloneable handle.
#[derive(Clone, Debug)]
pub struct JobRegistryRows {
    pool: SqlitePool,
}

impl JobRegistryRows {
    /// Read and write the rows of a migrated pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Register a job, or refresh the registration if the name exists.
    pub async fn register_job(&self, db: &Db, name: &str, kind: JobKind) -> Result<(), DbError> {
        let name = name.to_owned();
        let kind_text = kind.as_str().to_owned();
        db.write(Lane::Background, "durable-job-register", move |tx| {
            tx.execute(
                "INSERT INTO durable_job_registry (name, kind, state, updated_at)
                 VALUES (?1, ?2, 'idle', ?3)
                 ON CONFLICT (name) DO UPDATE SET
                     kind = excluded.kind,
                     updated_at = excluded.updated_at",
                rusqlite::params![name, kind_text, now_unix()],
            )?;
            Ok(())
        })
        .await
    }

    /// Move a job to a new liveness state.
    pub async fn set_job_state(&self, db: &Db, name: &str, state: JobState) -> Result<(), DbError> {
        let name = name.to_owned();
        let state_text = state.as_str().to_owned();
        db.write(Lane::Background, "durable-job-state", move |tx| {
            let changed = tx.execute(
                "UPDATE durable_job_registry
                 SET state = ?1, updated_at = ?2 WHERE name = ?3",
                rusqlite::params![state_text, now_unix(), name],
            )?;
            if changed == 0 {
                return Err(DbError::WriteFailed {
                    operation: "durable-job-state".to_owned(),
                    cause: "unknown job".to_owned(),
                });
            }
            Ok(())
        })
        .await
    }

    /// Beat a job's heart without changing its state.
    pub async fn heartbeat(&self, db: &Db, name: &str) -> Result<(), DbError> {
        let name = name.to_owned();
        db.write(Lane::Background, "durable-job-heartbeat", move |tx| {
            let changed = tx.execute(
                "UPDATE durable_job_registry
                 SET last_heartbeat_at = ?1, updated_at = ?1 WHERE name = ?2",
                rusqlite::params![now_unix(), name],
            )?;
            if changed == 0 {
                return Err(DbError::WriteFailed {
                    operation: "durable-job-heartbeat".to_owned(),
                    cause: "unknown job".to_owned(),
                });
            }
            Ok(())
        })
        .await
    }

    /// Read one registry row, or `None` when the name is unregistered.
    pub async fn get_job(&self, name: &str) -> Result<Option<JobRecord>, DbError> {
        let row: Option<JobRow> = sqlx::query_as(
            "SELECT name, kind, state, last_heartbeat_at, updated_at
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
            "SELECT name, kind, state, last_heartbeat_at, updated_at
                 FROM durable_job_registry ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(decode_job).collect())
    }
}

/// One raw registry row: name, kind, state, heartbeat, updated.
type JobRow = (String, String, String, Option<f64>, f64);

/// Decode a registry row; CHECK constraints make `None` unreachable on a
/// migrated database, but a corrupt row decodes to nothing instead of
/// failing the whole listing.
fn decode_job(row: JobRow) -> Option<JobRecord> {
    Some(JobRecord {
        name: row.0,
        kind: JobKind::parse(&row.1)?,
        state: JobState::parse(&row.2)?,
        last_heartbeat_at: row.3,
        updated_at: row.4,
    })
}

/// Current wall-clock as unix seconds for the REAL timestamp columns.
fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
