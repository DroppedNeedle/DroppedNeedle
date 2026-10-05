//! In-flight edition acquires.

use rusqlite::params;

use super::super::error::RequestsError;
use super::super::ledger::EditionMark;
use super::{lane_error, read_error};
use crate::acquire::db::{AcquireDb, to_i64, to_u64};

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
        let row: Option<(String, String, i64)> = sqlx::query_as(
            "SELECT release_group_mbid_lower, task_id, started_at \
             FROM acquire_edition_acquires WHERE release_group_mbid_lower = ?1",
        )
        .bind(key.to_lowercase())
        .fetch_optional(self.db.pool())
        .await
        .map_err(|error| read_error("editions.get", error))?;
        Ok(row.map(|(key, task_id, started_at)| EditionMark {
            key,
            task_id,
            started_at: to_u64(started_at),
        }))
    }

    /// Set one mark.
    pub async fn set(&self, mark: EditionMark) -> Result<(), RequestsError> {
        self.db
            .write("editions.set", move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO acquire_edition_acquires \
                     (release_group_mbid_lower, task_id, started_at) VALUES (?1, ?2, ?3)",
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

    /// Clear one mark.
    pub async fn clear(&self, key: &str) -> Result<(), RequestsError> {
        let key = key.to_lowercase();
        self.db
            .write("editions.clear", move |tx| {
                tx.execute(
                    "DELETE FROM acquire_edition_acquires WHERE release_group_mbid_lower = ?1",
                    params![key],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("editions.clear", error))
    }
}
