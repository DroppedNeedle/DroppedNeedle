//! Reads and writes behind the held-import review: the held rows a viewer
//! may see, resolving them, and settling a task once a person imported
//! what it held.

use rusqlite::params;
use sqlx::{Row as _, SqlitePool, sqlite::SqliteRow};

use super::queue_rows::Viewer;
use super::store::{DownloadStore, StoreError};

/// One held file as the review shows it.
#[derive(Debug, Clone, Default, PartialEq)]
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
    pub reason: String,
    pub reason_text: Option<String>,
    pub reason_action: Option<String>,
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

const HELD_COLUMNS: &str = "id, user_id, release_group_mbid, release_mbid, release_track_mbid, \
     recording_mbid, track_number, disc_number, track_title, artist_name, album_title, year, \
     held_path, original_filename, file_format, duration_seconds, expected_duration_seconds, \
     reason, reason_text, reason_action, reason_detail, evidence_title, evidence_artist, \
     evidence_score, source, source_task_id, origin, management_retry_count, \
     management_next_retry_at, created_at";

fn held_from_row(row: &SqliteRow) -> Result<HeldRow, sqlx::Error> {
    Ok(HeldRow {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        release_group_mbid: row.try_get("release_group_mbid")?,
        release_mbid: row.try_get("release_mbid")?,
        release_track_mbid: row.try_get("release_track_mbid")?,
        recording_mbid: row.try_get("recording_mbid")?,
        track_number: row.try_get("track_number")?,
        disc_number: row.try_get("disc_number")?,
        track_title: row.try_get("track_title")?,
        artist_name: row.try_get("artist_name")?,
        album_title: row.try_get("album_title")?,
        year: row.try_get("year")?,
        held_path: row.try_get("held_path")?,
        original_filename: row.try_get("original_filename")?,
        file_format: row.try_get("file_format")?,
        duration_seconds: row.try_get("duration_seconds")?,
        expected_duration_seconds: row.try_get("expected_duration_seconds")?,
        reason: row.try_get("reason")?,
        reason_text: row.try_get("reason_text")?,
        reason_action: row.try_get("reason_action")?,
        reason_detail: row.try_get("reason_detail")?,
        evidence_title: row.try_get("evidence_title")?,
        evidence_artist: row.try_get("evidence_artist")?,
        evidence_score: row.try_get("evidence_score")?,
        source: row.try_get("source")?,
        source_task_id: row.try_get("source_task_id")?,
        origin: row.try_get("origin")?,
        management_retry_count: row.try_get("management_retry_count")?,
        management_next_retry_at: row.try_get("management_next_retry_at")?,
        created_at: row.try_get("created_at")?,
    })
}

/// Held files the viewer may see (admins see everyone's), newest first,
/// optionally for one album or one download.
pub async fn list_held(
    pool: &SqlitePool,
    viewer: &Viewer,
    release_group_mbid: Option<&str>,
    source_task_id: Option<&str>,
) -> Result<Vec<HeldRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {HELD_COLUMNS} FROM held_imports WHERE status = 'held' \
           AND (?1 IS NULL OR user_id = ?1) \
           AND (?2 IS NULL OR lower(release_group_mbid) = lower(?2)) \
           AND (?3 IS NULL OR source_task_id = ?3) \
         ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(viewer.owner_filter())
        .bind(release_group_mbid)
        .bind(source_task_id)
        .fetch_all(pool)
        .await?;
    rows.iter().map(held_from_row).collect()
}

/// One held file still waiting for a decision.
pub async fn get_held(pool: &SqlitePool, id: i64) -> Result<Option<HeldRow>, sqlx::Error> {
    let sql = format!("SELECT {HELD_COLUMNS} FROM held_imports WHERE id = ?1 AND status = 'held'");
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    row.as_ref().map(held_from_row).transpose()
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
                 completed_at = ?1, updated_at = ?1 \
             WHERE id = ?2 AND status IN ('failed', 'partial')",
            params![now, task_id],
        )?;
        Ok(changed > 0)
    }
}
