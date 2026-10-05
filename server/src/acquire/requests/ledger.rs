//! In-memory request ledger and approval queues.
//!
//! One row per (kind, MBID); concurrent asks for the same key attach as
//! co-requesters instead of spawning rival rows. Every mutation carries a
//! generation compare-and-swap so a stale actor loses loudly instead of
//! overwriting a newer decision (v2 `expected_generation` quirk). Wiring
//! swaps these stores for SQLite ports without touching service signatures.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::RequestsError;
use super::models::RequestKind;

/// Row is live and owned by exactly one generation.
pub const STATUS_PENDING: &str = "pending";
/// Row is live and fetching.
pub const STATUS_DOWNLOADING: &str = "downloading";
/// Row is live and queued behind other fetches.
pub const STATUS_QUEUED: &str = "queued";
/// Row waits for an admin decision; no task exists yet.
pub const STATUS_AWAITING_APPROVAL: &str = "awaiting_approval";
/// Row is mid-cancel; new asks attach to the verdict, never a new row.
pub const STATUS_CANCELLING: &str = "cancelling";
/// Row was cancelled.
pub const STATUS_CANCELLED: &str = "cancelled";
/// Row failed.
pub const STATUS_FAILED: &str = "failed";
/// Row landed in the library.
pub const STATUS_IMPORTED: &str = "imported";
/// Row landed short.
pub const STATUS_INCOMPLETE: &str = "incomplete";
/// Row was rejected by a reviewer.
pub const STATUS_REJECTED: &str = "rejected";

/// Asks that keep a row alive (v2 `_ACTIVE_REQUEST_STATUSES`).
pub const ACTIVE_STATUSES: [&str; 4] = [
    STATUS_PENDING,
    STATUS_DOWNLOADING,
    STATUS_QUEUED,
    STATUS_AWAITING_APPROVAL,
];

/// Tasks an owner may still stop (v2 `_CANCELLABLE_STATUSES`).
pub const CANCELLABLE_STATUSES: [&str; 3] = [STATUS_PENDING, STATUS_DOWNLOADING, STATUS_QUEUED];

/// Terminal rows a retry may revive (v2 `_RETRYABLE_STATUSES`).
pub const RETRYABLE_STATUSES: [&str; 3] = [STATUS_FAILED, STATUS_CANCELLED, STATUS_INCOMPLETE];

/// Terminal rows history-clearing may drop (v2 `_CLEARABLE_STATUSES`).
pub const CLEARABLE_STATUSES: [&str; 4] = [
    STATUS_IMPORTED,
    STATUS_INCOMPLETE,
    STATUS_FAILED,
    STATUS_CANCELLED,
];

/// Whether a status keeps its row alive.
pub fn is_active(status: &str) -> bool {
    ACTIVE_STATUSES.contains(&status)
}

/// One co-requester attached to a shared row.
#[derive(Debug, Clone)]
pub struct Requester {
    /// Asking user id.
    pub user_id: String,
    /// Display name at ask time.
    pub name: Option<String>,
}

/// One stored ask.
#[derive(Debug, Clone)]
pub struct RequestRecord {
    /// Lowercased album or recording MBID.
    pub key: String,
    /// Album or track.
    pub kind: RequestKind,
    /// Row status.
    pub status: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
    /// Release year.
    pub year: Option<i32>,
    /// Edition release id, when resolved.
    pub release_mbid: Option<String>,
    /// Track title for exact-track rows.
    pub track_title: Option<String>,
    /// Track length for exact-track rows.
    pub duration_seconds: Option<i64>,
    /// Containing release group for exact-track rows.
    pub track_release_group_mbid: Option<String>,
    /// Immutable primary owner; retries keep it (v2 `_dispatch_record`).
    pub user_id: Option<String>,
    /// Owner display name.
    pub requested_by_name: Option<String>,
    /// Co-requesters beyond the owner.
    pub requesters: Vec<Requester>,
    /// Users who dismissed this row from their own history.
    pub dismissed_by: HashSet<String>,
    /// Epoch seconds when asked.
    pub requested_at: u64,
    /// Epoch seconds when terminal.
    pub completed_at: Option<u64>,
    /// Linked download task.
    pub task_id: Option<String>,
    /// Mutation generation; every write bumps it.
    pub generation: u64,
    /// True once a privileged role or an approval authorized dispatch. An
    /// ordinary user's retry may only dispatch generations carrying this
    /// provenance (v2 `claim_retry` quirk).
    pub dispatch_authorized: bool,
    /// Follow the artist for new releases.
    pub monitor_artist: bool,
    /// Auto-download future releases.
    pub auto_download_artist: bool,
    /// Reviewer display name.
    pub reviewed_by_name: Option<String>,
    /// Reviewer epoch seconds.
    pub reviewed_at: Option<u64>,
}

