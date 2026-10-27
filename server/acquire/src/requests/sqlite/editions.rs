//! In-flight edition acquires.

use rusqlite::{OptionalExtension, params};

use super::super::error::RequestsError;
use super::super::ledger::EditionMark;
use super::super::sql;
use super::{lane_error, read};
use crate::db::{AcquireDb, to_i64, to_u64};

/// Durable in-flight edition acquires.
#[derive(Clone)]
pub struct EditionStore {
    db: AcquireDb,
}

impl EditionStore {
    /// Marks over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Read one mark.
    pub async fn get(&self, key: &str) -> Result<Option<EditionMark>, RequestsError> {
        let key = key.to_lowercase();
        read(&self.db, "editions.get", move |conn| {
            conn.query_row(sql::EDITION_MARK.text(), params![key], |row| {
                Ok(EditionMark {
                    key: row.get(0)?,
                    task_id: row.get(1)?,
                    started_at: to_u64(row.get(2)?),
                })
            })
            .optional()
        })
        .await
    }

    /// Set one mark.
    pub async fn set(&self, mark: EditionMark) -> Result<(), RequestsError> {
        self.db
            .write("editions.set", move |tx| {
                tx.execute(
                    sql::SET_EDITION_MARK.text(),
                    params![
                        mark.key.to_lowercase(),
                        mark.task_id,
                        to_i64(mark.started_at)
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("editions.set", error))
    }
}
