//! Drop-import jobs and items in SQLite (`drop_import_jobs`,
//! `drop_import_items`; reason columns from 0029).
//!
//! Reads go through the reader pool, writes through the writer lane. Item
//! transitions name the status they expect, so the background run and a
//! person's match or discard never both act on one item.

use rusqlite::params;
use sqlx::Row as _;

use crate::db::AcquireDb;

/// Job lifecycle (v2 `JobStatus`).
pub mod job_status {
    pub const PROCESSING: &str = "processing";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
}

/// Item lifecycle (v2 `ItemStatus`).
pub mod item_status {
    pub const PROCESSING: &str = "processing";
    pub const IMPORTED: &str = "imported";
    pub const SKIPPED: &str = "skipped";
    pub const NEEDS_REVIEW: &str = "needs_review";
    pub const FAILED: &str = "failed";
    pub const DISCARDED: &str = "discarded";
}

/// One upload gesture.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub status: String,
    pub created_at: f64,
    pub upload_name: String,
    pub staging_dir: String,
    pub error: Option<String>,
    pub items: Vec<ItemRecord>,
}

/// One album-shaped folder of a job.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRecord {
    pub id: i64,
    pub job_id: String,
    pub folder_name: String,
    pub status: String,
    pub updated_at: f64,
    pub release_group_mbid: Option<String>,
    pub album_title: Option<String>,
    pub artist_name: Option<String>,
    pub files_total: i64,
    pub files_imported: i64,
    /// The reason or summary, as one plain sentence.
    pub detail: Option<String>,
    /// Stable reason code, when the item stopped short.
    pub reason_code: Option<String>,
    /// What to do about it.
    pub reason_action: Option<String>,
    /// Staged files still waiting (review items); empty once settled.
    pub staging_paths: Vec<String>,
}

/// Store failure: the database could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn read_error(error: sqlx::Error) -> StoreError {
    StoreError(format!("drop import read: {error}"))
}

fn write_error(error: droppedneedle_kernel::db::DbError) -> StoreError {
    StoreError(format!("drop import write: {error}"))
}

const JOB_SELECT: &str = "SELECT id, user_id, user_name, status, created_at, upload_name, \
     staging_dir, error FROM drop_import_jobs";
const ITEM_SELECT: &str = "SELECT id, job_id, folder_name, status, updated_at, \
     release_group_mbid, album_title, artist_name, files_total, files_imported, detail, \
     reason_code, reason_action, staging_paths FROM drop_import_items";

fn job_from(row: &sqlx::sqlite::SqliteRow) -> Result<JobRecord, sqlx::Error> {
    Ok(JobRecord {
        id: row.try_get(0)?,
        user_id: row.try_get(1)?,
        user_name: row.try_get(2)?,
        status: row.try_get(3)?,
        created_at: row.try_get(4)?,
        upload_name: row.try_get(5)?,
        staging_dir: row.try_get(6)?,
        error: row.try_get(7)?,
        items: Vec::new(),
    })
}

fn item_from(row: &sqlx::sqlite::SqliteRow) -> Result<ItemRecord, sqlx::Error> {
    let paths: String = row.try_get(13)?;
    Ok(ItemRecord {
        id: row.try_get(0)?,
        job_id: row.try_get(1)?,
        folder_name: row.try_get(2)?,
        status: row.try_get(3)?,
        updated_at: row.try_get(4)?,
        release_group_mbid: row.try_get(5)?,
        album_title: row.try_get(6)?,
        artist_name: row.try_get(7)?,
        files_total: row.try_get(8)?,
        files_imported: row.try_get(9)?,
        detail: row.try_get(10)?,
        reason_code: row.try_get(11)?,
        reason_action: row.try_get(12)?,
        staging_paths: serde_json::from_str(&paths).unwrap_or_default(),
    })
}

/// Drop-import rows over the application database.
#[derive(Clone)]
pub struct DropStore {
    db: AcquireDb,
}

impl DropStore {
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// The database the rows live in, and its clock.
    pub fn db(&self) -> &AcquireDb {
        &self.db
    }

    /// Register a job as processing.
    pub async fn create_job(
        &self,
        id: &str,
        user_id: &str,
        user_name: &str,
        upload_name: &str,
        staging_dir: &str,
    ) -> Result<(), StoreError> {
        let values = (
            id.to_owned(),
            user_id.to_owned(),
            user_name.to_owned(),
            upload_name.to_owned(),
            staging_dir.to_owned(),
            self.db.now_unix_f64(),
        );
        self.db
            .write("drop_import.create_job", move |tx| {
                let (id, user_id, user_name, upload_name, staging_dir, at) = values;
                tx.execute(
                    "INSERT INTO drop_import_jobs (id, user_id, user_name, status, created_at, \
                     upload_name, staging_dir) VALUES (?1, ?2, ?3, 'processing', ?4, ?5, ?6)",
                    params![id, user_id, user_name, at, upload_name, staging_dir],
                )?;
                Ok(())
            })
            .await
            .map_err(write_error)
    }