impl RequestRecord {
    /// Whether one user owns or co-requests this row.
    pub fn has_requester(&self, user_id: &str) -> bool {
        self.user_id.as_deref() == Some(user_id)
            || self.requesters.iter().any(|r| r.user_id == user_id)
    }
}

/// Outcome of claiming one request generation.
#[derive(Debug, Clone)]
pub enum BeginOutcome {
    /// This caller won the row.
    Won(RequestRecord),
    /// A live row already owns the key.
    Existing(RequestRecord),
}

/// Outcome of the atomic requester-cancel decision.
#[derive(Debug, Clone)]
pub enum CancelDecision {
    /// Caller may not touch this row.
    Denied {
        /// Status at decision time.
        prior_status: String,
    },
    /// Co-requester detached; the shared row continues.
    Detached,
    /// Waiting row cancelled outright (no task existed).
    CancelledDirect,
    /// Owner must cancel the linked task, then finalize.
    CancelTask {
        /// Status at decision time.
        prior_status: String,
        /// Task to cancel, when linked.
        task_id: Option<String>,
        /// Owner the task runs under.
        task_owner: String,
        /// Generation the finalizer must win.
        generation: u64,
    },
}

/// Shared rows by (kind, lowercased MBID).
type Rows = HashMap<(String, String), RequestRecord>;

/// Read guard over the shared rows.
type RowsReadGuard<'a> = std::sync::RwLockReadGuard<'a, Rows>;

/// Write guard over the shared rows.
type RowsWriteGuard<'a> = std::sync::RwLockWriteGuard<'a, Rows>;

/// Outcome of claiming a retry generation.
#[derive(Debug, Clone)]
pub enum RetryClaim {
    /// Claimed; dispatch when the target allows.
    Claimed {
        /// Claimed generation.
        generation: u64,
        /// Status the claim moved the row to.
        target_status: String,
    },
    /// Lost: row missing, moved on, or foreign.
    Lost,
}

/// The request ledger.
pub struct RequestStore {
    /// Rows by (kind, lowercased MBID).
    rows: RwLock<Rows>,
}

impl RequestStore {
    /// Empty ledger.
    pub fn new() -> Self {
        Self {
            rows: RwLock::new(HashMap::new()),
        }
    }

    /// Read one row.
    pub fn get(
        &self,
        kind: RequestKind,
        key: &str,
    ) -> Result<Option<RequestRecord>, RequestsError> {
        let rows = self.read_rows()?;
        Ok(rows.get(&store_key(kind, key)).cloned())
    }

    /// Lowercased MBIDs with a live row of one kind (v2
    /// `async_get_requested_mbids`, the batch dedupe read).
    pub fn active_mbids(&self, kind: RequestKind) -> Result<HashSet<String>, RequestsError> {
        let rows = self.read_rows()?;
        let mut out = HashSet::new();
        for ((row_kind, key), record) in rows.iter() {
            if row_kind == kind.as_str() && is_active(&record.status) {
                out.insert(key.clone());
            }
        }
        Ok(out)
    }

    /// Claim one row: win a fresh generation, or meet the live row that
    /// already owns the key (v2 `_begin_request`). A `cancelling` row also
    /// wins over the newcomer: the verdict lands on the old row.
    pub fn begin(&self, record: RequestRecord) -> Result<BeginOutcome, RequestsError> {
        let mut rows = self.write_rows()?;
        let key = (record.kind.as_str().to_owned(), record.key.clone());
        if let Some(existing) = rows.get(&key) {
            if is_active(&existing.status) || existing.status == STATUS_CANCELLING {
                return Ok(BeginOutcome::Existing(existing.clone()));
            }
            let generation = existing.generation.saturating_add(1);
            let mut fresh = record;
            fresh.generation = generation;
            rows.insert(key, fresh.clone());
            return Ok(BeginOutcome::Won(fresh));
        }
        let mut fresh = record;
        fresh.generation = 1;
        rows.insert(key, fresh.clone());
        Ok(BeginOutcome::Won(fresh))
    }

