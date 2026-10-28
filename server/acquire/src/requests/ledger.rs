//! Request ledger vocabulary: statuses, rows, and claim outcomes.
//!
//! One row per (kind, MBID); concurrent asks for the same key attach as
//! co-requesters instead of spawning rival rows. Every mutation carries a
//! generation compare-and-swap so a stale actor loses loudly instead of
//! overwriting a newer decision. The
//! SQLite stores in [`super::sqlite`] persist these rows.

use super::error::RequestsError;
use super::models::RequestKind;

/// One SQL conversion for a closed set stored as text: the value is its
/// `as_str`, and text outside the set fails decoding.
macro_rules! sql_text {
    ($type:ty) => {
        impl rusqlite::types::ToSql for $type {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
            }
        }

        impl rusqlite::types::FromSql for $type {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(
                        format!(concat!("not a ", stringify!($type), ": {}"), text).into(),
                    )
                })
            }
        }
    };
}

/// A request row's status. The `request_history.status` CHECK lists
/// exactly these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestStatus {
    /// Live, dispatched or about to be.
    Pending,
    /// Live, queued behind other fetches.
    Queued,
    /// Live and fetching.
    Downloading,
    /// Waits for an admin decision; no task exists yet.
    AwaitingApproval,
    /// Mid-cancel; new asks attach to the verdict, never a new row.
    Cancelling,
    /// Cancelled.
    Cancelled,
    /// Failed.
    Failed,
    /// Landed in the library.
    Imported,
    /// Landed short.
    Incomplete,
    /// Rejected by a reviewer.
    Rejected,
}

impl RequestStatus {
    /// Every status, in CHECK order.
    pub const ALL: [Self; 10] = [
        Self::Pending,
        Self::Queued,
        Self::Downloading,
        Self::AwaitingApproval,
        Self::Cancelling,
        Self::Cancelled,
        Self::Failed,
        Self::Imported,
        Self::Incomplete,
        Self::Rejected,
    ];

    /// Stored and wire form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Cancelling => "cancelling",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Imported => "imported",
            Self::Incomplete => "incomplete",
            Self::Rejected => "rejected",
        }
    }

    /// Read back a stored or requested status.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }

    /// Asks that keep a row alive.
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Downloading | Self::Queued | Self::AwaitingApproval
        )
    }

    /// Tasks an owner may still stop.
    pub const fn is_cancellable(self) -> bool {
        matches!(self, Self::Pending | Self::Downloading | Self::Queued)
    }

    /// Finished rows a retry may revive.
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled | Self::Incomplete)
    }

    /// Finished rows history-clearing may drop.
    pub const fn is_clearable(self) -> bool {
        matches!(
            self,
            Self::Imported | Self::Incomplete | Self::Failed | Self::Cancelled
        )
    }
}

impl std::fmt::Display for RequestStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

sql_text!(RequestStatus);
sql_text!(RequestKind);

/// Whether a request row may move `from -> to`.
///
/// Advisory, like the download task table: the store never refuses a
/// write for a missing edge. Tests and reviewers assert legality with it.
pub fn can_request_transition(from: RequestStatus, to: RequestStatus) -> bool {
    use RequestStatus as S;
    let settles = matches!(to, S::Cancelled | S::Failed | S::Imported | S::Incomplete);
    let restarts = matches!(to, S::Pending | S::AwaitingApproval | S::Queued);
    match from {
        // Live rows follow their task, and their owner may cancel. An
        // approval whose dispatch could not start goes back to waiting.
        // A row started from waiting for quota room whose download could
        // not be queued goes back to waiting.
        S::Pending => {
            settles
                || matches!(
                    to,
                    S::Downloading | S::Cancelling | S::AwaitingApproval | S::Queued
                )
        }
        // A queued row with no task waits for quota room, then starts.
        S::Queued => settles || matches!(to, S::Pending | S::Downloading | S::Cancelling),
        S::Downloading => settles || to == S::Cancelling,
        // An admin decides; the owner may withdraw; an album the library
        // already holds leaves nothing to approve.
        S::AwaitingApproval => matches!(to, S::Pending | S::Rejected | S::Cancelled | S::Imported),
        // A cancel lands, or a failed one puts the row back.
        S::Cancelling => matches!(to, S::Cancelled | S::Pending | S::Queued | S::Downloading),
        // Finished rows start again through a new ask or a retry, and
        // follow a new download of their album. A failed or short one the
        // library now holds settles as imported.
        S::Failed | S::Incomplete => restarts || matches!(to, S::Downloading | S::Imported),
        S::Cancelled => restarts || to == S::Downloading,
        S::Imported | S::Rejected => restarts,
    }
}

/// A wanted watch's state. The `wanted_watches.state` CHECK lists exactly
/// these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WatchState {
    /// Checked on its schedule.
    Watching,
    /// Stopped by its owner or an admin.
    Stopped,
    /// The album reached the library.
    Fulfilled,
}

impl WatchState {
    /// Every state, in CHECK order.
    pub const ALL: [Self; 3] = [Self::Watching, Self::Stopped, Self::Fulfilled];

    /// Stored and wire form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Stopped => "stopped",
            Self::Fulfilled => "fulfilled",
        }
    }

    /// Read back a stored state.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

sql_text!(WatchState);

/// Whether a watch may move `from -> to`. A stopped watch resumes; a
/// fulfilled one watches again when its album leaves the library.
pub fn can_watch_transition(from: WatchState, to: WatchState) -> bool {
    use WatchState as W;
    matches!(
        (from, to),
        (W::Watching, W::Stopped | W::Fulfilled)
            | (W::Stopped, W::Watching | W::Fulfilled)
            | (W::Fulfilled, W::Watching)
    )
}

