//! The discovery batch routes' bodies and replies.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One album to request inside a new batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchItemIn {
    /// MusicBrainz release-group id.
    pub musicbrainz_id: String,
    /// The artist's MusicBrainz id.
    #[serde(default)]
    pub artist_id: String,
    /// Album title.
    #[serde(default)]
    pub title: String,
    /// Artist name.
    #[serde(default)]
    pub artist_name: String,
}

/// Body creating a discovery batch (one request per album).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchCreate {
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    #[serde(default)]
    pub source_section: String,
    /// Albums to request.
    #[serde(default)]
    pub items: Vec<DiscoveryBatchItemIn>,
    /// A UUID naming this create. Sending the same key again returns the
    /// batch it made instead of filing the albums twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// What a batch did with one album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchOutcome {
    /// A request was filed.
    Requested,
    /// The library already held it.
    SkippedInLibrary,
    /// Someone had already requested it.
    SkippedDuplicate,
    /// The requests service did not file it.
    NotRequested,
}

impl BatchOutcome {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::SkippedInLibrary => "skipped_in_library",
            Self::SkippedDuplicate => "skipped_duplicate",
            Self::NotRequested => "not_requested",
        }
    }

    /// Reads a stored spelling back.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "skipped_in_library" => Some(Self::SkippedInLibrary),
            "skipped_duplicate" => Some(Self::SkippedDuplicate),
            "not_requested" => Some(Self::NotRequested),
            _ => None,
        }
    }
}

/// Per-album outcome inside a batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchItemStatus {
    /// MusicBrainz release-group id.
    pub musicbrainz_id: String,
    /// The artist's MusicBrainz id.
    #[serde(default)]
    pub artist_id: String,
    /// Album title.
    #[serde(default)]
    pub title: String,
    /// Artist name.
    #[serde(default)]
    pub artist_name: String,
    /// What the batch did with the album.
    pub outcome: BatchOutcome,
    /// Request state, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_status: Option<String>,
    /// True when already owned.
    #[serde(default)]
    pub in_library: bool,
}

/// A batch plus its album outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchDetail {
    /// Batch id.
    pub id: String,
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    #[serde(default)]
    pub source_section: String,
    /// Creation timestamp (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Album count.
    pub item_count: i64,
    /// Albums already imported.
    pub imported_count: i64,
    /// Albums still pending.
    pub pending_count: i64,
    /// Per-album outcomes.
    #[serde(default)]
    pub items: Vec<DiscoveryBatchItemStatus>,
}

/// A batch without its album outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchSummary {
    /// Batch id.
    pub id: String,
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    #[serde(default)]
    pub source_section: String,
    /// Creation timestamp (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Album count.
    pub item_count: i64,
    /// Albums already imported.
    pub imported_count: i64,
    /// Albums still pending.
    pub pending_count: i64,
}

/// The user's batches.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchListResponse {
    /// Batches, newest first.
    pub batches: Vec<DiscoveryBatchSummary>,
}

/// What removing a batch did.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchRemoveResult {
    /// Albums removed with the batch.
    #[serde(default)]
    pub removed_albums: i64,
    /// Requests cancelled with the batch.
    #[serde(default)]
    pub cancelled_requests: i64,
    /// Albums kept (already landed or owned elsewhere).
    #[serde(default)]
    pub kept: i64,
}
