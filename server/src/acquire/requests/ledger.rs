//! Request ledger vocabulary: statuses, rows, and claim outcomes.
//!
//! One row per (kind, MBID); concurrent asks for the same key attach as
//! co-requesters instead of spawning rival rows. Every mutation carries a
//! generation compare-and-swap so a stale actor loses loudly instead of
//! overwriting a newer decision (v2 `expected_generation` quirk). The
//! SQLite stores in [`super::sqlite`] persist these rows.

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

/// Request-count quota to enforce inside the write that admits an ask.
#[derive(Debug, Clone)]
pub struct QuotaGate {
    /// Asks allowed in the window (never zero; zero means no gate).
    pub limit: u32,
    /// Window length in days.
    pub window_days: u32,
    /// Asks this write would add.
    pub new_requests: u32,
}

/// Why a quota gate refused an ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaRefusal {
    /// Asks already counted in the window.
    pub used: u32,
    /// Asks allowed in the window.
    pub limit: u32,
    /// Window length in days.
    pub window_days: u32,
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

/// Wanted watch states (v2 `wanted_watches.state`).
pub const WATCH_WATCHING: &str = "watching";
/// Watched long enough without a find; the loop parks it.
pub const WATCH_DORMANT: &str = "dormant";
/// Stopped by its owner or an admin.
pub const WATCH_STOPPED: &str = "stopped";
/// Satisfied: the album reached the library.
pub const WATCH_FULFILLED: &str = "fulfilled";

/// One wanted watch: a user waiting on an album no source has yet.
#[derive(Debug, Clone, PartialEq)]
pub struct WantedWatch {
    /// Lowercased release-group MBID.
    pub key: String,
    /// Owner id.
    pub user_id: String,
    /// Owner display name (served to admins only).
    pub user_name: Option<String>,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
    /// Release year.
    pub year: Option<i32>,
    /// Custom artwork URL override.
    pub cover_url: Option<String>,
    /// `missing` (whole album) or `partial` (filling gaps).
    pub kind: String,
    /// One of the `WATCH_*` states.
    pub state: String,
    /// Epoch seconds when created (or last resumed).
    pub created_at: u64,
    /// First release date (`YYYY-MM-DD`, possibly partial), when known.
    pub first_release_date: Option<String>,
    /// Checks so far.
    pub check_count: u32,
    /// Consecutive quiet checks; long streaks back off to 28 days.
    pub quiet_streak: u32,
    /// Epoch seconds when the next check is due.
    pub next_check_at: u64,
    /// Unseen candidate count.
    pub new_candidate_count: u32,
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

/// One in-flight edition acquire.
#[derive(Debug, Clone)]
pub struct EditionMark {
    /// Lowercased release-group MBID.
    pub key: String,
    /// Linked download task.
    pub task_id: String,
    /// Epoch seconds when started.
    pub started_at: u64,
}

pub use crate::acquire::db::now_epoch;

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
