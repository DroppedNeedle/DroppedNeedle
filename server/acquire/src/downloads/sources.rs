//! Per-source fetch seam: the minimal traits the durable core needs.
//!
//! The slskd and usenet clients live in their own modules. This one defines
//! only the narrow surface downloads depend on (search, enqueue, poll,
//! landed, discard, abort). Source adapters implement
//! [`DownloadSource`]; the watchdog, recovery, and orphan logic below
//! never touch a client directly.

use std::future::Future;
use std::path::PathBuf;

use futures_util::future::BoxFuture;

use super::source_tag::SourceTag;
use crate::landing::Landed;
use crate::reason::Reason;
use crate::search_jobs::candidates::Candidate;
use crate::target::SearchTarget;

/// Client correlation handle: soulseek fills
/// username plus filenames (no batch id); usenet fills the job name before
/// enqueue so a crash between enqueue and journaling stays recoverable.
/// Serializes into the attempt journal's `handle_json` at enqueue time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceHandle {
    /// The source that holds the job.
    pub source: SourceTag,
    /// Soulseek peer username.
    pub username: String,
    /// Exact enqueued filenames.
    pub filenames: Vec<String>,
    /// Client job name.
    pub job_name: String,
    /// SABnzbd's own job id, once the add returned one. Polls and cleanup
    /// match on it so a job SABnzbd renamed is still found. Empty for
    /// soulseek handles.
    #[serde(default)]
    pub nzo_id: String,
    /// A plugin client's correlation token (the search result payload),
    /// handed back on every call. Empty for built-in sources.
    #[serde(default)]
    pub plugin_token: String,
    /// Advertised byte size of each file in `filenames` (same order), for
    /// locating the right copy and the size check at import. Empty when
    /// the source does not advertise sizes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sizes: Vec<i64>,
    /// The identity of the Usenet or plugin release this attempt fetches,
    /// which a failover blocklists. Empty for soulseek handles, which are
    /// identified by peer and files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub release_identity: String,
    /// This attempt fetches exactly the candidate a person picked (never a
    /// failover or a fallback search), so a quality waiver confirmed with
    /// that pick covers it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

impl SourceHandle {
    /// An empty handle on `source`, for the caller to fill in.
    pub fn new(source: SourceTag) -> Self {
        Self {
            source,
            username: String::new(),
            filenames: Vec::new(),
            job_name: String::new(),
            nzo_id: String::new(),
            plugin_token: String::new(),
            sizes: Vec::new(),
            release_identity: String::new(),
            pinned: false,
        }
    }
}

/// One poll's view of a transfer batch.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TransferProgress {
    /// True once every transfer reached a client-terminal state.
    pub all_terminal: bool,
    /// True when every terminal transfer succeeded.
    pub all_succeeded: bool,
    /// At least one transfer is actively moving bytes right now.
    pub has_active_transfer: bool,
    /// Total bytes downloaded so far across the batch.
    pub downloaded_bytes: u64,
    /// Filenames that completed successfully.
    pub succeeded_filenames: Vec<String>,
    /// Rank in the peer's remote upload queue, when queued.
    pub queue_position_start: Option<i64>,
    /// Newest observed queue rank.
    pub queue_position_end: Option<i64>,
    /// Total bytes of the batch, when the client knows it.
    pub total_bytes: Option<u64>,
    /// Files in the batch (usenet jobs count as one).
    pub files_total: u32,
    /// Files finished so far.
    pub files_completed: u32,
    /// Files the client gave up on.
    pub files_failed: u32,
}

/// What a source reports about already-materialized files.
#[derive(Debug, Clone, PartialEq)]
pub struct Materialization {
    /// The client's storage is reachable and healthy.
    pub mount_healthy: bool,
    /// `active` while the client still owns the job.
    pub state: String,
    /// Exact on-disk paths the client produced.
    pub paths: Vec<PathBuf>,
}

/// Source failures. Local faults (disk full, mount gone) must surface as
/// [`SourceError::LocalFault`] so callers retry with backoff instead of
/// blocklisting a healthy release.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The client endpoint is unreachable or refused auth.
    #[error("download client unavailable: {0}")]
    Unavailable(String),
    /// A local/environment fault: disk, mount, or permissions.
    #[error("local fault: {0}")]
    LocalFault(String),
    /// The source refused or failed the request itself: `reason` says
    /// why, `detail` is the text the log keeps.
    #[error("client rejected request: {detail}")]
    Rejected { reason: Reason, detail: String },
}

impl SourceError {
    /// The source refused for `reason`, with `detail` for the log.
    pub fn rejected(reason: impl Into<Reason>, detail: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
            detail: detail.into(),
        }
    }
}

/// One download source: a client that searches, fetches and reports.
/// The worker holds every enabled source as `Arc<dyn DownloadSource>` in
/// the configured try order; manual search, the worker's look before a
/// grab and the wanted watcher all search through [`Self::search`].
pub trait DownloadSource: Send + Sync {
    /// Which source this is, as journaled on attempts and handles.
    fn tag(&self) -> SourceTag;

    /// The target album's candidates on this source, ranked and gated by
    /// the download policy (blocklist, quality, size, retention).
    fn search<'a>(
        &'a self,
        target: &'a SearchTarget,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, SourceError>>;

    /// Enqueue the task's `candidate_index`-th pick on this source (the
    /// source picks; a manual pick goes first); returns the correlation
    /// handle.
    fn enqueue<'a>(
        &'a self,
        task_id: &'a str,
        candidate_index: i64,
    ) -> BoxFuture<'a, Result<SourceHandle, SourceError>>;

    /// Poll current progress for a handle.
    fn poll<'a>(
        &'a self,
        handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<TransferProgress, SourceError>>;

    /// The files (or job folder) a finished handle produced, with the
    /// client's downloads folders the landing confines them to.
    fn landed<'a>(&'a self, handle: &'a SourceHandle)
    -> BoxFuture<'a, Result<Landed, SourceError>>;

    /// Remove transfer/history records after local cleanup is durable.
    /// Best-effort: callers log and continue on failure.
    fn discard<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>>;

    /// Abort a live batch. Returns false when nothing was running.
    fn abort<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>>;

    /// Whether the client still owns one job (orphan evidence). `None`
    /// means the source cannot tell, and the folder stays.
    fn job_active<'a>(&'a self, _handle: &'a SourceHandle) -> BoxFuture<'a, Option<bool>> {
        Box::pin(async { None })
    }

    /// Client-side mount health for orphan evidence. `None` fails closed
    /// (the folder stays).
    fn mount_healthy<'a>(&'a self, _handle: &'a SourceHandle) -> BoxFuture<'a, Option<bool>> {
        Box::pin(async { None })
    }
}

/// Ownership lookups the orphan reconciler needs. Kept as a trait (rather
/// than store methods) because publisher-bundle state belongs to the
/// library module, not downloads.
pub trait OrphanOwnership: Send + Sync {
    /// True when any attempt journal still references this job.
    fn has_cleanup_debt(
        &self,
        source: &str,
        task_id: &str,
        job_name: &str,
    ) -> impl Future<Output = Result<bool, SourceError>> + Send;

    /// Owning task status, when the task row still exists.
    fn task_status(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<Option<String>, SourceError>> + Send;

    /// True when every publisher bundle for the task settled (or none
    /// exist). Any other bundle state keeps the folder.
    fn bundles_settled(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<bool, SourceError>> + Send;
}
