//! Wire shapes of the follow and auto-download approval routes.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Follow-toggle body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FollowBody {
    /// True follows the artist; false unfollows (and drops auto-download).
    pub followed: bool,
    /// Artist name to keep, when following.
    pub artist_name: Option<String>,
}

/// Auto-download-toggle body.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutoDownloadBody {
    /// True requests auto-download; false turns it off.
    pub enabled: bool,
}

/// One pending auto-download request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutoDownloadApprovalItem {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting user display name.
    pub user_name: Option<String>,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// When it was requested, epoch seconds.
    pub requested_at: u64,
}

/// Pending-approval list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AutoDownloadApprovalListResponse {
    /// Pending requests.
    pub items: Vec<AutoDownloadApprovalItem>,
    /// Item count.
    pub count: usize,
}

/// One grouped approval card: a user's pending requests as a batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApprovalBatchItem {
    /// Batch id, stable per user while requests stay pending.
    pub batch_id: String,
    /// Requesting user id.
    pub user_id: String,
    /// Requesting user display name.
    pub user_name: Option<String>,
    /// Pending artist count.
    pub artist_count: usize,
    /// First few artist names for the card preview.
    pub sample_names: Vec<String>,
    /// Oldest request time, epoch seconds.
    pub requested_at: u64,
    /// What produced the requests.
    pub source: String,
}

/// Approval-batch list answer.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApprovalBatchListResponse {
    /// Batches.
    pub batches: Vec<ApprovalBatchItem>,
    /// Batch count.
    pub count: usize,
}
