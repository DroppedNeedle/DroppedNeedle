//! Download-dispatch seam.
//!
//! Intake records the ask and decides who waits; the downloads module owns the
//! actual fetch. Everything intake needs from downloads lives in
//! [`DownloadDispatch`]: start one fetch, cancel one fetch, and read one
//! fetch's state for status sync. The downloads module implements the
//! durable side.

use futures_util::future::BoxFuture;

/// What triggered a dispatch. Origins decide quota exemptions, never routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOrigin {
    /// A fresh ask from intake.
    User,
    /// An admin approval releasing a waiting ask.
    Approval,
    /// A retry of a terminal ask (quota-exempt: already admitted once).
    Retry,
    /// A wanted-watch redispatch (quota-exempt for the same reason).
    Wanted,
    /// An upgrade of owned files (size-neutral, quota-exempt).
    Upgrade,
    /// An edition fill/upgrade.
    Edition,
}

impl DispatchOrigin {
    /// Wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Approval => "approval",
            Self::Retry => "retry",
            Self::Wanted => "wanted",
            Self::Upgrade => "upgrade",
            Self::Edition => "edition",
        }
    }
}

/// One fetch to start.
#[derive(Debug, Clone)]
pub struct DispatchRequest {
    /// Primary owner id (immutable request attribution, never the actor).
    pub user_id: String,
    /// `album`, `track`, or `edition`.
    pub kind: String,
    /// Album, recording, or release-group id.
    pub key: String,
    /// Artist name.
    pub artist_name: String,
    /// Album or track title.
    pub title: String,
    /// What triggered this dispatch.
    pub origin: DispatchOrigin,
    /// Edition release id, when pinned.
    pub release_mbid: Option<String>,
    /// For an album on `release_mbid`: only these (disc, position) tracks
    /// are fetched, from the best whole-album source. Empty fetches the
    /// whole album.
    pub wanted: Vec<(u32, u32)>,
    /// For an album on `release_mbid`: the library copy it is fetched
    /// for, which the files then join.
    pub planned_album: Option<crate::downloads::manifest::PlannedAlbum>,
    /// Caller-supplied idempotency key. A repeat dispatch with the same
    /// key answers the original task instead of minting a duplicate;
    /// `None` always mints fresh.
    pub idempotency_key: Option<String>,
}

/// How a dispatch call resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// A fetch started under this task id.
    Dispatched {
        /// Download task id.
        task_id: String,
    },
    /// The ask already lives in the library (v2 `ALREADY_IN_LIBRARY`).
    AlreadyInLibrary,
}

/// One fetch's state as status sync reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchTaskState {
    /// Still working.
    Active,
    /// Landed and imported.
    Imported,
    /// Landed short.
    Incomplete,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
    /// Unknown task id.
    Missing,
}

/// Dispatch failures. Validation faults are the caller's problem (quota,
/// policy); transport faults are the downloads module's problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The ask passed intake but failed admission (message is user-safe).
    Validation(String),
    /// The fetch could not start (message stays out of responses).
    Failed(String),
}

/// One fetch's progress snapshot as the request views read it. Every field
/// mirrors a `download_tasks` column; the views render what is present and
/// hide what is not (v2 served these same shapes, always empty).
#[derive(Debug, Clone, Default)]
pub struct TaskProgress {
    /// Task status wire string (`queued`, `downloading`, ...).
    pub status: String,
    /// Progress percent (0-100) the worker last reported.
    pub progress_percent: i64,
    /// Total transfer bytes, once known.
    pub total_size_bytes: Option<i64>,
    /// Bytes transferred so far.
    pub downloaded_bytes: i64,
    /// Last outcome text.
    pub error_message: Option<String>,
    /// Picked candidate quality (format label), once picked.
    pub quality: Option<String>,
    /// Fetch source (`soulseek`, `usenet`, `plugin:<key>`).
    pub protocol: String,
}

/// The durability seam. The downloads module owns the implementation; intake
/// only starts, cancels, and polls through here.
pub trait DownloadDispatch: Send + Sync {
    /// Start one fetch for an admitted ask.
    fn dispatch<'a>(
        &'a self,
        request: &'a DispatchRequest,
    ) -> BoxFuture<'a, Result<DispatchOutcome, DispatchError>>;
    /// Cancel one fetch: the task stops and its live transfer is handed to
    /// cleanup. Unknown or finished ids are a no-op success.
    fn cancel_task<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<(), DispatchError>>;
    /// Read one fetch's state for status sync.
    fn task_state<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<DispatchTaskState, DispatchError>>;
    /// Read one fetch's progress snapshot for the request views. Unknown
    /// ids answer None.
    fn task_progress<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<TaskProgress>, DispatchError>>;
    /// Whether one fetch can be reimported (failed or short-landed with
    /// its candidate still linked).
    fn reimportable<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<bool, DispatchError>>;
    /// The pending auto-retry for one failed fetch, or None when no retry
    /// is scheduled (auto-retry off, ladder exhausted, or not failed).
    fn retry_schedule<'a>(
        &'a self,
        task_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<RetrySchedule>, DispatchError>>;
    /// The newest fetch one owner started for an album or recording at or
    /// after `since` (epoch seconds). Startup recovery uses it to relink a
    /// request whose task was created but never linked.
    fn find_task_since<'a>(
        &'a self,
        owner: &'a str,
        kind: &'a str,
        key: &'a str,
        since: u64,
    ) -> BoxFuture<'a, Result<Option<String>, DispatchError>>;
}

/// One scheduled auto-retry for a failed fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySchedule {
    /// Retries already spent.
    pub retry_count: u32,
    /// Retries the ladder allows.
    pub max_attempts: u32,
    /// Epoch seconds when the next retry is due.
    pub next_retry_at: u64,
}
