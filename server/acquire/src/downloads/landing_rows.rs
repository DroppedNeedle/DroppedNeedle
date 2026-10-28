//! Journal rows the download import writes: the per-landing decision
//! record. `download_import_decisions` (0025) keeps one row per landing
//! with the outcome and every check's verdict. Held files are in the held
//! list ([`crate::held`]).

use rusqlite::{OptionalExtension, params};

use super::sql;
use super::store::{DownloadStore, StoreError};
use crate::db::{ms_from_secs_f64 as ms, secs_f64_from_ms as secs};
use crate::reason::Reason;

/// One landing's decision, as `download_import_decisions` stores it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportDecisionRow {
    pub task_id: String,
    pub attempt_id: Option<String>,
    /// `imported`, `partial`, `held`, `rejected`, or `deferred`.
    pub outcome: String,
    pub reason_code: Option<Reason>,
    pub detail: Option<String>,
    pub release_mbid: Option<String>,
    pub distance: Option<f64>,
    pub files_total: i64,
    pub files_imported: i64,
    pub files_held: i64,
    /// Every check's verdict, as JSON.
    pub checks_json: String,
    /// `[disc, track]` positions still missing after the landing, as JSON.
    pub missing_positions: String,
    pub decided_at: f64,
}

/// What a task's landings did so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LandingHistory {
    /// Files imported over every landing of the task.
    pub files_imported: i64,
    /// The newest hold's reason, when the newest landing held files.
    pub held_reason: Option<Reason>,
}

impl DownloadStore<'_> {
    /// Record one landing's decision.
    pub fn record_import_decision(&self, row: &ImportDecisionRow) -> Result<(), StoreError> {
        self.conn.execute(
            sql::INSERT_DECISION.text(),
            params![
                row.task_id,
                row.attempt_id,
                row.outcome,
                row.reason_code.map(Reason::code),
                row.detail,
                row.release_mbid,
                row.distance,
                row.files_total,
                row.files_imported,
                row.files_held,
                row.checks_json,
                row.missing_positions,
                ms(row.decided_at),
            ],
        )?;
        Ok(())
    }

    /// What the task's landings imported and held so far.
    pub fn landing_history(&self, task_id: &str) -> Result<LandingHistory, StoreError> {
        let files_imported: i64 =
            self.conn
                .query_row(sql::FILES_IMPORTED.text(), params![task_id], |row| {
                    row.get(0)
                })?;
        let held_reason = self
            .latest_import_decision(task_id)?
            .filter(|row| row.outcome == "held")
            .and_then(|row| row.reason_code);
        Ok(LandingHistory {
            files_imported,
            held_reason,
        })
    }

    /// The release positions the newest landing found missing, when it
    /// landed short: a failover asks the next source for these only.
    pub fn missing_positions(&self, task_id: &str) -> Result<Vec<(u32, u32)>, StoreError> {
        let Some(row) = self.latest_import_decision(task_id)? else {
            return Ok(Vec::new());
        };
        if !matches!(row.outcome.as_str(), "partial" | "held") {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&row.missing_positions).unwrap_or_default())
    }

    /// The wrong-product verdict: the album's files all named different
    /// content. The first verdict stands.
    pub fn record_wrong_product_verdict(
        &self,
        task_id: &str,
        detail: &str,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            sql::SET_WRONG_PRODUCT.text(),
            params![ms(now), detail, task_id],
        )?;
        Ok(())
    }

    /// Clear a task's wrong-product verdict once none of its files is held.
    pub fn clear_wrong_product_verdict(&self, task_id: &str) -> Result<(), StoreError> {
        self.conn
            .execute(sql::CLEAR_WRONG_PRODUCT.text(), params![task_id])?;
        Ok(())
    }

    /// A person imported held files and the album is now whole: a failed
    /// or partial task becomes completed, so it stops waiting on a retry
    /// it no longer needs. Answers
    /// whether the task moved.
    pub fn complete_after_manual_import(
        &self,
        task_id: &str,
        now: f64,
    ) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            sql::COMPLETE_AFTER_MANUAL_IMPORT.text(),
            params![ms(now), task_id],
        )?;
        if changed > 0 {
            self.settle_requests(task_id, super::state::TaskStatus::Completed, now)?;
        }
        Ok(changed > 0)
    }

    /// The newest decision recorded for a task.
    pub fn latest_import_decision(
        &self,
        task_id: &str,
    ) -> Result<Option<ImportDecisionRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::LATEST_DECISION.text(),
                params![task_id],
                ImportDecisionRow::from_row,
            )
            .optional()?)
    }

    /// The newest landing of a task that imported files (`imported` or
    /// `partial`). Its missing positions are what the release still lacked
    /// after it, earlier landings included.
    pub fn latest_import(&self, task_id: &str) -> Result<Option<ImportDecisionRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::LATEST_IMPORT.text(),
                params![task_id],
                ImportDecisionRow::from_row,
            )
            .optional()?)
    }

    /// The newest decision one attempt (`None` for a landing with no
    /// attempt) reached on a task, deferrals aside: a landing that waited
    /// never hides what an earlier landing of the attempt imported.
    pub fn last_landing_of_attempt(
        &self,
        task_id: &str,
        attempt_id: Option<&str>,
    ) -> Result<Option<ImportDecisionRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                sql::LAST_LANDING_OF_ATTEMPT.text(),
                params![task_id, attempt_id],
                ImportDecisionRow::from_row,
            )
            .optional()?)
    }
}

impl ImportDecisionRow {
    /// Decode one `download_import_decisions` row in the column order of
    /// the statements that read it.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            task_id: row.get(0)?,
            attempt_id: row.get(1)?,
            outcome: row.get(2)?,
            reason_code: row
                .get::<_, Option<String>>(3)?
                .map(|code| Reason::from_stored(&code)),
            detail: row.get(4)?,
            release_mbid: row.get(5)?,
            distance: row.get(6)?,
            files_total: row.get(7)?,
            files_imported: row.get(8)?,
            files_held: row.get(9)?,
            checks_json: row.get(10)?,
            missing_positions: row.get(11)?,
            decided_at: secs(row.get(12)?),
        })
    }
}
