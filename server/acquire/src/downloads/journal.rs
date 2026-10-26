//! The download journal's one way in: reads on the read pool, writes on the
//! shared writer lane, both through [`DownloadStore`] and failing with
//! [`StoreError`].

use std::collections::HashSet;
use std::sync::Arc;

use droppedneedle_kernel::db::Lane;

use super::quarantine::{Blocklist, QUARANTINE_TTL_SECONDS};
use super::queue_rows::{ActivitySummary, ListFilter, QueueTask, TaskFacts, Viewer};
use super::source_tag::SourceTag;
use super::sources::SourceHandle;
use super::state::TaskStatus;
use super::store::{AttemptRow, DownloadStore, QuarantineRow, RetryAnchor, StoreError, TaskRow};
use crate::db::AcquireDb;
use crate::flows::seams::TickSink;
use crate::requests::settle::announce_fulfilled;

/// The download journal. Every write is one transaction on the shared
/// writer lane, so multi-statement steps (claim a key and insert its task,
/// settle a task and its attempt and the requests it served) commit
/// together or not at all. Every read runs on the read pool and never
/// waits for a write. Requests a write brought to the library are
/// announced on `ticks` once it commits.
#[derive(Clone)]
pub struct Journal {
    db: AcquireDb,
    ticks: Arc<dyn TickSink>,
}

impl Journal {
    /// Journal over the application database, announcing fulfilled
    /// requests on `ticks`.
    pub fn new(db: AcquireDb, ticks: Arc<dyn TickSink>) -> Self {
        Self { db, ticks }
    }

    /// The database, for the stores beside the journal that share it.
    pub fn db(&self) -> &AcquireDb {
        &self.db
    }