    /// Attach a co-requester; the owner row is untouched otherwise.
    pub fn attach_requester(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        name: Option<String>,
    ) -> Result<(), RequestsError> {
        let mut rows = self.write_rows()?;
        if let Some(record) = rows.get_mut(&store_key(kind, key))
            && !record.has_requester(user_id)
        {
            record.requesters.push(Requester {
                user_id: user_id.to_owned(),
                name,
            });
            record.generation = record.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Widen monitoring flags; never narrows them (v2 batch/single quirk).
    pub fn widen_monitoring(
        &self,
        kind: RequestKind,
        key: &str,
        monitor_artist: bool,
        auto_download_artist: bool,
    ) -> Result<(), RequestsError> {
        let mut rows = self.write_rows()?;
        if let Some(record) = rows.get_mut(&store_key(kind, key))
            && monitor_artist
            && !record.monitor_artist
        {
            record.monitor_artist = true;
            record.auto_download_artist = auto_download_artist;
            record.generation = record.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Move one row's status behind the generation CAS.
    pub fn update_status(
        &self,
        kind: RequestKind,
        key: &str,
        status: &str,
        completed_at: Option<u64>,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(false);
        };
        if record.generation != expected_generation {
            return Ok(false);
        }
        record.status = status.to_owned();
        record.completed_at = completed_at;
        record.generation = record.generation.saturating_add(1);
        Ok(true)
    }

    /// Link one row to its download task behind the generation CAS.
    pub fn link_task(
        &self,
        kind: RequestKind,
        key: &str,
        task_id: &str,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(false);
        };
        if record.generation != expected_generation {
            return Ok(false);
        }
        record.task_id = Some(task_id.to_owned());
        record.generation = record.generation.saturating_add(1);
        Ok(true)
    }

    /// Claim an approval: `awaiting_approval` becomes `pending` with the
    /// reviewer stamped and dispatch authorized (v2 `async_claim_approval`).
    /// Anything else loses the claim.
    pub fn claim_approval(
        &self,
        kind: RequestKind,
        key: &str,
        reviewer_name: Option<String>,
        reviewed_at: u64,
        expected_generation: u64,
    ) -> Result<Option<RequestRecord>, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(None);
        };
        if record.status != STATUS_AWAITING_APPROVAL || record.generation != expected_generation {
            return Ok(None);
        }
        record.status = STATUS_PENDING.to_owned();
        record.dispatch_authorized = true;
        record.reviewed_by_name = reviewer_name;
        record.reviewed_at = Some(reviewed_at);
        record.generation = record.generation.saturating_add(1);
        Ok(Some(record.clone()))
    }

