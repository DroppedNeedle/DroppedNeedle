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
    pub detail: Option<String>,
    pub release_mbid: Option<String>,
    pub distance: Option<f64>,
    pub files_total: i64,
    pub files_imported: i64,
    pub files_held: i64,
    /// Every check's verdict, as JSON.
    pub checks_json: String,
    pub decided_at: f64,
}

impl DownloadStore<'_> {
    /// Record one held file. `None` when the same file of the same task is
    /// already held (a resumed landing), so the caller drops its copy.
    pub fn record_held_file(&self, held: &HeldFile, now: f64) -> Result<Option<i64>, StoreError> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM held_imports WHERE source_task_id = ? \
                 AND original_filename = ? AND status = 'held'",
                params![held.source_task_id, held.original_filename],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok(None);
        }
        self.conn.execute(
            "INSERT INTO held_imports (user_id, release_group_mbid, release_mbid, \
             release_track_mbid, recording_mbid, track_number, disc_number, track_title, \
             artist_name, artist_mbid, album_title, year, held_path, original_filename, \
             file_format, duration_seconds, expected_duration_seconds, reason, reason_detail, \
             evidence_title, evidence_artist, evidence_score, source, source_task_id, origin, \
             status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
             'held', ?)",
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
                now,
            ],
        )?;
        Ok(Some(self.conn.last_insert_rowid()))
    }

    /// Record one landing's decision.
    pub fn record_import_decision(&self, row: &ImportDecisionRow) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO download_import_decisions (task_id, attempt_id, outcome, reason_code, \
             detail, release_mbid, distance, files_total, files_imported, files_held, \
             checks_json, decided_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                row.task_id,
                row.attempt_id,
                row.outcome,
                row.reason_code,
                row.detail,
                row.release_mbid,
                row.distance,
                row.files_total,
                row.files_imported,
                row.files_held,
                row.checks_json,
                row.decided_at,
            ],
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
                 distance, files_total, files_imported, files_held, checks_json, decided_at \
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
                        decided_at: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }
}
