//! Free Music tasks in SQLite (`free_music_tasks`; error code and action
//! from 0029).
//!
//! Every transition names the statuses it may leave, so a cancel and the
//! running download never both win: whichever write lands first decides,
//! and the other sees `false` and stops.

use rusqlite::{OptionalExtension as _, params};
use sqlx::Row as _;

use crate::acquire::db::AcquireDb;

/// Task lifecycle (v2 `FreeMusicStatus`).
pub mod status {
    pub const SEARCHING: &str = "searching";
    pub const DOWNLOADING: &str = "downloading";
    pub const IMPORTING: &str = "importing";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";

    /// Finished one way or another.
    pub fn is_terminal(status: &str) -> bool {
        matches!(status, COMPLETED | FAILED | CANCELLED)
    }
}

/// One Free Music download.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRecord {
    pub id: String,
    pub user_id: String,
    /// `album` or `track`.
    pub kind: String,
    /// Release group (album) or recording (track).
    pub mbid: String,
    pub artist: String,
    pub title: String,
    pub status: String,
    pub created_at: f64,
    pub updated_at: f64,
    /// Tracks the album has, when known (ranks sources).
    pub track_count: i64,
    /// The Internet Archive item chosen.
    pub identifier: String,
    pub licence_url: String,
    pub format: String,
    pub files_total: i64,
    pub files_completed: i64,
    pub bytes_total: i64,
    pub bytes_downloaded: i64,
    pub attempts: i64,
    /// Why it failed, as one plain sentence.
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub error_action: Option<String>,
    pub origin: String,
    /// Sources in the order they are tried, with what became of each.
    pub tried_candidates_json: String,
    /// The release group a track belongs to, or the album's own.
    pub release_group_mbid: Option<String>,
    /// The quality policy pinned when the request was made.
    pub quality_snapshot_json: Option<String>,
    pub quality_snapshot_hash: Option<String>,
    pub quality_snapshot_summary: Option<String>,
    /// The edition the request asked for, when it named one.
    pub release_mbid: Option<String>,
}

/// A new task.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub id: String,
    pub user_id: String,
    pub kind: &'static str,
    pub mbid: String,
    pub artist: String,
    pub title: String,
    pub track_count: i64,
    pub origin: String,
    pub release_group_mbid: Option<String>,
    pub quality_snapshot_json: String,
    pub quality_snapshot_hash: String,
    pub quality_snapshot_summary: String,
    /// The edition asked for, when the request named one.
    pub release_mbid: Option<String>,
    /// The caller's key for this ask; a repeat answers the same task.
    pub idempotency_key: Option<String>,
}

/// Store failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn read_error(error: sqlx::Error) -> StoreError {
    StoreError(format!("free music read: {error}"))
}

fn write_error(error: crate::db::DbError) -> StoreError {
    StoreError(format!("free music write: {error}"))
}

const SELECT: &str = "SELECT id, user_id, kind, mbid, artist, title, status, created_at, \
     updated_at, track_count, COALESCE(identifier, ''), COALESCE(licence_url, ''), \
     COALESCE(format, ''), files_total, files_completed, bytes_total, bytes_downloaded, \
     attempts, error, error_code, error_action, origin, tried_candidates_json, \
     release_group_mbid, quality_snapshot_json, quality_snapshot_hash, quality_snapshot_summary, \
     release_mbid FROM free_music_tasks";

fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TaskRecord, sqlx::Error> {
    Ok(TaskRecord {
        id: row.try_get(0)?,
        user_id: row.try_get(1)?,
        kind: row.try_get(2)?,
        mbid: row.try_get(3)?,
        artist: row.try_get(4)?,
        title: row.try_get(5)?,
        status: row.try_get(6)?,
        created_at: row.try_get(7)?,
        updated_at: row.try_get(8)?,
        track_count: row.try_get(9)?,
        identifier: row.try_get(10)?,
        licence_url: row.try_get(11)?,
        format: row.try_get(12)?,
        files_total: row.try_get(13)?,
        files_completed: row.try_get(14)?,
        bytes_total: row.try_get(15)?,
        bytes_downloaded: row.try_get(16)?,
        attempts: row.try_get(17)?,
        error: row.try_get(18)?,
        error_code: row.try_get(19)?,
        error_action: row.try_get(20)?,
        origin: row.try_get(21)?,
        tried_candidates_json: row.try_get(22)?,
        release_group_mbid: row.try_get(23)?,
        quality_snapshot_json: row.try_get(24)?,
        quality_snapshot_hash: row.try_get(25)?,
        quality_snapshot_summary: row.try_get(26)?,
        release_mbid: row.try_get(27)?,
    })
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// `status IN (...)` for a fixed list of statuses.
fn any_of(statuses: &[&str]) -> String {
    let quoted: Vec<String> = statuses
        .iter()
        .map(|status| format!("'{status}'"))
        .collect();
    format!("status IN ({})", quoted.join(", "))
}

const ACTIVE: &[&str] = &[status::SEARCHING, status::DOWNLOADING, status::IMPORTING];
const TERMINAL: &[&str] = &[status::COMPLETED, status::FAILED, status::CANCELLED];

/// Free Music tasks over the application database.
#[derive(Clone)]
pub struct FreeMusicStore {
    db: AcquireDb,
}

impl FreeMusicStore {
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Register a task as searching. A repeat of an idempotency key
    /// answers the task the key already names instead of a twin; the
    /// lookup and the insert share one write.
    pub async fn create(&self, task: NewTask) -> Result<String, StoreError> {
        self.db
            .write("free_music.create", move |tx| {
                if let Some(key) = &task.idempotency_key {
                    let existing: Option<String> = tx
                        .query_row(
                            "SELECT id FROM free_music_tasks WHERE idempotency_key = ?1",
                            params![key],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if let Some(id) = existing {
                        return Ok(id);
                    }
                }
                let at = now();
                tx.execute(
                    "INSERT INTO free_music_tasks (id, user_id, kind, mbid, artist, title, \
                     status, created_at, updated_at, track_count, origin, release_group_mbid, \
                     quality_snapshot_json, quality_snapshot_hash, quality_snapshot_summary, \
                     release_mbid, idempotency_key) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'searching', ?7, ?7, ?8, ?9, ?10, ?11, ?12, \
                     ?13, ?14, ?15)",
                    params![
                        task.id,
                        task.user_id,
                        task.kind,
                        task.mbid,
                        task.artist,
                        task.title,
                        at,
                        task.track_count.max(0),
                        task.origin,
                        task.release_group_mbid,
                        task.quality_snapshot_json,
                        task.quality_snapshot_hash,
                        task.quality_snapshot_summary,
                        task.release_mbid,
                        task.idempotency_key,
                    ],
                )?;
                Ok(task.id)
            })
            .await
            .map_err(write_error)
    }

    /// One task.
    pub async fn get(&self, id: &str) -> Result<Option<TaskRecord>, StoreError> {
        let row = sqlx::query(&format!("{SELECT} WHERE id = ?1"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(read_error)?;
        row.as_ref().map(from_row).transpose().map_err(read_error)
    }

    /// Newest first, one user's or everyone's.
    pub async fn list(
        &self,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<TaskRecord>, StoreError> {
        let rows = sqlx::query(&format!(
            "{SELECT} WHERE (?1 IS NULL OR user_id = ?1) ORDER BY created_at DESC, id DESC LIMIT ?2"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(read_error)?;
        rows.iter()
            .map(from_row)
            .collect::<Result<_, _>>()
            .map_err(read_error)
    }

    /// One guarded update: `assignments` (SQL with `?2..`) applied while
    /// the task is in one of `from`. Answers whether it landed.
    async fn guarded(
        &self,
        name: &'static str,
        id: &str,
        from: &'static [&'static str],
        assignments: &'static str,
        values: Vec<rusqlite::types::Value>,
    ) -> Result<bool, StoreError> {
        let id = id.to_owned();
        self.db
            .write_background(name, move |tx| {
                let sql = format!(
                    "UPDATE free_music_tasks SET {assignments}, updated_at = ?{} \
                     WHERE id = ?1 AND {}",
                    values.len() + 2,
                    any_of(from)
                );
                let mut bound: Vec<rusqlite::types::Value> = Vec::with_capacity(values.len() + 2);
                bound.push(rusqlite::types::Value::Text(id));
                bound.extend(values);
                bound.push(rusqlite::types::Value::Real(now()));
                let changed = tx.execute(&sql, rusqlite::params_from_iter(bound))?;
                Ok(changed == 1)
            })
            .await
            .map_err(write_error)
    }

    /// Start downloading one source.
    pub async fn start_candidate(
        &self,
        id: &str,
        identifier: &str,
        licence_url: &str,
        format: &str,
        files_total: i64,
        bytes_total: i64,
    ) -> Result<bool, StoreError> {
        use rusqlite::types::Value;
        self.guarded(
            "free_music.start_candidate",
            id,
            &[status::SEARCHING, status::DOWNLOADING],
            "status = 'downloading', identifier = ?2, licence_url = ?3, format = ?4, \
             files_total = ?5, bytes_total = ?6, files_completed = 0, bytes_downloaded = 0",
            vec![
                Value::Text(identifier.to_owned()),
                Value::Text(licence_url.to_owned()),
                Value::Text(format.to_owned()),
                Value::Integer(files_total),
                Value::Integer(bytes_total),
            ],
        )
        .await
    }

    /// Record download progress; `false` means the task was cancelled.
    pub async fn progress(&self, id: &str, bytes: i64, files: i64) -> Result<bool, StoreError> {
        use rusqlite::types::Value;
        self.guarded(
            "free_music.progress",
            id,
            &[status::DOWNLOADING],
            "bytes_downloaded = ?2, files_completed = ?3",
            vec![Value::Integer(bytes), Value::Integer(files)],
        )
        .await
    }

    /// Count a download attempt.
    pub async fn attempt(&self, id: &str, attempts: i64) -> Result<bool, StoreError> {
        self.guarded(
            "free_music.attempt",
            id,
            &[status::DOWNLOADING],
            "attempts = ?2",
            vec![rusqlite::types::Value::Integer(attempts)],
        )
        .await
    }

    /// Keep the ranked source ladder and what became of each source.
    pub async fn set_ladder(&self, id: &str, ladder: String) -> Result<bool, StoreError> {
        self.guarded(
            "free_music.ladder",
            id,
            &[status::SEARCHING, status::DOWNLOADING],
            "tried_candidates_json = ?2",
            vec![rusqlite::types::Value::Text(ladder)],
        )
        .await
    }

    /// Move between two working statuses.
    pub async fn advance(
        &self,
        id: &str,
        from: &'static [&'static str],
        to: &'static str,
    ) -> Result<bool, StoreError> {
        self.guarded(
            "free_music.advance",
            id,
            from,
            "status = ?2",
            vec![rusqlite::types::Value::Text(to.to_owned())],
        )
        .await
    }

    /// Fail a working task with one reason.
    pub async fn fail(
        &self,
        id: &str,
        message: &str,
        code: &str,
        action: &str,
    ) -> Result<bool, StoreError> {
        use rusqlite::types::Value;
        self.guarded(
            "free_music.fail",
            id,
            ACTIVE,
            "status = 'failed', error = ?2, error_code = ?3, error_action = ?4",
            vec![
                Value::Text(message.to_owned()),
                Value::Text(code.to_owned()),
                Value::Text(action.to_owned()),
            ],
        )
        .await
    }

    /// Cancel a task that has not started importing.
    pub async fn cancel_active(&self, id: &str) -> Result<bool, StoreError> {
        self.guarded(
            "free_music.cancel",
            id,
            &[status::SEARCHING, status::DOWNLOADING],
            "status = 'cancelled', error = NULL, error_code = NULL, error_action = NULL",
            Vec::new(),
        )
        .await
    }

    /// Start a failed or cancelled task over, keeping its source ladder.
    pub async fn restart_terminal(&self, id: &str) -> Result<bool, StoreError> {
        self.guarded(
            "free_music.restart",
            id,
            &[status::FAILED, status::CANCELLED],
            "status = 'searching', error = NULL, error_code = NULL, error_action = NULL, \
             bytes_downloaded = 0, files_completed = 0",
            Vec::new(),
        )
        .await
    }

    /// Delete one finished task; answers its owner.
    pub async fn delete_terminal(&self, id: &str) -> Result<Option<String>, StoreError> {
        let id = id.to_owned();
        self.db
            .write("free_music.delete", move |tx| {
                let owner: Option<String> = tx
                    .query_row(
                        &format!(
                            "SELECT user_id FROM free_music_tasks WHERE id = ?1 AND {}",
                            any_of(TERMINAL)
                        ),
                        params![id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if owner.is_some() {
                    tx.execute("DELETE FROM free_music_tasks WHERE id = ?1", params![id])?;
                }
                Ok(owner)
            })
            .await
            .map_err(write_error)
    }

    /// Delete every finished task of one user (or everyone's); answers
    /// the owners touched.
    pub async fn delete_terminal_tasks(
        &self,
        user_id: Option<String>,
    ) -> Result<Vec<String>, StoreError> {
        self.db
            .write("free_music.clear", move |tx| {
                let filter = format!("(?1 IS NULL OR user_id = ?1) AND {}", any_of(TERMINAL));
                let mut select = tx.prepare(&format!(
                    "SELECT user_id FROM free_music_tasks WHERE {filter}"
                ))?;
                let owners: Vec<String> = select
                    .query_map(params![user_id], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                drop(select);
                tx.execute(
                    &format!("DELETE FROM free_music_tasks WHERE {filter}"),
                    params![user_id],
                )?;
                Ok(owners)
            })
            .await
            .map_err(write_error)
    }

    /// Startup: tasks a restart cut off fail with one reason.
    pub async fn fail_stale(
        &self,
        message: &'static str,
        code: &'static str,
        action: &'static str,
    ) -> Result<usize, StoreError> {
        self.db
            .write_background("free_music.fail_stale", move |tx| {
                Ok(tx.execute(
                    &format!(
                        "UPDATE free_music_tasks SET status = 'failed', error = ?1, \
                         error_code = ?2, error_action = ?3, updated_at = ?4 WHERE {}",
                        any_of(ACTIVE)
                    ),
                    params![message, code, action, now()],
                )?)
            })
            .await
            .map_err(write_error)
    }
}
