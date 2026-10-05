//! Clean-slate wire shapes for request intake.
//!
//! Times are epoch seconds; ids are MusicBrainz ids validated to the UUID
//! shape v2 enforces (its `MBID_PATTERN` validator).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What kind of thing a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// A whole album by release-group id.
    Album,
    /// One exact recording by recording id.
    Track,
}

impl RequestKind {
    /// Parse the `kind` query/body value. Unknown values fail closed to None.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "album" => Some(Self::Album),
            "track" => Some(Self::Track),
            _ => None,
        }
    }

    /// Wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Track => "track",
        }
    }
}

/// Album intake body. Names may be omitted only when they resolve; intake
/// has no catalog lookup, so blank or literal-"Unknown" names are
/// rejected (v2 `request_service._meaningful_name` quirk).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AlbumIntake {
    /// Release-group MBID.
    pub musicbrainz_id: String,
    /// Artist name.
    pub artist: Option<String>,
    /// Album title.
    pub album: Option<String>,
    /// Release year.
    pub year: Option<i32>,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
    /// Follow the artist for new releases.
    #[serde(default)]
    pub monitor_artist: bool,
    /// Auto-download the artist's future releases (approval-gated for users).
    #[serde(default)]
    pub auto_download_artist: bool,
}

/// Exact-track intake body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TrackIntake {
    /// Recording MBID.
    pub recording_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Track title.
    pub track_title: String,
    /// Album title, when known.
    pub album_title: Option<String>,
    /// Track length in seconds.
    pub duration_seconds: Option<i64>,
    /// Containing release-group MBID, when known.
    pub release_group_mbid: Option<String>,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
    /// Release MBID.
    pub release_mbid: Option<String>,
}

/// One batch row.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BatchItem {
    /// Release-group MBID.
    pub musicbrainz_id: String,
    /// Artist name.
    pub artist_name: Option<String>,
    /// Album title.
    pub album_title: Option<String>,
    /// Release year.
    pub year: Option<i32>,
    /// Artist MBID.
    pub artist_mbid: Option<String>,
}

/// Batch intake body (v2 caps batches at 500 rows).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BatchIntake {
    /// Rows to request.
    pub items: Vec<BatchItem>,
    /// Follow each artist for new releases.
    #[serde(default)]
    pub monitor_artist: bool,
    /// Auto-download future releases (approval-gated for users).
    #[serde(default)]
    pub auto_download_artist: bool,
}

/// Batch-cancel body. Dedupe is by exact string before lookup.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BatchCancelBody {
    /// MBIDs to cancel.
    pub musicbrainz_ids: Vec<String>,
    /// Request kind; defaults to album.
    pub kind: Option<String>,
}

/// `kind` query shared by the per-request mutations.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct KindQuery {
    /// `album` (default) or `track`.
    pub kind: Option<String>,
}

/// History paging and filter query.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct HistoryQuery {
    /// 1-based page.
    pub page: Option<u32>,
    /// Rows per page (1..=100).
    pub page_size: Option<u32>,
    /// Status filter.
    pub status: Option<String>,
    /// `newest` (default), `oldest`, or `status`.
    pub sort: Option<String>,
    /// `album` or `track`; unset means both.
    pub kind: Option<String>,
}

/// Intake response. Clients render this decision, never infer from roles.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IntakeResponse {
    /// Whether the ask was recorded.
    pub success: bool,
    /// Human outcome.
    pub message: String,
    /// Canonical MBID.
    pub musicbrainz_id: String,
    /// Row status after this call.
    pub status: String,
    /// Linked download task, when dispatched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Exact-track intake response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TrackIntakeResponse {
    /// `queued`, `awaiting_approval`, or `already_in_library`.
    pub status: String,
    /// Linked download task, when dispatched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Batch intake response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BatchIntakeResponse {
    /// Whether the batch recorded anything.
    pub success: bool,
    /// Human outcome.
    pub message: String,
    /// Rows recorded (auto roles: rows dispatched).
    pub requested: u32,
    /// Rows skipped (duplicates, in-progress, unresolvable).
    pub skipped: u32,
    /// Rows over the batch cap; always zero (cap rejects the batch instead).
    pub overflow: u32,
    /// Batch decision: `pending`, `awaiting_approval`, `already_requested`,
    /// or `failed`.
    pub status: String,
}

/// Batch-cancel response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BatchCancelResponse {
    /// Whether anything was cancelled.
    pub success: bool,
    /// Rows cancelled or detached.
    pub cancelled: u32,
    /// Rows that failed.
    pub failed: u32,
    /// Human outcome.
    pub message: String,
}

/// One grouped status detail behind the request card's expander. Intake
/// has no producer for these yet (v2 never sent them
/// either), so views answer None until one exists.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RequestStatusMessage {
    /// Group heading, when grouped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Detail lines.
    pub messages: Vec<String>,
}