    /// Claim a rejection: `awaiting_approval` becomes `rejected`.
    pub fn claim_rejection(
        &self,
        kind: RequestKind,
        key: &str,
        reviewer_name: Option<String>,
        reviewed_at: u64,
        completed_at: u64,
        expected_generation: u64,
    ) -> Result<Option<RequestRecord>, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(None);
        };
        if record.status != STATUS_AWAITING_APPROVAL || record.generation != expected_generation {
            return Ok(None);
        }
        record.status = STATUS_REJECTED.to_owned();
        record.dispatch_authorized = false;
        record.reviewed_by_name = reviewer_name;
        record.reviewed_at = Some(reviewed_at);
        record.completed_at = Some(completed_at);
        record.generation = record.generation.saturating_add(1);
        Ok(Some(record.clone()))
    }

    /// Claim a retry generation: only retryable rows move, and a non-admin
    /// must already be a requester (v2 `async_claim_retry`).
    #[allow(clippy::too_many_arguments)]
    pub fn claim_retry(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        target_status: &str,
        dispatch_authorized: bool,
        require_membership: bool,
        requested_at: u64,
        expected_generation: u64,
    ) -> Result<RetryClaim, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(RetryClaim::Lost);
        };
        if !RETRYABLE_STATUSES.contains(&record.status.as_str()) {
            return Ok(RetryClaim::Lost);
        }
        if record.generation != expected_generation {
            return Ok(RetryClaim::Lost);
        }
        if require_membership && !record.has_requester(user_id) {
            return Ok(RetryClaim::Lost);
        }
        record.status = target_status.to_owned();
        record.dispatch_authorized = dispatch_authorized;
        record.requested_at = requested_at;
        record.completed_at = None;
        record.task_id = None;
        record.generation = record.generation.saturating_add(1);
        Ok(RetryClaim::Claimed {
            generation: record.generation,
            target_status: target_status.to_owned(),
        })
    }

    /// Whether one user owns or co-requests a row.
    pub fn is_requester(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
    ) -> Result<bool, RequestsError> {
        let rows = self.read_rows()?;
        Ok(rows
            .get(&store_key(kind, key))
            .is_some_and(|record| record.has_requester(user_id)))
    }

    /// The atomic requester-cancel decision (v2
    /// `async_prepare_requester_cancel`): strangers are denied, co-requesters
    /// detach, waiting rows cancel outright, and owners of task rows move
    /// through `cancelling` so a failed task-cancel can restore the prior
    /// status instead of stranding the row.
    pub fn prepare_requester_cancel(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
        now_epoch: u64,
    ) -> Result<Option<CancelDecision>, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(None);
        };
        let prior = record.status.clone();
        if !record.has_requester(user_id) {
            return Ok(Some(CancelDecision::Denied {
                prior_status: prior,
            }));
        }
        let owned_by_caller = record.user_id.as_deref() == Some(user_id);
        if !owned_by_caller {
            record.requesters.retain(|r| r.user_id != user_id);
            record.generation = record.generation.saturating_add(1);
            return Ok(Some(CancelDecision::Detached));
        }
        if record.status == STATUS_AWAITING_APPROVAL {
            record.status = STATUS_CANCELLED.to_owned();
            record.completed_at = Some(now_epoch);
            record.dispatch_authorized = false;
            record.generation = record.generation.saturating_add(1);
            return Ok(Some(CancelDecision::CancelledDirect));
        }
        if !CANCELLABLE_STATUSES.contains(&record.status.as_str()) {
            return Ok(Some(CancelDecision::Denied {
                prior_status: prior,
            }));
        }
        let decision = CancelDecision::CancelTask {
            prior_status: prior,
            task_id: record.task_id.clone(),
            task_owner: record.user_id.clone().unwrap_or_else(|| user_id.to_owned()),
            generation: record.generation.saturating_add(1),
        };
        record.status = STATUS_CANCELLING.to_owned();
        record.generation = record.generation.saturating_add(1);
        Ok(Some(decision))
    }

    /// Restore a row's status after a failed cancel, but only when the row
    /// still sits in the expected status and generation.
    pub fn restore_status(
        &self,
        kind: RequestKind,
        key: &str,
        status: &str,
        expected_status: &str,
        expected_generation: u64,
    ) -> Result<bool, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(false);
        };
        if record.status != expected_status || record.generation != expected_generation {
            return Ok(false);
        }
        record.status = status.to_owned();
        record.completed_at = None;
        record.generation = record.generation.saturating_add(1);
        Ok(true)
    }

    /// Revoke or restore the persisted dispatch capability.
    pub fn set_dispatch_authorized(
        &self,
        kind: RequestKind,
        key: &str,
        value: bool,
    ) -> Result<(), RequestsError> {
        let mut rows = self.write_rows()?;
        if let Some(record) = rows.get_mut(&store_key(kind, key)) {
            record.dispatch_authorized = value;
            record.generation = record.generation.saturating_add(1);
        }
        Ok(())
    }

    /// Drop a row outright (admin history-clear).
    pub fn delete(&self, kind: RequestKind, key: &str) -> Result<bool, RequestsError> {
        let mut rows = self.write_rows()?;
        Ok(rows.remove(&store_key(kind, key)).is_some())
    }

    /// Hide a row from one user's own history (non-admin history-clear).
    pub fn dismiss(
        &self,
        kind: RequestKind,
        key: &str,
        user_id: &str,
    ) -> Result<bool, RequestsError> {
        let mut rows = self.write_rows()?;
        let Some(record) = rows.get_mut(&store_key(kind, key)) else {
            return Ok(false);
        };
        record.dismissed_by.insert(user_id.to_owned());
        record.generation = record.generation.saturating_add(1);
        Ok(true)
    }

    /// Live rows, newest first.
    pub fn active(
        &self,
        user_id: Option<&str>,
        kind: Option<RequestKind>,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        let rows = self.read_rows()?;
        let mut out: Vec<RequestRecord> = rows
            .values()
            .filter(|record| is_active(&record.status))
            .filter(|record| kind.is_none_or(|k| record.kind == k))
            .filter(|record| {
                user_id.is_none_or(|user| {
                    record.has_requester(user) && !record.dismissed_by.contains(user)
                })
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.requested_at.cmp(&a.requested_at));
        Ok(out)
    }

    /// Paged history rows with status filter and sort (v2 history shape:
    /// total pages floor at 1, computed by the service).
    pub fn history(
        &self,
        user_id: Option<&str>,
        kind: Option<RequestKind>,
        status_filter: Option<&str>,
        sort: &str,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        let rows = self.read_rows()?;
        let mut out: Vec<RequestRecord> = rows
            .values()
            .filter(|record| kind.is_none_or(|k| record.kind == k))
            .filter(|record| status_filter.is_none_or(|s| record.status == s))
            .filter(|record| {
                user_id.is_none_or(|user| {
                    record.has_requester(user) && !record.dismissed_by.contains(user)
                })
            })
            .cloned()
            .collect();
        match sort {
            "oldest" => out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at)),
            "status" => out.sort_by(|a, b| {
                a.status
                    .cmp(&b.status)
                    .then(b.requested_at.cmp(&a.requested_at))
            }),
            _ => out.sort_by(|a, b| b.requested_at.cmp(&a.requested_at)),
        }
        Ok(out)
    }

    /// Rows waiting for review, oldest first.
    pub fn pending_approvals(
        &self,
        kind: Option<RequestKind>,
    ) -> Result<Vec<RequestRecord>, RequestsError> {
        let rows = self.read_rows()?;
        let mut out: Vec<RequestRecord> = rows
            .values()
            .filter(|record| record.status == STATUS_AWAITING_APPROVAL)
            .filter(|record| kind.is_none_or(|k| record.kind == k))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(out)
    }

    fn read_rows(&self) -> Result<RowsReadGuard<'_>, RequestsError> {
        self.rows.read().map_err(|cause| {
            RequestsError::internal(&format_args!("request rows read failed: {cause}"))
        })
    }

    fn write_rows(&self) -> Result<RowsWriteGuard<'_>, RequestsError> {
        self.rows.write().map_err(|cause| {
            RequestsError::internal(&format_args!("request rows write failed: {cause}"))
        })
    }
}

