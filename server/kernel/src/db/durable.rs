//! The loop liveness rows.
//!
//! `durable_loops` names every background loop the server runs this start,
//! with its state, last heartbeat and the code of its last failure, for the
//! admin jobs view. The loop supervisor in `jobs::supervisor` writes them;
//! the rows are cleared and rebuilt at every start. Writes go through the
//! writer lane's background class, reads through [`Db::read`].

use rusqlite::types::Type;

use super::{error::DbError, handle::Db, sql, writer::Lane};

/// Liveness of a loop row. The table's CHECK still allows `idle`, which
/// nothing writes since every loop runs on the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = LoopState)]
pub enum JobState {
    /// Running now.
    Running,
    /// Stopped cleanly.
    Stopped,
    /// Ended on a panic or a failed one-shot; `last_error` says which.
    Failed,
}

impl JobState {
    /// Row value in `durable_loops`.
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Stopped => "stopped",
            JobState::Failed => "failed",
        }
    }

    /// Parse a row value back into a state.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
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