/// One request row in list views.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RequestItem {
    /// Album or recording MBID.
    pub musicbrainz_id: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Artist MBID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Release year.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// Custom artwork URL override. None: artwork resolves client-side
    /// from the MBID, which is what v2's built cover URL did anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// Epoch seconds when asked.
    pub requested_at: u64,
    /// Epoch seconds when terminal, when terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    /// Row status.
    pub status: String,
    /// Download progress percent (0-100) from the linked task, when linked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// Epoch seconds when the transfer should finish. None: no live ETA
    /// source exists (v2 never sent one either).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<u64>,
    /// Total transfer bytes from the linked task, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// Bytes still to transfer from the linked task, once known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_remaining: Option<f64>,
    /// Task status detail groups behind the card's expander. None until a
    /// producer exists (v2 never sent these either).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_messages: Option<Vec<RequestStatusMessage>>,
    /// Last failure text from the linked task, when failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Picked candidate quality from the linked task, once picked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Fetch source from the linked task (`soulseek`, `usenet`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Primary owner id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Owner display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by_name: Option<String>,
    /// Reviewer display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_by_name: Option<String>,
    /// Epoch seconds when reviewed, when reviewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<u64>,
    /// Whether the album is in the library. None: intake has
    /// no library seam; callers treat unknown as present for imported
    /// rows, which only land after the task completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_library: Option<bool>,
    /// Linked download task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Whether an admin can reimport the linked task (failed with its
    /// candidate still linked). None when not applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_reimport: Option<bool>,
    /// `album` or `track`.
    pub request_kind: String,
    /// Track title for exact-track rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_title: Option<String>,
    /// Track length for exact-track rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    /// Containing release group for exact-track rows (album context).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_release_group_mbid: Option<String>,
    /// Co-requester count beyond the owner.
    pub requester_count: u32,
}

/// Active-requests list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ActiveRequestsResponse {
    /// Active rows.
    pub items: Vec<RequestItem>,
    /// Row count.
    pub count: u32,
}

/// Bare count shape for badges.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ActiveCountResponse {
    /// Row count.
    pub count: u32,
}

/// Paged history list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HistoryResponse {
    /// Page rows.
    pub items: Vec<RequestItem>,
    /// Total rows.
    pub total: u32,
    /// Current page.
    pub page: u32,
    /// Rows per page.
    pub page_size: u32,
    /// Total pages (at least 1).
    pub total_pages: u32,
}

/// Mutation outcome for approve/reject/cancel/retry/clear.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ActionResponse {
    /// Whether the action applied.
    pub success: bool,
    /// Human outcome.
    pub message: String,
}

/// Clear-history outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClearHistoryResponse {
    /// Whether a row was cleared.
    pub success: bool,
}

/// One wanted watch.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WantedItem {
    /// Release-group MBID.
    pub musicbrainz_id: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// `missing` (whole album) or `partial` (filling gaps).
    pub kind: String,
    /// Watch state: `watching`, `paused`, or loop-set `dormant`.
    pub state: String,
    /// Passed checks so far.
    pub check_count: u32,
    /// Epoch seconds when the next check is due, when scheduled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_check_at: Option<u64>,
    /// Unseen candidate count.
    pub new_candidate_count: u32,
    /// Epoch seconds when created.
    pub created_at: u64,
    /// Artist MBID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Release year.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// Custom artwork URL override. None: artwork resolves client-side
    /// from the MBID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// Owner id (admins see every row).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Owner display name (admins only; the "watched for" chip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
}

/// One auto-retry entry behind the wanted view.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WantedRetryingItem {
    /// Release-group MBID.
    pub musicbrainz_id: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Attempts so far.
    pub retry_count: u32,
    /// Attempts allowed.
    pub max_attempts: u32,
    /// Epoch seconds when the next try is due, when scheduled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<u64>,
    /// Artist MBID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Release year.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// Custom artwork URL override. None: artwork resolves client-side
    /// from the MBID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// Owner id (admins see every row).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Owner display name (admins only; the "requested by" chip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
}

/// Wanted list: watches plus the still-retrying set.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WantedResponse {
    /// Watches.
    pub items: Vec<WantedItem>,
    /// Watch count.
    pub count: u32,
    /// Still-scheduled auto-retries.
    pub retrying: Vec<WantedRetryingItem>,
}

/// Wanted mutation outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WantedActionResponse {
    /// Whether the action applied.
    pub success: bool,
    /// Watch state after this call.
    pub state: String,
}

/// One pending auto-download approval.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutoDownloadApprovalItem {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Pending auto-download approvals.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutoDownloadApprovalsResponse {
    /// Pending rows.
    pub items: Vec<AutoDownloadApprovalItem>,
    /// Row count.
    pub count: u32,
}

/// One pending bulk-approval batch.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalBatchItem {
    /// Batch id.
    pub batch_id: String,
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Artists covered.
    pub artist_count: u32,
    /// Sample artist names.
    pub sample_names: Vec<String>,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Pending bulk-approval batches.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApprovalBatchListResponse {
    /// Pending batches.
    pub batches: Vec<ApprovalBatchItem>,
    /// Batch count.
    pub count: u32,
}

/// One pending personal-mix auto-request approval.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PersonalMixApprovalItem {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Pending personal-mix approvals.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PersonalMixApprovalsResponse {
    /// Pending rows.
    pub items: Vec<PersonalMixApprovalItem>,
    /// Row count.
    pub count: u32,
}

/// Personal-mix refresh outcome. The schema renames to avoid discover's
/// unrelated `RefreshResponse` in the shared document.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = RequestsRefreshResponse)]
pub struct RefreshResponse {
    /// `started` or `already_running`.
    pub status: String,
}

/// Edition-acquire outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EditionAcquireResponse {
    /// `started`, `already_in_progress`, or `already_complete`.
    pub status: String,
    /// Human outcome.
    pub message: String,
    /// Linked download task, when dispatched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Status-sync outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncResponse {
    /// Rows reconciled.
    pub reconciled: u32,
}