impl Default for RequestStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Storage key: kind plus lowercased MBID (v2 folds keys case-insensitively).
fn store_key(kind: RequestKind, key: &str) -> (String, String) {
    (kind.as_str().to_owned(), key.to_lowercase())
}

/// One wanted watch.
#[derive(Debug, Clone)]
pub struct WantedWatch {
    /// Lowercased release-group MBID.
    pub key: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// `missing` (whole album) or `partial` (filling gaps).
    pub kind: String,
    /// `watching`, `paused`, or loop-set `dormant`.
    pub state: String,
    /// Passed checks so far.
    pub check_count: u32,
    /// Epoch seconds when the next check is due, when scheduled.
    pub next_check_at: Option<u64>,
    /// Unseen candidate count.
    pub new_candidate_count: u32,
    /// Epoch seconds when created.
    pub created_at: u64,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
    /// Release year.
    pub year: Option<i32>,
    /// Custom artwork URL override.
    pub cover_url: Option<String>,
    /// Owner id.
    pub user_id: String,
    /// Owner display name (served to admins only).
    pub user_name: Option<String>,
}

/// One still-scheduled auto-retry.
#[derive(Debug, Clone)]
pub struct WantedRetrying {
    /// Lowercased release-group MBID.
    pub key: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Attempts so far.
    pub retry_count: u32,
    /// Attempts allowed.
    pub max_attempts: u32,
    /// Epoch seconds when the next try is due, when scheduled.
    pub next_retry_at: Option<u64>,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
    /// Release year.
    pub year: Option<i32>,
    /// Custom artwork URL override.
    pub cover_url: Option<String>,
    /// Owner id.
    pub user_id: String,
    /// Owner display name (served to admins only).
    pub user_name: Option<String>,
}

/// Wanted watches plus the retrying set. The watcher loop lives in another
/// slice; this store holds the rows the views read and the stop/resume/seen
/// mutations flip.
pub struct WantedStore {
    /// Watches by lowercased MBID.
    watches: RwLock<HashMap<String, WantedWatch>>,
    /// Retrying entries by lowercased MBID.
    retrying: RwLock<HashMap<String, WantedRetrying>>,
}

impl WantedStore {
    /// Empty store.
    pub fn new() -> Self {
        Self {
            watches: RwLock::new(HashMap::new()),
            retrying: RwLock::new(HashMap::new()),
        }
    }