    /// Run one closure against the download store inside one background
    /// write transaction. A store error rolls the whole closure back.
    pub async fn run<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.write(Lane::Background, name, op).await
    }

    /// Same as [`Self::run`] on the request-path lane.
    pub async fn run_foreground<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.write(Lane::Foreground, name, op).await
    }

    async fn write<R, F>(&self, lane: Lane, name: &'static str, op: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        let (result, fulfilled) = self
            .db
            .db()
            .write(lane, name, move |tx| {
                let store = DownloadStore::new(tx);
                let result = op(&store)?;
                Ok::<_, StoreError>((result, store.take_fulfilled()))
            })
            .await?;
        announce_fulfilled(
            self.ticks.as_ref(),
            fulfilled,
            crate::db::to_i64(self.db.now_epoch()),
        );
        Ok(result)
    }

    /// Run one closure against the download store on a read-pool
    /// connection, in one snapshot. Writes fail there: reads only.
    pub async fn read<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: for<'a, 'b> FnOnce(&'a DownloadStore<'b>) -> Result<R, StoreError> + Send + 'static,
    {
        self.db
            .db()
            .read(name, move |conn| op(&DownloadStore::new(conn)))
            .await
    }

    /// One task, if it exists.
    pub async fn read_task(&self, task_id: &str) -> Result<Option<TaskRow>, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.task", move |store| store.get_task(&task_id))
            .await
    }

    /// Every task in one of `statuses`.
    pub async fn read_tasks_in(&self, statuses: &[TaskStatus]) -> Result<Vec<TaskRow>, StoreError> {
        let statuses = statuses.to_vec();
        self.read("downloads.tasks_in", move |store| {
            store.list_active(&statuses)
        })
        .await
    }

    /// The newest live task for one album.
    pub async fn read_active_for_album(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<TaskRow>, StoreError> {
        let key = release_group_mbid.to_owned();
        self.read("downloads.active_for_album", move |store| {
            store.newest_active_for_album(&key)
        })
        .await
    }

    /// Retryable tasks (see [`DownloadStore::list_retryable`]).
    pub async fn read_retryable(&self, max_retry_count: i64) -> Result<Vec<TaskRow>, StoreError> {
        self.read("downloads.retryable", move |store| {
            store.list_retryable(max_retry_count)
        })
        .await
    }

    /// Every attempt for a task, oldest first.
    pub async fn read_attempts(&self, task_id: &str) -> Result<Vec<AttemptRow>, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.attempts", move |store| {
            store.list_attempts(&task_id)
        })
        .await
    }

    /// One attempt's stored client handle, if any.
    pub async fn read_attempt_handle(
        &self,
        attempt_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let attempt_id = attempt_id.to_owned();
        self.read("downloads.attempt_handle", move |store| {
            store.attempt_handle_json(&attempt_id)
        })
        .await
    }

    /// Stored handles of a task's attempts on one source.
    pub async fn read_source_handles(
        &self,
        task_id: &str,
        source: &SourceTag,
    ) -> Result<Vec<String>, StoreError> {
        let (task_id, source) = (task_id.to_owned(), source.to_string());
        self.read("downloads.source_handles", move |store| {
            store.source_handles(&task_id, &source)
        })
        .await
    }

    /// The release identities a task's attempts on one source already
    /// fetched, so its next attempt walks on to an untried release.
    pub async fn read_tried_releases(
        &self,
        task_id: &str,
        source: &SourceTag,
    ) -> Result<HashSet<String>, StoreError> {
        let mut tried = HashSet::new();
        for json in self.read_source_handles(task_id, source).await? {
            match serde_json::from_str::<SourceHandle>(&json) {
                Ok(handle) if !handle.release_identity.is_empty() => {
                    tried.insert(handle.release_identity);
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(task_id, %error, "stored handle does not decode");
                }
            }
        }
        Ok(tried)
    }

    /// One source's live blocklist.
    pub async fn read_blocklist(
        &self,
        source: &SourceTag,
        now: f64,
    ) -> Result<Blocklist, StoreError> {
        let live = self
            .read_quarantine_set(now, QUARANTINE_TTL_SECONDS)
            .await?;
        Ok(Blocklist::for_source(source, live))
    }

    /// The newest client handle a task's attempts recorded.
    pub async fn read_newest_handle(&self, task_id: &str) -> Result<Option<String>, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.newest_handle", move |store| {
            store.newest_handle(&task_id)
        })
        .await
    }

    /// Whether a client job still carries cleanup debt.
    pub async fn read_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> Result<bool, StoreError> {
        let (source, task_id, job_name) =
            (source.to_owned(), task_id.to_owned(), job_name.to_owned());
        self.read("downloads.cleanup_debt", move |store| {
            store.has_cleanup_debt(&source, &task_id, &job_name)
        })
        .await
    }

    /// Whether one task can be reimported.
    pub async fn read_reimportable(&self, task_id: &str) -> Result<bool, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.reimportable", move |store| {
            store.is_reimportable(&task_id)
        })
        .await
    }

    /// The reason the task's newest landing held its files, when it held.
    pub async fn read_hold_code(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::reason::Reason>, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.hold_code", move |store| {
            Ok(store
                .latest_import_decision(&task_id)?
                .filter(|row| row.outcome == "held")
                .and_then(|row| row.reason_code))
        })
        .await
    }

    /// The retry anchor of one task while it is the newest for its target.
    pub async fn read_retry_anchor(
        &self,
        task_id: &str,
    ) -> Result<Option<RetryAnchor>, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.retry_anchor", move |store| {
            store.retry_anchor(&task_id)
        })
        .await
    }

    /// Newest task one owner started for an album or recording since a
    /// time (epoch seconds).
    pub async fn read_task_since(
        &self,
        owner: &str,
        kind: &str,
        key: &str,
        since: f64,
    ) -> Result<Option<String>, StoreError> {
        let (owner, kind, key) = (owner.to_owned(), kind.to_owned(), key.to_owned());
        self.read("downloads.task_since", move |store| {
            store.task_since(&owner, &kind, &key, since)
        })
        .await
    }

    /// Live `(source, identity)` quarantine pairs.
    pub async fn read_quarantine_set(
        &self,
        now: f64,
        ttl_seconds: f64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        self.read("downloads.quarantine_set", move |store| {
            store.load_quarantine_set(now, ttl_seconds)
        })
        .await
    }

    /// One page of quarantine entries, newest first.
    pub async fn read_quarantine_page(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<Vec<QuarantineRow>, StoreError> {
        self.read("downloads.quarantine_page", move |store| {
            store.quarantine_page(page, page_size)
        })
        .await
    }

    /// The viewer's tasks, newest first.
    pub async fn read_queue(
        &self,
        viewer: &Viewer,
        filter: &ListFilter,
    ) -> Result<Vec<QueueTask>, StoreError> {
        let (viewer, filter) = (viewer.clone(), filter.clone());
        self.read("downloads.queue", move |store| {
            store.queue_page(&viewer, &filter)
        })
        .await
    }

    /// One task as the queue view reads it, unscoped.
    pub async fn read_queue_task(&self, task_id: &str) -> Result<Option<QueueTask>, StoreError> {
        let task_id = task_id.to_owned();
        self.read("downloads.queue_task", move |store| {
            store.queue_task(&task_id)
        })
        .await
    }

    /// Facts for every task id given, keyed by task id.
    pub async fn read_task_facts(
        &self,
        task_ids: &[String],
    ) -> Result<std::collections::HashMap<String, TaskFacts>, StoreError> {
        let task_ids = task_ids.to_vec();
        self.read("downloads.task_facts", move |store| {
            store.task_facts(&task_ids)
        })
        .await
    }

    /// The viewer's activity summary.
    pub async fn read_activity_summary(
        &self,
        viewer: &Viewer,
    ) -> Result<ActivitySummary, StoreError> {
        let viewer = viewer.clone();
        self.read("downloads.activity_summary", move |store| {
            store.activity_summary(&viewer)
        })
        .await
    }

    /// The viewer's failed and short tasks still newest for their target.
    pub async fn read_newest_unsettled(&self, viewer: &Viewer) -> Result<Vec<TaskRow>, StoreError> {
        let viewer = viewer.clone();
        self.read("downloads.newest_unsettled", move |store| {
            store.newest_unsettled(&viewer)
        })
        .await
    }
}
