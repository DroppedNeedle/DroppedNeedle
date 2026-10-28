//! Drop-import jobs and items in SQLite (`drop_import_jobs`,
//! `drop_import_items`). An item's files waiting for a person live in its
//! entry of the held list.
//!
//! Reads go through the reader pool, writes through the writer lane. Item
//! transitions name the status they expect, so the background run and a
//! person's match or discard never both act on one item.

use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};

use droppedneedle_kernel::db::DbError;

use super::sql;
use crate::db::{AcquireDb, ms_from_secs_f64 as ms, secs_f64_from_ms as secs};
use crate::held::store::{EntryOrigin, GroupKey, HeldRows, NewEntry, NewFile};
use crate::reason::Reason;

/// Job lifecycle.
pub mod job_status {
    pub const PROCESSING: &str = "processing";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
}

/// Item lifecycle.
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
    /// Why the item stopped short, when it did.
    pub reason_code: Option<Reason>,
    /// The item's staged files while it is processed; empty once settled
    /// (files still waiting are in the held list).
    pub staging_paths: Vec<String>,
}

/// Store failure: the database could not be read or written. The text
/// goes to the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The database was busy during `operation`; the call can run again.
    Busy { operation: String },
    /// Any other failure.
    Failed(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy { operation } => write!(f, "database busy during {operation}"),
            Self::Failed(text) => f.write_str(text),
        }
    }
}

fn store_error(what: &str, error: DbError) -> StoreError {
    match error {
        DbError::Busy { operation } => StoreError::Busy { operation },
        other => StoreError::Failed(format!("drop import {what}: {other}")),
    }
}

fn read_error(error: DbError) -> StoreError {
    store_error("read", error)
}

fn write_error(error: DbError) -> StoreError {
    store_error("write", error)
}

impl JobRecord {
    /// One row of [`sql::JOB_ROW`], without its items.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            user_id: row.get(1)?,
            user_name: row.get(2)?,
            status: row.get(3)?,
            created_at: secs(row.get(4)?),
            upload_name: row.get(5)?,
            staging_dir: row.get(6)?,
            error: row.get(7)?,
            items: Vec::new(),
        })
    }
}

impl ItemRecord {
    /// One row of [`sql::ITEM_ROW`].
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let paths: String = row.get(12)?;
        Ok(Self {
            id: row.get(0)?,
            job_id: row.get(1)?,
            folder_name: row.get(2)?,
            status: row.get(3)?,
            updated_at: secs(row.get(4)?),
            release_group_mbid: row.get(5)?,
            album_title: row.get(6)?,
            artist_name: row.get(7)?,
            files_total: row.get(8)?,
            files_imported: row.get(9)?,
            detail: row.get(10)?,
            reason_code: row
                .get::<_, Option<String>>(11)?
                .map(|code| Reason::from_stored(&code)),
            staging_paths: serde_json::from_str(&paths).unwrap_or_default(),
        })
    }
}

