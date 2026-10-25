//! Reads and writes behind the held-import review: the held rows a viewer
//! may see, resolving them, and settling a task once a person imported
//! what it held.

use rusqlite::{OptionalExtension as _, Row, params};

use super::queue_rows::Viewer;
use super::sql;
use super::state::TaskStatus;
use super::store::{DownloadStore, StoreError};
use crate::reason::Reason;

/// One held file as the review shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldRow {
    pub id: i64,
    pub user_id: String,
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub track_title: Option<String>,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub year: Option<i64>,
    pub held_path: String,
    pub original_filename: Option<String>,
    pub file_format: Option<String>,
    pub duration_seconds: Option<f64>,
    pub expected_duration_seconds: Option<f64>,
    pub reason: Reason,
    pub reason_detail: Option<String>,
    pub evidence_title: Option<String>,
    pub evidence_artist: Option<String>,
    pub evidence_score: Option<f64>,
    pub source: String,
    pub source_task_id: Option<String>,
    pub origin: String,
    pub management_retry_count: i64,
    pub management_next_retry_at: Option<f64>,
    pub created_at: f64,
}

impl HeldRow {
    /// Decode one `held_imports` row as the review reads it.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            release_group_mbid: row.get("release_group_mbid")?,
            release_mbid: row.get("release_mbid")?,
            release_track_mbid: row.get("release_track_mbid")?,
            recording_mbid: row.get("recording_mbid")?,
            track_number: row.get("track_number")?,
            disc_number: row.get("disc_number")?,
            track_title: row.get("track_title")?,
            artist_name: row.get("artist_name")?,
            album_title: row.get("album_title")?,
            year: row.get("year")?,
            held_path: row.get("held_path")?,
            original_filename: row.get("original_filename")?,
            file_format: row.get("file_format")?,
            duration_seconds: row.get("duration_seconds")?,
            expected_duration_seconds: row.get("expected_duration_seconds")?,
            reason: Reason::from_stored(&row.get::<_, String>("reason")?),
            reason_detail: row.get("reason_detail")?,
            evidence_title: row.get("evidence_title")?,
            evidence_artist: row.get("evidence_artist")?,
            evidence_score: row.get("evidence_score")?,
            source: row.get("source")?,
            source_task_id: row.get("source_task_id")?,
            origin: row.get("origin")?,
            management_retry_count: row.get("management_retry_count")?,
            management_next_retry_at: row.get("management_next_retry_at")?,
            created_at: row.get("created_at")?,
        })
    }
}

impl DownloadStore<'_> {
    /// Held files the viewer may see (admins see everyone's), newest first,
    /// optionally for one album or one download.
    pub fn list_held(
        &self,
        viewer: &Viewer,
        release_group_mbid: Option<&str>,
        source_task_id: Option<&str>,
    ) -> Result<Vec<HeldRow>, StoreError> {
        let mut stmt = self.conn.prepare_cached(sql::HELD_LIST.text())?;
        let rows = stmt.query_map(
            params![viewer.owner_filter(), release_group_mbid, source_task_id],
            HeldRow::from_row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Files still held for `reason` by the owner's other downloads of
    /// one recording than `task_id`.
    pub fn held_for_recording(
        &self,
        reason: Reason,
        user_id: &str,
        recording_mbid: &str,
        task_id: &str,
    ) -> Result<Vec<HeldRow>, StoreError> {
        let mut stmt = self.conn.prepare_cached(sql::HELD_FOR_RECORDING.text())?;
        let rows = stmt.query_map(
            params![reason.code(), user_id, recording_mbid, task_id],
            HeldRow::from_row,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One held file still waiting for a decision.
    pub fn get_held(&self, id: i64) -> Result<Option<HeldRow>, StoreError> {
        Ok(self
            .conn
            .query_row(sql::HELD_BY_ID.text(), params![id], HeldRow::from_row)
            .optional()?)
    }
}

impl DownloadStore<'_> {
    /// Mark held files imported or discarded. Only rows still held move,
    /// so two clicks never resolve one file twice. Answers how many moved.
    pub fn resolve_held(&self, ids: &[i64], status: &str, now: f64) -> Result<usize, StoreError> {
        let mut moved = 0;
        for id in ids {
            moved += self.conn.execute(
                "UPDATE held_imports SET status = ?1, resolved_at = ?2 \
                 WHERE id = ?3 AND status = 'held'",
                params![status, now, id],
            )?;
        }
        Ok(moved)
    }

    /// Clear a task's wrong-product verdict once its held files are gone.
    pub fn clear_wrong_product_verdict(&self, task_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE download_tasks SET wrong_product_verdict_at = NULL, \
             wrong_product_detail = NULL WHERE id = ?",
            params![task_id],
        )?;
        Ok(())
    }

    /// A person imported held files and the album is now whole: a failed
    /// or partial task becomes completed, so it stops waiting on a retry
    /// it no longer needs (v2 `settle_after_manual_import`). Answers
    /// whether the task moved.
    pub fn complete_after_manual_import(
        &self,
        task_id: &str,
        now: f64,
    ) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "UPDATE download_tasks SET status = 'completed', error_message = NULL, \
                 outcome_code = NULL, completed_at = ?1, updated_at = ?1 \
             WHERE id = ?2 AND status IN ('failed', 'partial')",
            params![now, task_id],
        )?;
        if changed > 0 {
            self.settle_requests(task_id, TaskStatus::Completed, now)?;
        }
        Ok(changed > 0)
    }
}