    /// Seed one watch (fixtures only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_watch(&self, watch: WantedWatch) {
        if let Ok(mut watches) = self.watches.write() {
            watches.insert(watch.key.to_lowercase(), watch);
        }
    }

    /// Seed one retrying entry (fixtures only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_retrying(&self, entry: WantedRetrying) {
        if let Ok(mut retrying) = self.retrying.write() {
            retrying.insert(entry.key.to_lowercase(), entry);
        }
    }

    /// Watches visible to one caller: own rows, or every row for admins (v2
    /// `list_watches_for` quirk).
    pub fn watches_for(
        &self,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Vec<WantedWatch>, RequestsError> {
        let watches = self.watches.read().map_err(|cause| {
            RequestsError::internal(&format_args!("wanted watches read failed: {cause}"))
        })?;
        let mut out: Vec<WantedWatch> = watches
            .values()
            .filter(|watch| is_admin || watch.user_id == user_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// Retrying entries visible to one caller.
    pub fn retrying_for(
        &self,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Vec<WantedRetrying>, RequestsError> {
        let retrying = self.retrying.read().map_err(|cause| {
            RequestsError::internal(&format_args!("wanted retrying read failed: {cause}"))
        })?;
        let mut out: Vec<WantedRetrying> = retrying
            .values()
            .filter(|entry| is_admin || entry.user_id == user_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// Pause one watch. Owners and admins only; missing or foreign is None.
    pub fn stop(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Option<WantedWatch>, RequestsError> {
        let mut watches = self.watches.write().map_err(|cause| {
            RequestsError::internal(&format_args!("wanted watches write failed: {cause}"))
        })?;
        let Some(watch) = watches.get_mut(&key.to_lowercase()) else {
            return Ok(None);
        };
        if !is_admin && watch.user_id != user_id {
            return Ok(None);
        }
        watch.state = "paused".to_owned();
        Ok(Some(watch.clone()))
    }

    /// Resume one watch. Owners and admins only; missing or foreign is None.
    pub fn resume(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Option<WantedWatch>, RequestsError> {
        let mut watches = self.watches.write().map_err(|cause| {
            RequestsError::internal(&format_args!("wanted watches write failed: {cause}"))
        })?;
        let Some(watch) = watches.get_mut(&key.to_lowercase()) else {
            return Ok(None);
        };
        if !is_admin && watch.user_id != user_id {
            return Ok(None);
        }
        watch.state = "watching".to_owned();
        Ok(Some(watch.clone()))
    }

    /// Clear one watch's unseen candidates. Owners and admins only.
    pub fn mark_seen(
        &self,
        key: &str,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Option<WantedWatch>, RequestsError> {
        let mut watches = self.watches.write().map_err(|cause| {
            RequestsError::internal(&format_args!("wanted watches write failed: {cause}"))
        })?;
        let Some(watch) = watches.get_mut(&key.to_lowercase()) else {
            return Ok(None);
        };
        if !is_admin && watch.user_id != user_id {
            return Ok(None);
        }
        watch.new_candidate_count = 0;
        Ok(Some(watch.clone()))
    }
}

impl Default for WantedStore {
    fn default() -> Self {
        Self::new()
    }
}

/// One auto-download approval row.
#[derive(Debug, Clone)]
pub struct FollowApproval {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Lowercased artist MBID.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// `pending`, `approved`, `rejected`, or `revoked`.
    pub state: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// One bulk-approval batch (Lidarr import grouping).
#[derive(Debug, Clone)]
pub struct ApprovalBatch {
    /// Batch id.
    pub batch_id: String,
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Covered artists.
    pub artists: Vec<(String, String)>,
    /// `pending`, `approved`, or `rejected`.
    pub state: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Auto-download approvals plus bulk batches. Reject and revoke keep the
/// follow and flip intent off (v2 `follow_service` L4 quirk); intent itself
/// lives with follows in the reads slice, so this store only tracks the
/// approval verdict.
pub struct FollowApprovalStore {
    /// Approvals by (user, lowercased artist MBID).
    approvals: RwLock<HashMap<(String, String), FollowApproval>>,
    /// Batches by id.
    batches: RwLock<HashMap<String, ApprovalBatch>>,
}

impl FollowApprovalStore {
    /// Empty store.
    pub fn new() -> Self {
        Self {
            approvals: RwLock::new(HashMap::new()),
            batches: RwLock::new(HashMap::new()),
        }
    }

    /// Seed one pending approval (fixtures only).
    pub fn seed_pending(&self, approval: FollowApproval) {
        if let Ok(mut approvals) = self.approvals.write() {
            approvals.insert(
                (
                    approval.user_id.clone(),
                    approval.artist_mbid.to_lowercase(),
                ),
                approval,
            );
        }
    }

    /// Seed one pending batch (fixtures only).
    pub fn seed_batch(&self, batch: ApprovalBatch) {
        if let Ok(mut batches) = self.batches.write() {
            batches.insert(batch.batch_id.clone(), batch);
        }
    }

    /// Pending approvals, oldest first.
    pub fn pending(&self) -> Result<Vec<FollowApproval>, RequestsError> {
        let approvals = self.approvals.read().map_err(|cause| {
            RequestsError::internal(&format_args!("follow approvals read failed: {cause}"))
        })?;
        let mut out: Vec<FollowApproval> = approvals
            .values()
            .filter(|row| row.state == "pending")
            .cloned()
            .collect();
        out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(out)
    }

    /// Pending units for the badge: pending rows plus pending batch artists
    /// (v2 `count_pending_approval_units`).
    pub fn pending_units(&self) -> Result<u32, RequestsError> {
        let approvals = self.approvals.read().map_err(|cause| {
            RequestsError::internal(&format_args!("follow approvals read failed: {cause}"))
        })?;
        let batches = self.batches.read().map_err(|cause| {
            RequestsError::internal(&format_args!("follow batches read failed: {cause}"))
        })?;
        let mut count = 0_u32;
        for row in approvals.values() {
            if row.state == "pending" {
                count = count.saturating_add(1);
            }
        }
        for batch in batches.values() {
            if batch.state == "pending" {
                count = count.saturating_add(batch.artists.len() as u32);
            }
        }
        Ok(count)
    }

    /// Move one approval out of `pending`. Approve and reject both require a
    /// pending row; anything else reports no match (v2 `set_approval_state`
    /// returns false the same way).
    pub fn decide(
        &self,
        user_id: &str,
        artist_mbid: &str,
        state: &str,
    ) -> Result<bool, RequestsError> {
        let mut approvals = self.approvals.write().map_err(|cause| {
            RequestsError::internal(&format_args!("follow approvals write failed: {cause}"))
        })?;
        let Some(row) = approvals.get_mut(&(user_id.to_owned(), artist_mbid.to_lowercase())) else {
            return Ok(false);
        };
        if row.state != "pending" {
            return Ok(false);
        }
        row.state = state.to_owned();
        Ok(true)
    }

    /// Withdraw a pending ask (the reads slice calls this when the user
    /// turns auto-download back off). Only pending rows withdraw.
    pub fn withdraw(&self, user_id: &str, artist_mbid: &str) -> Result<bool, RequestsError> {
        let mut approvals = self.approvals.write().map_err(|cause| {
            RequestsError::internal(&format_args!("follow approvals write failed: {cause}"))
        })?;
        let key = (user_id.to_owned(), artist_mbid.to_lowercase());
        let withdraw = approvals
            .get(&key)
            .is_some_and(|row| row.state == "pending");
        if withdraw {
            approvals.remove(&key);
        }
        Ok(withdraw)
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub fn revoke(&self, user_id: &str, artist_mbid: &str) -> Result<bool, RequestsError> {
        let mut approvals = self.approvals.write().map_err(|cause| {
            RequestsError::internal(&format_args!("follow approvals write failed: {cause}"))
        })?;
        let Some(row) = approvals.get_mut(&(user_id.to_owned(), artist_mbid.to_lowercase())) else {
            return Ok(false);
        };
        if row.state != "approved" {
            return Ok(false);
        }
        row.state = "revoked".to_owned();
        Ok(true)
    }

    /// Pending batches, oldest first.
    pub fn pending_batches(&self) -> Result<Vec<ApprovalBatch>, RequestsError> {
        let batches = self.batches.read().map_err(|cause| {
            RequestsError::internal(&format_args!("follow batches read failed: {cause}"))
        })?;
        let mut out: Vec<ApprovalBatch> = batches
            .values()
            .filter(|batch| batch.state == "pending")
            .cloned()
            .collect();
        out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(out)
    }

    /// Decide one batch. Returns the affected artist count, or zero when
    /// nothing matched (v2 `set_batch_approval_state`).
    pub fn decide_batch(&self, batch_id: &str, state: &str) -> Result<u32, RequestsError> {
        let mut batches = self.batches.write().map_err(|cause| {
            RequestsError::internal(&format_args!("follow batches write failed: {cause}"))
        })?;
        let Some(batch) = batches.get_mut(batch_id) else {
            return Ok(0);
        };
        if batch.state != "pending" {
            return Ok(0);
        }
        batch.state = state.to_owned();
        Ok(batch.artists.len() as u32)
    }
}

impl Default for FollowApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

/// One personal-mix auto-request approval row.
#[derive(Debug, Clone)]
pub struct MixApproval {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// `pending`, `approved`, `rejected`, or `revoked`.
    pub state: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Personal-mix approvals, refresh guard, and link flags. The mix build
/// itself lives in another slice; this store holds the approval queue plus
/// the refresh in-flight key (A:393 answers `already_running` while set).
pub struct PersonalMixStore {
    /// Approvals by user.
    approvals: RwLock<HashMap<String, MixApproval>>,
    /// Users with a refresh currently running.
    refresh_running: RwLock<HashSet<String>>,
    /// Users without a linked ListenBrainz account (everyone else is linked).
    unlinked: RwLock<HashSet<String>>,
}

impl PersonalMixStore {
    /// Empty store.
    pub fn new() -> Self {
        Self {
            approvals: RwLock::new(HashMap::new()),
            refresh_running: RwLock::new(HashSet::new()),
            unlinked: RwLock::new(HashSet::new()),
        }
    }

    /// Seed one pending approval (fixtures only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_pending(&self, approval: MixApproval) {
        if let Ok(mut approvals) = self.approvals.write() {
            approvals.insert(approval.user_id.clone(), approval);
        }
    }

    /// Mark one user unlinked (fixtures only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_unlinked(&self, user_id: &str) {
        if let Ok(mut unlinked) = self.unlinked.write() {
            unlinked.insert(user_id.to_owned());
        }
    }

    /// Whether one user may build a mix.
    pub fn is_linked(&self, user_id: &str) -> Result<bool, RequestsError> {
        let unlinked = self.unlinked.read().map_err(|cause| {
            RequestsError::internal(&format_args!("mix links read failed: {cause}"))
        })?;
        Ok(!unlinked.contains(user_id))
    }

    /// Pending approvals, oldest first.
    pub fn pending(&self) -> Result<Vec<MixApproval>, RequestsError> {
        let approvals = self.approvals.read().map_err(|cause| {
            RequestsError::internal(&format_args!("mix approvals read failed: {cause}"))
        })?;
        let mut out: Vec<MixApproval> = approvals
            .values()
            .filter(|row| row.state == "pending")
            .cloned()
            .collect();
        out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(out)
    }

    /// Pending approval count for the badge.
    pub fn pending_count(&self) -> Result<u32, RequestsError> {
        Ok(self.pending()?.len() as u32)
    }

    /// Move one approval out of `pending`. Only pending rows decide.
    pub fn decide(&self, user_id: &str, state: &str) -> Result<bool, RequestsError> {
        let mut approvals = self.approvals.write().map_err(|cause| {
            RequestsError::internal(&format_args!("mix approvals write failed: {cause}"))
        })?;
        let Some(row) = approvals.get_mut(user_id) else {
            return Ok(false);
        };
        if row.state != "pending" {
            return Ok(false);
        }
        row.state = state.to_owned();
        Ok(true)
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub fn revoke(&self, user_id: &str) -> Result<bool, RequestsError> {
        let mut approvals = self.approvals.write().map_err(|cause| {
            RequestsError::internal(&format_args!("mix approvals write failed: {cause}"))
        })?;
        let Some(row) = approvals.get_mut(user_id) else {
            return Ok(false);
        };
        if row.state != "approved" {
            return Ok(false);
        }
        row.state = "revoked".to_owned();
        Ok(true)
    }

    /// Claim the refresh key. False means a build already holds it.
    pub fn refresh_start(&self, user_id: &str) -> Result<bool, RequestsError> {
        let mut running = self.refresh_running.write().map_err(|cause| {
            RequestsError::internal(&format_args!("mix refresh write failed: {cause}"))
        })?;
        Ok(running.insert(user_id.to_owned()))
    }

    /// Release the refresh key once the build lands.
    pub fn refresh_finish(&self, user_id: &str) -> Result<(), RequestsError> {
        let mut running = self.refresh_running.write().map_err(|cause| {
            RequestsError::internal(&format_args!("mix refresh write failed: {cause}"))
        })?;
        running.remove(user_id);
        Ok(())
    }
}

impl Default for PersonalMixStore {
    fn default() -> Self {
        Self::new()
    }
}

/// One in-flight edition acquire (A:12).
#[derive(Debug, Clone)]
pub struct EditionMark {
    /// Lowercased release-group MBID.
    pub key: String,
    /// Linked download task.
    pub task_id: String,
    /// Epoch seconds when started.
    pub started_at: u64,
}

/// In-flight edition acquires. Rows exist only while a task runs; a
/// duplicate ask while one is live reports `already_in_progress` instead of
/// dispatching a rival fetch.
pub struct EditionStore {
    /// Marks by lowercased MBID.
    marks: RwLock<HashMap<String, EditionMark>>,
}

impl EditionStore {
    /// Empty store.
    pub fn new() -> Self {
        Self {
            marks: RwLock::new(HashMap::new()),
        }
    }

    /// Read one mark.
    pub fn get(&self, key: &str) -> Result<Option<EditionMark>, RequestsError> {
        let marks = self.marks.read().map_err(|cause| {
            RequestsError::internal(&format_args!("edition marks read failed: {cause}"))
        })?;
        Ok(marks.get(&key.to_lowercase()).cloned())
    }

    /// Set one mark.
    pub fn set(&self, mark: EditionMark) -> Result<(), RequestsError> {
        let mut marks = self.marks.write().map_err(|cause| {
            RequestsError::internal(&format_args!("edition marks write failed: {cause}"))
        })?;
        marks.insert(mark.key.to_lowercase(), mark);
        Ok(())
    }

    /// Clear one mark.
    pub fn clear(&self, key: &str) -> Result<(), RequestsError> {
        let mut marks = self.marks.write().map_err(|cause| {
            RequestsError::internal(&format_args!("edition marks write failed: {cause}"))
        })?;
        marks.remove(&key.to_lowercase());
        Ok(())
    }
}

impl Default for EditionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Current time as epoch seconds. Falls back to zero when the clock is
/// broken rather than failing the request.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Validate one MBID to the UUID shape v2 enforces (`validators.py`
/// `MBID_PATTERN`): 8-4-4-4-12 lowercase-insensitive hex with dashes.
pub fn validate_mbid(value: &str) -> Result<String, RequestsError> {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    let groups = [8, 4, 4, 4, 12];
    let mut index = 0;
    for (position, width) in groups.iter().enumerate() {
        if position > 0 {
            if bytes.get(index) != Some(&b'-') {
                return Err(invalid_mbid());
            }
            index += 1;
        }
        for _ in 0..*width {
            let Some(byte) = bytes.get(index) else {
                return Err(invalid_mbid());
            };
            if !byte.is_ascii_hexdigit() {
                return Err(invalid_mbid());
            }
            index += 1;
        }
    }
    if index != bytes.len() {
        return Err(invalid_mbid());
    }
    Ok(trimmed.to_owned())
}

/// The v2 MBID rejection message, kept word for word.
fn invalid_mbid() -> RequestsError {
    RequestsError::InvalidInput {
        message: "Invalid MBID format".to_owned(),
    }
}

/// None for absent, blank, or literal-"Unknown" names; else the stripped
/// value (v2 `request_service._meaningful_name` quirk: older callers send
/// placeholders that must not become stored names).
pub fn meaningful_name(value: Option<&str>) -> Option<String> {
    let text = value?.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("unknown") {
        return None;
    }
    Some(text.to_owned())
}
