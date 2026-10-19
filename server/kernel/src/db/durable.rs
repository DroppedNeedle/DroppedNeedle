//! The loop liveness rows.
//!
//! `durable_loops` names every background loop the server runs this start,
//! with its state, last heartbeat and the code of its last failure, for the
//! admin jobs view. The loop supervisor in `jobs::supervisor` writes them;
//! the rows are cleared and rebuilt at every start. Writes go through the
//! writer lane's background class, reads through [`Db::read`].

use std::fmt;
use std::sync::Arc;

use rusqlite::types::Type;

use super::{error::DbError, handle::Db, sql, writer::Lane};
use crate::clock::Clock;

/// Liveness of a loop row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Registered, not running.
    Idle,
    /// Running now.
    Running,
    /// Stopped cleanly.
    Stopped,
    /// Ended on a panic or an abort; `last_error` says which.
    Failed,
}

impl JobState {
    /// Row value in `durable_loops`.
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

/// One loop row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRecord {
    /// Loop name, the primary key.
    pub name: String,
    /// Current liveness.
    pub state: JobState,
    /// Last heartbeat as unix milliseconds, if the loop ever beat.
    pub last_heartbeat_at: Option<i64>,
    /// Fixed code of the last failure, cleared by the next clean tick.
    pub last_error: Option<String>,
    /// Last change as unix milliseconds.
    pub updated_at: i64,
}

/// Forget every loop row.
pub(crate) async fn clear_loops(db: &Db) -> Result<(), DbError> {
    db.write(Lane::Background, "durable-loops-clear", |tx| {
        tx.execute(sql::CLEAR_LOOPS.text(), [])?;
        Ok(())
    })
    .await
}

/// Write each row as given, in one transaction.
pub(crate) async fn put_loops(db: &Db, rows: Vec<LoopRecord>) -> Result<(), DbError> {
    db.write(Lane::Background, "durable-loops-put", move |tx| {
        let mut statement = tx.prepare_cached(sql::PUT_LOOP.text())?;
        for row in &rows {
            statement.execute(rusqlite::params![
                row.name,
                row.state.as_str(),
                row.last_heartbeat_at,
                row.last_error,
                row.updated_at,
            ])?;
        }
        Ok(())
    })
    .await
}

/// Every loop row in name order.
pub async fn list_loops(db: &Db) -> Result<Vec<LoopRecord>, DbError> {
    db.read("durable-loops-list", |conn| {
        let mut statement = conn.prepare_cached(sql::ALL_LOOPS.text())?;
        let rows = statement.query_map([], |row| {
            let state: String = row.get(1)?;
            let state = JobState::parse(&state).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    Type::Text,
                    format!("unknown loop state {state:?}").into(),
                )
            })?;
            Ok(LoopRecord {
                name: row.get(0)?,
                state,
                last_heartbeat_at: row.get(2)?,
                last_error: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .await
}

/// Rows for loops that run outside the supervisor and write their own
/// liveness. Made only by a started supervisor, so no row can be written
/// before the start clears the table.
#[derive(Clone)]
pub struct JobRegistryRows {
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for JobRegistryRows {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobRegistryRows")
            .finish_non_exhaustive()
    }
}

impl JobRegistryRows {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self { clock }
    }

    /// Register a loop as idle, keeping the state of a row already there.
    pub async fn register_job(&self, db: &Db, name: &str) -> Result<(), DbError> {
        let name = name.to_owned();
        let now = self.clock.now_ms();
        db.write(Lane::Background, "durable-loop-register", move |tx| {
            tx.execute(sql::REGISTER_LOOP.text(), rusqlite::params![name, now])?;
            Ok(())
        })
        .await
    }

    /// Move a registered loop to a new state.
    pub async fn set_job_state(&self, db: &Db, name: &str, state: JobState) -> Result<(), DbError> {
        let name = name.to_owned();
        let now = self.clock.now_ms();
        db.write(Lane::Background, "durable-loop-state", move |tx| {
            let changed = tx.execute(
                sql::SET_LOOP_STATE.text(),
                rusqlite::params![state.as_str(), now, name],
            )?;
            if changed == 0 {
                return Err(DbError::WriteFailed {
                    operation: "durable-loop-state".to_owned(),
                    cause: "unknown loop".to_owned(),
                });
            }
            Ok(())
        })
        .await
    }

    /// Beat a registered loop's heart without changing its state.
    pub async fn heartbeat(&self, db: &Db, name: &str) -> Result<(), DbError> {
        let name = name.to_owned();
        let now = self.clock.now_ms();
        db.write(Lane::Background, "durable-loop-heartbeat", move |tx| {
            let changed = tx.execute(sql::BEAT_LOOP.text(), rusqlite::params![now, name])?;
            if changed == 0 {
                return Err(DbError::WriteFailed {
                    operation: "durable-loop-heartbeat".to_owned(),
                    cause: "unknown loop".to_owned(),
                });
            }
            Ok(())
        })
        .await
    }
}