/// An approval's state, for follow auto-downloads and personal mixes. The
/// `auto_download_approvals.state` CHECK lists exactly these values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalState {
    /// Waits for an admin.
    Pending,
    /// Approved.
    Approved,
    /// Rejected.
    Rejected,
    /// Approved once, then withdrawn.
    Revoked,
}

impl ApprovalState {
    /// Every state, in CHECK order.
    pub const ALL: [Self; 4] = [Self::Pending, Self::Approved, Self::Rejected, Self::Revoked];

    /// Stored and wire form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Revoked => "revoked",
        }
    }

    /// Read back a stored state.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

sql_text!(ApprovalState);

/// Whether an approval may move `from -> to`. A new ask puts a rejected
/// or revoked row back to pending.
pub fn can_approval_transition(from: ApprovalState, to: ApprovalState) -> bool {
    use ApprovalState as A;
    matches!(
        (from, to),
        (A::Pending, A::Approved | A::Rejected)
            | (A::Approved, A::Revoked)
            | (A::Rejected | A::Revoked, A::Pending)
    )
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
    pub status: RequestStatus,
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
    /// Immutable primary owner; retries keep it.
    pub user_id: Option<String>,
    /// What made the request.
    pub origin: crate::dispatch::DispatchOrigin,
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
    /// provenance.
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

    /// Whether the row waits for room in its owner's quota: queued with no
    /// download yet. Only such a row is queued without a task.
    pub fn waits_for_quota(&self) -> bool {
        self.status == RequestStatus::Queued && self.task_id.is_none()
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
        prior_status: RequestStatus,
    },
    /// Co-requester detached; the shared row continues.
    Detached,
    /// Waiting row cancelled outright (no task existed).
    CancelledDirect,
    /// Owner must cancel the linked task, then finalize.
    CancelTask {
        /// Status at decision time.
        prior_status: RequestStatus,
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
        target_status: RequestStatus,
    },
    /// Lost: row missing, moved on, or foreign.
    Lost,
}

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
    /// Watch state.
    pub state: WatchState,
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
    /// Approval state.
    pub state: ApprovalState,
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
    /// Approval state.
    pub state: ApprovalState,
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

/// Validate one MBID to the UUID shape: 8-4-4-4-12 lowercase-insensitive hex
/// with dashes.
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

/// The MBID rejection message.
fn invalid_mbid() -> RequestsError {
    RequestsError::InvalidInput {
        message: "Invalid MBID format".to_owned(),
    }
}

/// None for absent, blank, or literal-"Unknown" names; else the stripped
/// value (older callers send placeholders that must not become stored names).
pub fn meaningful_name(value: Option<&str>) -> Option<String> {
    let text = value?.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("unknown") {
        return None;
    }
    Some(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalState, RequestStatus, WatchState, can_approval_transition, can_request_transition,
        can_watch_transition,
    };

    /// Every request status pair: exactly the listed moves are allowed.
    #[test]
    fn request_transitions_are_exactly_the_listed_pairs() {
        use RequestStatus::{
            AwaitingApproval, Cancelled, Cancelling, Downloading, Failed, Imported, Incomplete,
            Pending, Queued, Rejected,
        };
        let live_settles = [Cancelled, Failed, Imported, Incomplete];
        let mut allowed = Vec::new();
        for from in [Pending, Queued] {
            for to in live_settles.into_iter().chain([Downloading, Cancelling]) {
                allowed.push((from, to));
            }
        }
        allowed.push((Pending, AwaitingApproval));
        allowed.push((Queued, Pending));
        allowed.push((Pending, Queued));
        for to in live_settles.into_iter().chain([Cancelling]) {
            allowed.push((Downloading, to));
        }
        for to in [Pending, Rejected, Cancelled, Imported] {
            allowed.push((AwaitingApproval, to));
        }
        for to in [Cancelled, Pending, Queued, Downloading] {
            allowed.push((Cancelling, to));
        }
        for from in [Failed, Cancelled, Incomplete] {
            for to in [Pending, AwaitingApproval, Queued, Downloading] {
                allowed.push((from, to));
            }
        }
        for from in [Failed, Incomplete] {
            allowed.push((from, Imported));
        }
        for from in [Imported, Rejected] {
            for to in [Pending, AwaitingApproval, Queued] {
                allowed.push((from, to));
            }
        }
        for from in RequestStatus::ALL {
            for to in RequestStatus::ALL {
                assert_eq!(
                    can_request_transition(from, to),
                    allowed.contains(&(from, to)),
                    "{from} -> {to}"
                );
            }
        }
    }

    /// Every watch state pair: exactly the listed moves are allowed.
    #[test]
    fn watch_transitions_are_exactly_the_listed_pairs() {
        use WatchState::{Fulfilled, Stopped, Watching};
        let allowed = [
            (Watching, Stopped),
            (Watching, Fulfilled),
            (Stopped, Watching),
            (Stopped, Fulfilled),
            (Fulfilled, Watching),
        ];
        for from in WatchState::ALL {
            for to in WatchState::ALL {
                assert_eq!(
                    can_watch_transition(from, to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    /// Every approval state pair: exactly the listed moves are allowed.
    #[test]
    fn approval_transitions_are_exactly_the_listed_pairs() {
        use ApprovalState::{Approved, Pending, Rejected, Revoked};
        let allowed = [
            (Pending, Approved),
            (Pending, Rejected),
            (Approved, Revoked),
            (Rejected, Pending),
            (Revoked, Pending),
        ];
        for from in ApprovalState::ALL {
            for to in ApprovalState::ALL {
                assert_eq!(
                    can_approval_transition(from, to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }
}
