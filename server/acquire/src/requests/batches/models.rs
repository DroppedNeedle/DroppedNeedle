//! The discovery batch routes' bodies and replies.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use droppedneedle_catalog::discover::record::MadeFrom;

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
    /// Where the batch was made, when the caller says. Kept on the batch
    /// and on the asker's own row of each album it asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_from: Option<MadeFrom>,
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

/// Where a request batch came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchSource {
    /// Discover's "Download all".
    Discover,
    /// A playlist's Request missing.
    Playlist,
}

droppedneedle_kernel::spelled!(BatchSource {
    Discover => "discover",
    Playlist => "playlist",
});

droppedneedle_kernel::spelled!(BatchOutcome {
    Requested => "requested",
    SkippedInLibrary => "skipped_in_library",
    SkippedDuplicate => "skipped_duplicate",
    NotRequested => "not_requested",
});

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

/// One album waiting for approval on a card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ApprovalCardItem {
    /// MusicBrainz release-group id.
    pub musicbrainz_id: String,
    /// Album title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// The album's cover route.
    pub cover_url: String,
    /// How many of the playlist's songs it carries.
    pub song_count: Option<u32>,
}

/// One press's albums waiting for approval, decided together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ApprovalCard {
    /// The batch id.
    pub id: String,
    /// Where the press came from.
    pub source: BatchSource,
    /// The playlist's name when it was pressed.
    pub name: String,
    /// Who pressed it.
    pub requester_name: String,
    /// When (RFC 3339).
    pub created_at: String,
    /// Albums still waiting.
    pub waiting: u32,
    /// Every waiting album, in the press's order.
    pub items: Vec<ApprovalCardItem>,
}

/// The approval cards, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ApprovalCardListResponse {
    /// Cards with an album still waiting.
    pub cards: Vec<ApprovalCard>,
}

/// One decision on a card: the albums to approve. Every other album
/// still waiting on it is rejected in the same write.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CardDecision {
    /// Release-group ids to approve; empty rejects them all.
    #[serde(default)]
    pub approve: Vec<String>,
}

/// What a decision on a card did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CardDecisionResult {
    /// Albums approved.
    pub approved: u32,
    /// Albums rejected.
    pub rejected: u32,
    /// Ids that named no album still waiting on the card.
    pub skipped: u32,
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
    /// Albums left in the batch because no recycle bin is known yet.
    #[serde(default)]
    pub waiting: i64,
    /// Why those albums wait, when any do.
    #[serde(default)]
    pub waiting_reason: Option<String>,
}