/// `job` with its items, read on the same connection.
fn with_items(conn: &rusqlite::Connection, mut job: JobRecord) -> rusqlite::Result<JobRecord> {
    job.items = conn
        .prepare_cached(sql::ITEMS_OF_JOB.text())?
        .query_map(params![job.id], ItemRecord::from_row)?
        .collect::<Result<_, _>>()?;
    Ok(job)
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
                    sql::INSERT_JOB.text(),
                    params![id, user_id, user_name, ms(at), upload_name, staging_dir],
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
                tx.execute(sql::SET_JOB_STATUS.text(), params![id, status, error])?;
                Ok(())
            })
            .await
            .map_err(write_error)
    }

    /// Add a job's processing items, one per `(folder name, paths)`, in one
    /// transaction, and answer their ids in order. A job either has every
    /// item or none, so a cut-off job is grouped again whole.
    pub async fn add_items(
        &self,
        job_id: &str,
        units: Vec<(String, Vec<String>)>,
    ) -> Result<Vec<i64>, StoreError> {
        let job_id = job_id.to_owned();
        let at = ms(self.db.now_unix_f64());
        self.db
            .write_background("drop_import.add_items", move |tx| {
                let mut ids = Vec::with_capacity(units.len());
                for (folder_name, paths) in units {
                    let total = i64::try_from(paths.len()).unwrap_or(i64::MAX);
                    let paths = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_owned());
                    tx.execute(
                        sql::INSERT_ITEM.text(),
                        params![job_id, folder_name, total, paths, at],
                    )?;
                    ids.push(tx.last_insert_rowid());
                }
                Ok(ids)
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
                Ok(write_item(
                    tx,
                    &item,
                    expected,
                    crate::db::unix_secs_f64(clock.as_ref()),
                )?)
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
                let changed =
                    tx.execute(sql::CLAIM_REVIEW.text(), params![item_id, clock.now_ms()])?;
                Ok(changed == 1)
            })
            .await
            .map_err(write_error)
    }

    /// One job with its items.
    pub async fn get_job(&self, id: &str) -> Result<Option<JobRecord>, StoreError> {
        let id = id.to_owned();
        self.read("drop_import.get_job", move |conn| {
            conn.query_row(sql::JOB_BY_ID.text(), params![id], JobRecord::from_row)
                .optional()?
                .map(|job| with_items(conn, job))
                .transpose()
        })
        .await
    }

    /// Newest jobs first, one user's or everyone's.
    pub async fn list_jobs(
        &self,
        user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<JobRecord>, StoreError> {
        let user_id = user_id.map(str::to_owned);
        self.read("drop_import.list_jobs", move |conn| {
            let jobs: Vec<JobRecord> = conn
                .prepare(sql::LIST_JOBS.text())?
                .query_map(params![user_id, limit], JobRecord::from_row)?
                .collect::<Result<_, _>>()?;
            jobs.into_iter().map(|job| with_items(conn, job)).collect()
        })
        .await
    }

    /// One item.
    pub async fn get_item(&self, id: i64) -> Result<Option<ItemRecord>, StoreError> {
        self.read("drop_import.get_item", move |conn| {
            conn.query_row(sql::ITEM_BY_ID.text(), params![id], ItemRecord::from_row)
                .optional()
        })
        .await
    }

    /// One read on the read pool.
    async fn read<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.db
            .db()
            .read(name, move |conn| op(conn).map_err(DbError::from))
            .await
            .map_err(read_error)
    }

    /// Startup: undo what a restart cut off. A hand match that was
    /// running (an item with a reason, or of a finished job) goes back to
    /// review; items of a job that was still being worked on wait with
    /// `reason`, and the job fails. An item back in review with no open
    /// entry gets one for its staged files below `root`, so its upload
    /// waits in the held list and is never lost to a restart. Answers how
    /// many jobs failed.
    pub async fn fail_stale_processing(
        &self,
        reason: Reason,
        root: &Path,
    ) -> Result<usize, StoreError> {
        let root = root.to_path_buf();
        let clock = self.db.clock().clone();
        self.db
            .write_background("drop_import.fail_stale", move |tx| {
                let now = clock.now_ms();
                let mut select = tx.prepare(sql::CUT_OFF_ITEMS.text())?;
                type Cut = (
                    i64,
                    String,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    String,
                    bool,
                    bool,
                );
                let cut: Vec<Cut> = select
                    .query_map([], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                        ))
                    })?
                    .collect::<Result<_, _>>()?;
                drop(select);
                let rows = HeldRows::new(tx);
                for (id, staged, code, group, artist, album, user_id, job_live, user_known) in cut {
                    // A hand match keeps its own reason; an item of a job
                    // still being worked on was cut off part way.
                    let kept = code
                        .filter(|_| !job_live)
                        .map(|code| Reason::from_stored(&code));
                    let waits = kept.unwrap_or(reason);
                    tx.execute(
                        sql::RETURN_TO_REVIEW.text(),
                        params![waits.message(), waits.code(), now, kept.is_some(), id],
                    )?;
                    if !user_known || rows.entry_of_item(id)?.is_some() {
                        continue;
                    }
                    let files: Vec<NewFile> = serde_json::from_str::<Vec<String>>(&staged)
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|path| {
                            let path = PathBuf::from(path);
                            let rel = path
                                .strip_prefix(&root)
                                .ok()?
                                .to_string_lossy()
                                .into_owned();
                            Some(NewFile::upload(&rel, waits))
                        })
                        .collect();
                    if files.is_empty() {
                        continue;
                    }
                    rows.hold(
                        &NewEntry {
                            user_id,
                            origin: EntryOrigin::Upload,
                            key: GroupKey::Upload(id),
                            release_group_mbid: group,
                            recording_mbid: None,
                            drop_item_id: Some(id),
                            artist_name: artist,
                            album_title: album,
                        },
                        &files,
                        now,
                    )?;
                }
                let jobs =
                    tx.execute(sql::FAIL_PROCESSING_JOBS.text(), params![reason.message()])?;
                Ok(jobs)
            })
            .await
            .map_err(write_error)
    }

    /// Jobs still processing that have no items yet (cut off while their
    /// upload was being unpacked), with their staging folders.
    pub async fn processing_jobs_without_items(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.read("drop_import.jobs_without_items", |conn| {
            conn.prepare(sql::JOBS_WITHOUT_ITEMS.text())?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .await
    }

    /// Every job's staging folder, and whether it is kept: the job is
    /// still being worked on, or one of its items is, or waits in an open
    /// entry of the held list.
    pub async fn staging_dirs(&self) -> Result<Vec<(String, bool)>, StoreError> {
        self.read("drop_import.staging_dirs", |conn| {
            conn.prepare(sql::STAGING_DIRS.text())?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .await
    }
}

/// Write an item's settled state on `conn`, but only while it is still in
/// `expected` status. Answers whether the write landed.
pub(crate) fn write_item(
    conn: &rusqlite::Connection,
    item: &ItemRecord,
    expected: &str,
    at: f64,
) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        sql::SAVE_ITEM.text(),
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
            item.reason_code.map(Reason::code),
            serde_json::to_string(&item.staging_paths).unwrap_or_else(|_| "[]".to_owned()),
            ms(at),
        ],
    )?;
    Ok(changed == 1)
}
