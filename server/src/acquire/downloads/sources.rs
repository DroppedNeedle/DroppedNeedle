//! Per-source fetch seam: the minimal traits the durable core needs.
//!
//! SEAM NOTE (integrator): the slskd and usenet clients live in other
//! slices. This module defines only the narrow surface downloads depend
//! on - enqueue, poll, inspect, discard - ported from v2's
//! `repositories/protocols/download_client.py`. Source adapters implement
//! [`DownloadSource`]; the watchdog, recovery, and orphan logic below
//! never touch a client directly.

use std::future::Future;
use std::path::PathBuf;

/// Client correlation handle. Mirrors v2's `TaskHandle`: soulseek fills
/// username plus filenames (no batch id); usenet fills the job name before
/// enqueue so a crash between enqueue and journaling stays recoverable.
/// Serializes into the attempt journal's `handle_json` at enqueue time.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SourceHandle {
    /// `soulseek`, `usenet`, or `plugin:<key>`.
    pub source: String,
    /// Soulseek peer username.
    pub username: String,
    /// Exact enqueued filenames.
    pub filenames: Vec<String>,
    /// Client job name.
    pub job_name: String,
}

/// One poll's view of a transfer batch.
#[derive(Debug, Clone, PartialEq)]
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

/// Terminal fetch result for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Every transfer terminal and succeeded.
    Completed,
    /// Every transfer terminal, at least one failed.
    Terminal,
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
    /// The client rejected the request itself.
    #[error("client rejected request: {0}")]
    Rejected(String),
}

/// Minimal per-source fetch surface. Object-safe and sync-friendly: each
/// method returns an opaque future so adapters can be async without this
/// slice depending on an executor.
pub trait DownloadSource: Send + Sync {
    /// Adapter name (`slskd`, `sabnzbd`, `plugin:<key>`).
    fn name(&self) -> &str;

    /// Enqueue a candidate; returns the correlation handle.
    fn enqueue(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> impl Future<Output = Result<SourceHandle, SourceError>> + Send;

    /// Poll current progress for a handle.
    fn poll(
        &self,
        handle: &SourceHandle,
    ) -> impl Future<Output = Result<TransferProgress, SourceError>> + Send;

    /// Resolve current client state and exact local evidence.
    fn inspect(
        &self,
        handle: &SourceHandle,
    ) -> impl Future<Output = Result<Materialization, SourceError>> + Send;

    /// Remove transfer/history records after local cleanup is durable.
    /// Best-effort: callers log and continue on failure.
    fn discard(
        &self,
        handle: &SourceHandle,
    ) -> impl Future<Output = Result<bool, SourceError>> + Send;

    /// Abort a live batch. Returns false when nothing was running.
    fn abort(
        &self,
        handle: &SourceHandle,
    ) -> impl Future<Output = Result<bool, SourceError>> + Send;
}

/// Ownership lookups the orphan reconciler needs. Kept as a trait (rather
/// than store methods) because publisher-bundle state belongs to the
/// library slice, not downloads.
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

    /// True when every publisher bundle for the task completed (or none
    /// exist). Any other bundle state keeps the folder.
    fn bundles_settled(
        &self,
        task_id: &str,
    ) -> impl Future<Output = Result<bool, SourceError>> + Send;
}