    /// Settle a job: completed or failed, with an optional note.
    pub async fn set_job_status(
        &self,
        id: &str,
        status: &'static str,
        error: Option<String>,
    ) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.db
            .write_background("drop_import.job_status", move |tx| {
                tx.execute(
                    "UPDATE drop_import_jobs SET status = ?2, error = ?3 WHERE id = ?1",
                    params![id, status, error],
                )?;
                Ok(())
            })
            .await
            .map_err(write_error)
    }

    /// Add one processing item and answer its id.
    pub async fn add_item(
        &self,
        job_id: &str,
        folder_name: &str,
        paths: &[String],
    ) -> Result<i64, StoreError> {
        let values = (
            job_id.to_owned(),
            folder_name.to_owned(),
            serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_owned()),
            i64::try_from(paths.len()).unwrap_or(i64::MAX),
            self.db.now_unix_f64(),
        );
        self.db
            .write_background("drop_import.add_item", move |tx| {
                let (job_id, folder_name, paths, total, at) = values;
                tx.execute(
                    "INSERT INTO drop_import_items (job_id, folder_name, status, files_total, \
                     staging_paths, updated_at) VALUES (?1, ?2, 'processing', ?3, ?4, ?5)",
                    params![job_id, folder_name, total, paths, at],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .await
            .map_err(write_error)
    }

    /// Write an item's settled state, but only while it is still in
    /// `expected` status. Answers whether the write landed.
    pub async fn save_item(
        &self,
        item: &ItemRecord,
        expected: &'static str,
    ) -> Result<bool, StoreError> {
        let item = item.clone();
        let clock = self.db.clock().clone();
        self.db
            .write_background("drop_import.save_item", move |tx| {
                let changed = tx.execute(
                    "UPDATE drop_import_items SET status = ?3, release_group_mbid = ?4, \
                     album_title = ?5, artist_name = ?6, files_total = ?7, files_imported = ?8, \
                     detail = ?9, reason_code = ?10, reason_action = ?11, staging_paths = ?12, \
                     updated_at = ?13 WHERE id = ?1 AND status = ?2",
                    params![
                        item.id,
                        expected,
                        item.status,
                        item.release_group_mbid,
                        item.album_title,
                        item.artist_name,
                        item.files_total,
                        item.files_imported,
                        item.detail,
                        item.reason_code,
                        item.reason_action,
                        serde_json::to_string(&item.staging_paths)
                            .unwrap_or_else(|_| "[]".to_owned()),
                        crate::db::unix_secs_f64(clock.as_ref()),
                    ],
                )?;
                Ok(changed == 1)
            })
            .await
            .map_err(write_error)
    }

    /// Claim a review item for a person's match: `needs_review` to
    /// `processing`, so a second click or a discard cannot race it.
    pub async fn claim_review(&self, item_id: i64) -> Result<bool, StoreError> {
        let clock = self.db.clock().clone();
        self.db
            .write("drop_import.claim_review", move |tx| {
                let changed = tx.execute(
                    "UPDATE drop_import_items SET status = 'processing', updated_at = ?2 \
                     WHERE id = ?1 AND status = 'needs_review'",
                    params![item_id, crate::db::unix_secs_f64(clock.as_ref())],
                )?;
                Ok(changed == 1)
            })
            .await
            .map_err(write_error)
    }

    /// One job with its items.
    pub async fn get_job(&self, id: &str) -> Result<Option<JobRecord>, StoreError> {
        let row = sqlx::query(&format!("{JOB_SELECT} WHERE id = ?1"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(read_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut job = job_from(&row).map_err(read_error)?;
        job.items = self.items_of(&job.id).await?;
        Ok(Some(job))
    }

    /// Newest jobs first, one user's or everyone's.
    pub async fn list_jobs(
        &self,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<JobRecord>, StoreError> {
        let rows = sqlx::query(&format!(
            "{JOB_SELECT} WHERE (?1 IS NULL OR user_id = ?1) \
             ORDER BY created_at DESC, id DESC LIMIT ?2"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(read_error)?;
        let mut jobs = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut job = job_from(row).map_err(read_error)?;
            job.items = self.items_of(&job.id).await?;
            jobs.push(job);
        }
        Ok(jobs)
    }

    async fn items_of(&self, job_id: &str) -> Result<Vec<ItemRecord>, StoreError> {
        let rows = sqlx::query(&format!("{ITEM_SELECT} WHERE job_id = ?1 ORDER BY id"))
            .bind(job_id)
            .fetch_all(self.db.pool())
            .await
            .map_err(read_error)?;
        rows.iter()
            .map(item_from)
            .collect::<Result<_, _>>()
            .map_err(read_error)
    }

    /// One item.
    pub async fn get_item(&self, id: i64) -> Result<Option<ItemRecord>, StoreError> {
        let row = sqlx::query(&format!("{ITEM_SELECT} WHERE id = ?1"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(read_error)?;
        row.as_ref().map(item_from).transpose().map_err(read_error)
    }

    /// Startup: undo what a restart cut off. A hand match that was
    /// running (an item with a reason, or of a finished job) goes back to
    /// review with its files; items of a job that was still being
    /// worked on fail with `detail`, and so does the job. Answers how many
    /// jobs failed.
    pub async fn fail_stale_processing(
        &self,
        detail: &'static str,
        code: &'static str,
        action: &'static str,
    ) -> Result<usize, StoreError> {
        let clock = self.db.clock().clone();
        self.db
            .write_background("drop_import.fail_stale", move |tx| {
                let at = crate::db::unix_secs_f64(clock.as_ref());
                tx.execute(
                    "UPDATE drop_import_items SET status = 'needs_review', \
                     detail = COALESCE(detail, ?1), reason_code = COALESCE(reason_code, ?2), \
                     reason_action = COALESCE(reason_action, ?3), updated_at = ?4 \
                     WHERE status = 'processing' AND (reason_code IS NOT NULL \
                       OR job_id NOT IN (SELECT id FROM drop_import_jobs \
                                         WHERE status = 'processing'))",
                    params![detail, code, action, at],
                )?;
                // Items of a job still being worked on keep their files
                // for review too: an upload is never lost to a restart.
                tx.execute(
                    "UPDATE drop_import_items SET status = 'needs_review', detail = ?1, \
                     reason_code = ?2, reason_action = ?3, updated_at = ?4 \
                     WHERE status = 'processing'",
                    params![detail, code, action, at],
                )?;
                let jobs = tx.execute(
                    "UPDATE drop_import_jobs SET status = 'failed', error = ?1 \
                     WHERE status = 'processing'",
                    params![detail],
                )?;
                Ok(jobs)
            })
            .await
            .map_err(write_error)
    }

    /// Jobs still processing that have no items yet (cut off while their
    /// upload was being unpacked), with their staging folders.
    pub async fn processing_jobs_without_items(&self) -> Result<Vec<(String, String)>, StoreError> {
        sqlx::query_as(
            "SELECT j.id, j.staging_dir FROM drop_import_jobs j \
             WHERE j.status = 'processing' \
               AND NOT EXISTS (SELECT 1 FROM drop_import_items i WHERE i.job_id = j.id)",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(read_error)
    }

    /// Every job's staging folder, and whether it still holds files a
    /// person has to review (or a job still being worked on).
    pub async fn staging_dirs(&self) -> Result<Vec<(String, bool)>, StoreError> {
        sqlx::query_as(
            "SELECT j.staging_dir, (j.status = 'processing' OR EXISTS (SELECT 1 \
               FROM drop_import_items i WHERE i.job_id = j.id \
               AND i.status IN ('needs_review', 'processing'))) FROM drop_import_jobs j",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(read_error)
    }

    /// Startup retention: delete review items whose job is older than
    /// `cutoff` and answer their staged paths so the files go too.
    pub async fn prune_stale_review_items(&self, cutoff: f64) -> Result<Vec<String>, StoreError> {
        self.db
            .write_background("drop_import.prune_review", move |tx| {
                let mut select = tx.prepare(
                    "SELECT i.id, i.staging_paths FROM drop_import_items i \
                     JOIN drop_import_jobs j ON j.id = i.job_id \
                     WHERE i.status = 'needs_review' AND j.created_at < ?1",
                )?;
                let rows: Vec<(i64, String)> = select
                    .query_map(params![cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<_, _>>()?;
                drop(select);
                let mut paths = Vec::new();
                for (id, staged) in rows {
                    tx.execute("DELETE FROM drop_import_items WHERE id = ?1", params![id])?;
                    paths.extend(serde_json::from_str::<Vec<String>>(&staged).unwrap_or_default());
                }
                Ok(paths)
            })
            .await
            .map_err(write_error)
    }
}
