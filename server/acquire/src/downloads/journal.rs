//! The download journal handle: writes on the shared writer lane, reads
//! through the reader pool.

use crate::db::AcquireDb;
use droppedneedle_kernel::db::{DbError, Lane};

use super::store::{
    CLEANUP_DEBT_SQL, DownloadStore, QUARANTINE_SET_SQL, RETRYABLE_SQL, StoreError, TaskRow,
    task_from_sqlx,
};

/// The download journal. Every operation is one transaction on the shared
/// writer lane, so multi-statement steps (claim a key and insert its task,
/// settle a task and its attempt) commit together or not at all.
pub struct Journal {
    db: AcquireDb,
}

impl Journal {
    /// Journal over the application database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Database handle, for the read-only queries that use the pool.
    pub fn db(&self) -> &AcquireDb {
        &self.db
    }

    /// Run one closure against the download store inside one background
    /// write transaction. A store error rolls the whole closure back.
    pub async fn run<R, F>(&self, name: &'static str, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.run_on(Lane::Background, name, op).await
    }

    /// Same as [`Self::run`] on the request-path lane.
    pub async fn run_foreground<R, F>(&self, name: &'static str, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.run_on(Lane::Foreground, name, op).await
    }

    /// Read one task through the reader pool.
    pub async fn read_task(&self, task_id: &str) -> Result<Option<TaskRow>, String> {
        let row = sqlx::query("SELECT * FROM download_tasks WHERE id = ?")
            .bind(task_id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| format!("read download task: {error}"))?;
        row.as_ref()
            .map(task_from_sqlx)
            .transpose()
            .map_err(|error| format!("decode download task: {error}"))
    }

    /// Retryable tasks (see [`DownloadStore::list_retryable`]) through the
    /// reader pool.
    pub async fn read_retryable(&self, max_retry_count: i64) -> Result<Vec<TaskRow>, String> {
        let rows = sqlx::query(RETRYABLE_SQL)
            .bind(max_retry_count)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| format!("read retryable tasks: {error}"))?;
        rows.iter()
            .map(task_from_sqlx)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode retryable task: {error}"))
    }

    /// One attempt's stored client handle, if any, through the reader pool.
    pub async fn read_attempt_handle(&self, attempt_id: &str) -> Result<Option<String>, String> {
        let json: Option<String> =
            sqlx::query_scalar("SELECT handle_json FROM download_attempts WHERE id = ?")
                .bind(attempt_id)
                .fetch_optional(self.db.pool())
                .await
                .map_err(|error| format!("read attempt handle: {error}"))?;
        Ok(json.filter(|text| !text.is_empty()))
    }

    /// Stored handles of a task's attempts on one source.
    pub async fn read_source_handles(
        &self,
        task_id: &str,
        source: &str,
    ) -> Result<Vec<String>, String> {
        sqlx::query_scalar(
            "SELECT handle_json FROM download_attempts \
             WHERE task_id = ? AND source = ? AND handle_json != ''",
        )
        .bind(task_id)
        .bind(source)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| format!("read attempt handles: {error}"))
    }

    /// Whether a client job still carries cleanup debt.
    pub async fn read_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> Result<bool, String> {
        let count: i64 = sqlx::query_scalar(CLEANUP_DEBT_SQL)
            .bind(source)
            .bind(task_id)
            .bind(job_name)
            .fetch_one(self.db.pool())
            .await
            .map_err(|error| format!("read cleanup debt: {error}"))?;
        Ok(count > 0)
    }

    /// Live `(source, identity)` quarantine pairs.
    pub async fn read_quarantine_set(
        &self,
        now: f64,
        ttl_seconds: f64,
    ) -> Result<Vec<(String, String)>, String> {
        sqlx::query_as(QUARANTINE_SET_SQL)
            .bind(now - ttl_seconds)
            .fetch_all(self.db.pool())
            .await
            .map_err(|error| format!("read quarantine: {error}"))
    }

    async fn run_on<R, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.db
            .db()
            .write(lane, name, move |tx| {
                let store = DownloadStore::new(tx);
                op(&store).map_err(|error| match error {
                    StoreError::Sqlite(error) => DbError::from(error),
                    other => DbError::WriteFailed {
                        operation: name.to_owned(),
                        cause: other.to_string(),
                    },
                })
            })
            .await
            .map_err(|error: DbError| error.to_string())
    }
}
