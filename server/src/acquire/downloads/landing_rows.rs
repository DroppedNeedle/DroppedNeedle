//! Journal rows the download import writes: held files and the per-landing
//! decision record.
//!
//! `held_imports` (migration 0001) keeps one row per file a person must
//! look at, with the evidence the rejecting check saw; the held-import
//! views read it. `download_import_decisions` (0025) keeps one row per
//! landing with the outcome and every check's verdict.

use rusqlite::{OptionalExtension, params};

use super::store::{DownloadStore, StoreError};

/// One held file, as `held_imports` stores it.
#[derive(Debug, Clone, Default)]
pub struct HeldFile {
    pub user_id: String,
    /// Where the held copy lives.
    pub held_path: String,
    pub original_filename: String,
    /// Machine reason (`tag_mismatch`, `weak_match`, ...).
    pub reason: String,
    /// The reason as a plain sentence, and what to do about it.
    pub reason_text: Option<String>,
    pub reason_action: Option<String>,
    pub reason_detail: Option<String>,
    /// `soulseek`, `usenet`, or `plugin:<key>`.
    pub source: String,
    pub source_task_id: String,
    pub origin: String,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_title: Option<String>,
    pub artist_name: Option<String>,
    pub artist_mbid: Option<String>,
    pub album_title: Option<String>,
    pub year: Option<i64>,
    pub file_format: Option<String>,
    pub duration_seconds: Option<f64>,
    pub expected_duration_seconds: Option<f64>,
    /// What the file's own tags say it is.
    pub evidence_title: Option<String>,
    pub evidence_artist: Option<String>,
    pub evidence_score: Option<f64>,
}

/// One landing's decision, as `download_import_decisions` stores it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportDecisionRow {
    pub task_id: String,
    pub attempt_id: Option<String>,
    /// `imported`, `partial`, `held`, `rejected`, or `deferred`.
    pub outcome: String,
    pub reason_code: Option<String>,
    /// The reason as a plain sentence, and what to do about it.
    pub reason_text: Option<String>,
    pub reason_action: Option<String>,
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
    /// The newest hold's reason as a plain sentence, when the newest
    /// landing held files.
    pub held_detail: Option<String>,
}

impl DownloadStore<'_> {
    /// The task's held row for the same release track (v2: one held copy
    /// per track, whichever candidate brought it) or, for a file paired to
    /// no track, the same file.
    pub fn held_row_for(&self, held: &HeldFile) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM held_imports WHERE source_task_id = ?1 AND status = 'held' \
                 AND CASE WHEN ?2 IS NOT NULL THEN release_track_mbid = ?2 \
                 ELSE release_track_mbid IS NULL AND original_filename = ?3 END \
                 LIMIT 1",
                params![
                    held.source_task_id,
                    held.release_track_mbid,
                    held.original_filename
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Record one held file. `None` when the task already holds this track
    /// or file (a resumed landing, or a later candidate), so the caller
    /// drops its copy.
    pub fn record_held_file(&self, held: &HeldFile, now: f64) -> Result<Option<i64>, StoreError> {
        if self.held_row_for(held)?.is_some() {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO held_imports (user_id, release_group_mbid, release_mbid, \
             release_track_mbid, recording_mbid, track_number, disc_number, track_title, \
             artist_name, artist_mbid, album_title, year, held_path, original_filename, \
             file_format, duration_seconds, expected_duration_seconds, reason, reason_detail, \
             evidence_title, evidence_artist, evidence_score, source, source_task_id, origin, \
             reason_text, reason_action, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
             ?, ?, 'held', ?)",
            params![
                held.user_id,
                held.release_group_mbid,
                held.release_mbid,
                held.release_track_mbid,
                held.recording_mbid,
                held.track_number,
                held.disc_number,
                held.track_title,
                held.artist_name,
                held.artist_mbid,
                held.album_title,
                held.year,
                held.held_path,
                held.original_filename,
                held.file_format,
                held.duration_seconds,
                held.expected_duration_seconds,
                held.reason,
                held.reason_detail,
                held.evidence_title,
                held.evidence_artist,
                held.evidence_score,
                held.source,
                held.source_task_id,
                held.origin,
                held.reason_text,
                held.reason_action,
                now,
            ],
        )?;
        Ok(Some(self.conn.last_insert_rowid()))
    }

    /// Record one landing's decision.
    pub fn record_import_decision(&self, row: &ImportDecisionRow) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO download_import_decisions (task_id, attempt_id, outcome, reason_code, \
             reason_text, reason_action, detail, release_mbid, distance, files_total, \
             files_imported, files_held, checks_json, missing_positions, decided_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                row.task_id,
                row.attempt_id,
                row.outcome,
                row.reason_code,
                row.reason_text,
                row.reason_action,
                row.detail,
                row.release_mbid,
                row.distance,
                row.files_total,
                row.files_imported,
                row.files_held,
                row.checks_json,
                row.missing_positions,
                row.decided_at,
            ],
        )?;
        Ok(())
    }

    /// What the task's landings imported and held so far.
    pub fn landing_history(&self, task_id: &str) -> Result<LandingHistory, StoreError> {
        let files_imported: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(files_imported), 0) FROM download_import_decisions \
             WHERE task_id = ?",
            params![task_id],
            |row| row.get(0),
        )?;
        let held_detail = self
            .latest_import_decision(task_id)?
            .filter(|row| row.outcome == "held")
            .map(|row| row.reason_text.or(row.detail).unwrap_or_default());
        Ok(LandingHistory {
            files_imported,
            held_detail,
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

    /// v2's wrong-product verdict: the album's files all named different
    /// content. The first verdict stands.
    pub fn record_wrong_product_verdict(
        &self,
        task_id: &str,
        detail: &str,
        now: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE download_tasks SET wrong_product_verdict_at = ?, wrong_product_detail = ? \
             WHERE id = ? AND wrong_product_verdict_at IS NULL",
            params![now, detail, task_id],
        )?;
        Ok(())
    }

    /// The newest decision recorded for a task.
    pub fn latest_import_decision(
        &self,
        task_id: &str,
    ) -> Result<Option<ImportDecisionRow>, StoreError> {
        self.conn
            .query_row(
                "SELECT task_id, attempt_id, outcome, reason_code, detail, release_mbid, \
                 distance, files_total, files_imported, files_held, checks_json, \
                 missing_positions, decided_at, reason_text, reason_action \
                 FROM download_import_decisions WHERE task_id = ? \
                 ORDER BY decided_at DESC, id DESC LIMIT 1",
                params![task_id],
                |row| {
                    Ok(ImportDecisionRow {
                        task_id: row.get(0)?,
                        attempt_id: row.get(1)?,
                        outcome: row.get(2)?,
                        reason_code: row.get(3)?,
                        detail: row.get(4)?,
                        release_mbid: row.get(5)?,
                        distance: row.get(6)?,
                        files_total: row.get(7)?,
                        files_imported: row.get(8)?,
                        files_held: row.get(9)?,
                        checks_json: row.get(10)?,
                        missing_positions: row.get(11)?,
                        decided_at: row.get(12)?,
                        reason_text: row.get(13)?,
                        reason_action: row.get(14)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }
}
