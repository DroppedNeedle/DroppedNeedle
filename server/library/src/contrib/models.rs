//! Contribution value types: states, drafts, snapshots, seeds, and the pure
//! rules that govern them.
//!
//! The library contribution models plus the pure
//! helpers on `LibraryContributionService`. Every intentional quirk keeps a
//! `Quirk (v2 ...)` citation so a later reader can tell intended behavior
//! from accident.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Longest trimmed free-text value accepted in a draft.
pub const MAX_TEXT_LENGTH: usize = 1_000;
/// How long a selected Discogs snapshot stays displayable (6h).
pub const DISCOGS_DISPLAY_SECONDS: f64 = 6.0 * 60.0 * 60.0;
/// MusicBrainz release-editor seed target. The curator's browser POSTs here;
/// this codebase never POSTs to a provider itself.
pub const MUSICBRAINZ_RELEASE_EDITOR: &str = "https://musicbrainz.org/release/add";
/// MB link type for a Discogs release URL attached to a seeded release.
pub const MUSICBRAINZ_DISCOGS_RELEASE_LINK_TYPE: &str = "76";
/// Callback path the seed's `redirect_uri` points at, relative to the
/// public base URL.
pub const CALLBACK_PATH: &str = "/api/v3/library/contributions/musicbrainz/callback";
/// Callback token lifetime (30m).
pub const CALLBACK_TOKEN_SECONDS: f64 = 30.0 * 60.0;
/// Deterministic provider-payload failure: review immediately, breaker stays
/// closed, no provider resurrection, manual retry after an app update.
pub const UNMAPPABLE_PROVIDER_PAYLOAD: &str = "UNMAPPABLE_PROVIDER_PAYLOAD";

/// Failure codes the verification worker persists on jobs/attempts.
pub const FAILURE_MB_UNAVAILABLE: &str = "MUSICBRAINZ_TEMPORARILY_UNAVAILABLE";
pub const FAILURE_MB_NOT_PROPAGATED: &str = "MUSICBRAINZ_RELEASE_NOT_PROPAGATED";
pub const FAILURE_RETURNED_RELEASE_MISMATCH: &str = "RETURNED_RELEASE_MISMATCH";

/// v2 edit-note footer baked into every seed (Quirk: exact text, incl. URL).
pub fn seed_edit_note(discogs_url: Option<&str>) -> String {
    let mut note = String::from(
        "Seeded with DroppedNeedle (https://github.com/DroppedNeedle/DroppedNeedle).\nSources:\n* Local audio-file metadata",
    );
    if let Some(url) = discogs_url {
        note.push_str("\n* Discogs release: ");
        note.push_str(url);
    }
    note
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// Contribution lifecycle states. The strings match v2's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContributionState {
    Draft,
    Ready,
    Seeded,
    Verifying,
    Linked,
    NeedsReview,
    Stale,
    Cancelled,
}

droppedneedle_kernel::spelled!(ContributionState {
    Draft => "draft",
    Ready => "ready",
    Seeded => "seeded",
    Verifying => "verifying",
    Linked => "linked",
    NeedsReview => "needs_review",
    Stale => "stale",
    Cancelled => "cancelled",
});

/// Follow-up actions the UI may offer. The strings match v2's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContributionNextAction {
    EditDraft,
    RefreshDiscogs,
    RunDuplicateCheck,
    AttachExisting,
    SeedMusicbrainz,
    RetryVerification,
    Rebuild,
    Cancel,
}

/// Where a draft field value came from. The strings match v2's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContributionFieldSource {
    #[default]
    Local,
    Discogs,
    EnteredHere,
}

/// Duplicate evidence kinds, ordered weakest-last:
/// exact_discogs_url < release_group < barcode < similar.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateEvidenceKind {
    ExactDiscogsUrl,
    ReleaseGroup,
    Barcode,
    Similar,
}

/// Track alignment classes from Discogs selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentClassification {
    Exact,
    Partial,
    Conflicting,
    #[default]
    Unmatched,
}

// ---------------------------------------------------------------------------
// Draft + snapshot documents
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseTextField {
    #[serde(default)]
    #[schema(required = true)]
    pub value: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub source: ContributionFieldSource,
}

impl ReleaseTextField {
    pub fn local(value: Option<String>) -> Self {
        Self {
            value,
            source: ContributionFieldSource::Local,
        }
    }

    pub fn text(&self) -> &str {
        self.value.as_deref().unwrap_or("")
    }
}

impl Default for ReleaseTextField {
    fn default() -> Self {
        Self {
            value: None,
            source: ContributionFieldSource::Local,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseTrackSnapshot {
    pub local_track_id: String,
    pub disc_number: i64,
    pub track_number: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub title: String,
    #[serde(default)]
    #[schema(required = true)]
    pub artist_name: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    #[schema(required = true)]
    pub duration_reliable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseMediumSnapshot {
    pub position: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub title: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub tracks: Vec<ReleaseTrackSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LocalReleaseSnapshot {
    #[serde(default = "schema_one")]
    #[schema(required = true)]
    pub schema_version: i32,
    #[serde(default)]
    #[schema(required = true)]
    pub local_album_id: String,
    #[serde(default)]
    #[schema(required = true)]
    pub local_artist_id: String,
    #[serde(default = "revision_one")]
    #[schema(required = true)]
    pub album_row_revision: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub input_revision: String,
    #[serde(default)]
    #[schema(required = true)]
    pub title: String,
    #[serde(default)]
    #[schema(required = true)]
    pub album_artist_name: String,
    #[serde(default = "unknown_kind")]
    #[schema(required = true)]
    pub artist_kind: String,
    #[serde(default)]
    #[schema(required = true)]
    pub musicbrainz_artist_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub musicbrainz_release_group_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub musicbrainz_release_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub release_date: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub year: Option<i32>,
    #[serde(default)]
    #[schema(required = true)]
    pub is_compilation: bool,
    #[serde(default)]
    #[schema(required = true)]
    pub captured_at: f64,
    #[serde(default)]
    #[schema(required = true)]
    pub media: Vec<ReleaseMediumSnapshot>,
}

fn schema_one() -> i32 {
    1
}
fn revision_one() -> i64 {
    1
}
fn unknown_kind() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseTrackDraft {
    pub local_track_id: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub title: ReleaseTextField,
    pub artist_name: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseMediumDraft {
    pub position: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub title: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub format: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub tracks: Vec<ReleaseTrackDraft>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseDraft {
    #[serde(default = "schema_one")]
    #[schema(required = true)]
    pub schema_version: i32,
    #[serde(default)]
    #[schema(required = true)]
    pub title: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub artist_credit: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub release_date: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub country: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub label: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub catalogue_number: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub barcode: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub packaging: ReleaseTextField,
    #[serde(default)]
    #[schema(required = true)]
    pub media: Vec<ReleaseMediumDraft>,
}

impl Default for ReleaseDraft {
    fn default() -> Self {
        Self {
            schema_version: 1,
            title: ReleaseTextField::default(),
            artist_credit: ReleaseTextField::default(),
            release_date: ReleaseTextField::default(),
            country: ReleaseTextField::default(),
            label: ReleaseTextField::default(),
            catalogue_number: ReleaseTextField::default(),
            barcode: ReleaseTextField::default(),
            packaging: ReleaseTextField::default(),
            media: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SourceReference {
    pub provider: String,
    pub entity_type: String,
    pub external_id: String,
    pub canonical_url: String,
    #[serde(default)]
    #[schema(required = true)]
    pub fetched_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TrackAlignment {
    pub local_track_id: String,
    #[serde(default)]
    #[schema(required = true)]
    pub provider_position: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub classification: AlignmentClassification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ContributionSourceSelection {
    #[serde(default = "schema_one")]
    #[schema(required = true)]
    pub schema_version: i32,
    #[serde(default)]
    #[schema(required = true)]
    pub sources: Vec<SourceReference>,
    #[serde(default)]
    #[schema(required = true)]
    pub alignments: Vec<TrackAlignment>,
}

impl Default for ContributionSourceSelection {
    fn default() -> Self {
        Self {
            schema_version: 1,
            sources: Vec::new(),
            alignments: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Discogs view (trimmed to what selection/validation/seeding consume)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsReleaseCandidate {
    pub release_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist_name: String,
    #[serde(default)]
    pub canonical_url: String,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub catalogue_number: Option<String>,
    #[serde(default)]
    pub format_summary: Option<String>,
    #[serde(default)]
    pub track_count: Option<i64>,
    #[serde(default)]
    pub master_id: Option<String>,
    #[serde(default)]
    pub fetched_at: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsArtistCredit {
    #[serde(default)]
    #[schema(required = true)]
    pub name: String,
    #[serde(default)]
    #[schema(required = true)]
    pub credited_name: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub join_phrase: String,
    #[serde(default)]
    #[schema(required = true)]
    pub artist_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub canonical_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsLabel {
    #[serde(default)]
    #[schema(required = true)]
    pub name: String,
    #[serde(default)]
    #[schema(required = true)]
    pub catalogue_number: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub label_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub canonical_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsIdentifier {
    #[serde(default, rename = "type")]
    #[schema(required = true)]
    pub kind: String,
    #[serde(default)]
    #[schema(required = true)]
    pub value: String,
    #[serde(default)]
    #[schema(required = true)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsFormat {
    #[serde(default)]
    #[schema(required = true)]
    pub name: String,
    #[serde(default)]
    #[schema(required = true)]
    pub quantity: Option<i64>,
    #[serde(default)]
    #[schema(required = true)]
    pub descriptions: Vec<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsTrack {
    #[serde(default)]
    #[schema(required = true)]
    pub source_position: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub number: Option<i64>,
    #[serde(default)]
    #[schema(required = true)]
    pub title: String,
    #[serde(default)]
    #[schema(required = true)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    #[schema(required = true)]
    pub heading: bool,
    #[serde(default)]
    #[schema(required = true)]
    pub artists: Vec<DiscogsArtistCredit>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsMedium {
    #[schema(required = true)]
    pub position: i64,
    #[serde(default)]
    #[schema(required = true)]
    pub title: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub format: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub tracks: Vec<DiscogsTrack>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct DiscogsRelease {
    #[serde(default)]
    #[schema(required = true)]
    pub release_id: String,
    #[serde(default)]
    #[schema(required = true)]
    pub master_id: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub canonical_release_url: String,
    #[serde(default)]
    #[schema(required = true)]
    pub canonical_master_url: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub title: String,
    #[serde(default)]
    #[schema(required = true)]
    pub artist_name: String,
    #[serde(default)]
    #[schema(required = true)]
    pub artists: Vec<DiscogsArtistCredit>,
    #[serde(default)]
    #[schema(required = true)]
    pub released_date: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub year: Option<i32>,
    #[serde(default)]
    #[schema(required = true)]
    pub country: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub labels: Vec<DiscogsLabel>,
    #[serde(default)]
    #[schema(required = true)]
    pub identifiers: Vec<DiscogsIdentifier>,
    #[serde(default)]
    #[schema(required = true)]
    pub barcode: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub formats: Vec<DiscogsFormat>,
    #[serde(default)]
    #[schema(required = true)]
    pub media: Vec<DiscogsMedium>,
    #[serde(default)]
    #[schema(required = true)]
    pub source_fetched_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscogsSourceView {
    #[serde(default)]
    #[schema(required = true)]
    pub release: Option<DiscogsRelease>,
    #[serde(default)]
    #[schema(required = true)]
    pub expired: bool,
    #[serde(default)]
    #[schema(required = true)]
    pub expires_at: Option<f64>,
}

// ---------------------------------------------------------------------------
// Duplicate check + MusicBrainz verification payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DuplicateCandidate {
    #[serde(default)]
    #[schema(required = true)]
    pub release_mbid: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub release_group_mbid: Option<String>,
    #[serde(default)]
    #[schema(required = true)]
    pub title: String,
    #[serde(default)]
    #[schema(required = true)]
    pub artist_name: String,
    pub evidence_kind: DuplicateEvidenceKind,
    #[serde(default)]
    #[schema(required = true)]
    pub exact: bool,
    #[serde(default)]
    #[schema(required = true)]
    pub differences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DuplicateCheckResult {
    #[serde(default = "schema_one")]
    #[schema(required = true)]
    pub schema_version: i32,
    #[serde(default)]
    #[schema(required = true)]
    pub checked_at: f64,
    #[serde(default)]
    #[schema(required = true)]
    pub input_revision: String,
    #[serde(default)]
    #[schema(required = true)]
    pub candidates: Vec<DuplicateCandidate>,
    #[serde(default)]
    #[schema(required = true)]
    pub different_edition_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzUrlResolution {
    #[serde(default)]
    pub resource_url: String,
    #[serde(default)]
    pub release_mbids: Vec<String>,
    #[serde(default)]
    pub release_group_mbids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzVerifiedTrack {
    #[serde(default)]
    pub title: String,
    pub position: i64,
    pub disc_number: i64,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub recording_mbid: Option<String>,
    #[serde(default)]
    pub release_track_mbid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzVerifiedRelease {
    #[serde(default)]
    pub release_mbid: String,
    #[serde(default)]
    pub release_group_mbid: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist_name: String,
    #[serde(default)]
    pub artist_mbid: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub packaging: Option<String>,
    #[serde(default)]
    pub barcode: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub catalogue_number: Option<String>,
    #[serde(default)]
    pub tracks: Vec<MusicBrainzVerifiedTrack>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzSeedField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzSeed {
    pub action_url: String,
    pub method: String,
    pub fields: Vec<MusicBrainzSeedField>,
    pub contribution_revision: i64,
    pub expires_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ContributionValidationIssue {
    pub code: String,
    pub field: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

/// What the service hands back: persisted row plus derived presentation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ContributionRecord {
    pub id: String,
    pub local_album_id: String,
    #[schema(required = true)]
    pub created_by_user_id: Option<String>,
    #[schema(required = true)]
    pub updated_by_user_id: Option<String>,
    pub state: ContributionState,
    pub album_row_revision: i64,
    pub input_revision: String,
    pub local_snapshot: LocalReleaseSnapshot,
    pub draft: ReleaseDraft,
    pub source_selection: ContributionSourceSelection,
    #[schema(required = true)]
    pub provider_snapshot_expires_at: Option<f64>,
    #[schema(required = true)]
    pub discogs_source: Option<DiscogsSourceView>,
    #[schema(required = true)]
    pub duplicate_result: Option<DuplicateCheckResult>,
    #[schema(required = true)]
    pub duplicate_checked_at: Option<f64>,
    #[schema(required = true)]
    pub result_release_mbid: Option<String>,
    #[schema(required = true)]
    pub result_source: Option<String>,
    #[schema(required = true)]
    pub result_received_at: Option<f64>,
    #[schema(required = true)]
    pub seeded_at: Option<f64>,
    #[schema(required = true)]
    pub terminal_at: Option<f64>,
    pub created_at: f64,
    pub updated_at: f64,
    pub row_revision: i64,
    pub input_is_current: bool,
    pub validation: Vec<ContributionValidationIssue>,
    pub next_actions: Vec<ContributionNextAction>,
    /// Why the contribution needs review, when it does: a stable code, a
    /// plain sentence, and what to do next.
    #[schema(required = true)]
    pub review_reason: Option<ContributionReason>,
}

/// One reason shown to the curator: stable code, plain sentence, action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ContributionReason {
    pub code: String,
    pub message: String,
    pub action: String,
}

/// How a verification job finished (worker outcomes, the strings match v2's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationOutcome {
    Linked,
    NeedsReview,
    /// Local input moved mid-verification; the contribution went stale and
    /// the job was cancelled with LOCAL_INPUT_CHANGED.
    Stale,
    RetryScheduled,
    NoLongerVerifying,
    SubjectMissing,
}

impl VerificationOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::NeedsReview => "needs_review",
            Self::Stale => "stale",
            Self::RetryScheduled => "retry_scheduled",
            Self::NoLongerVerifying => "no_longer_verifying",
            Self::SubjectMissing => "subject_missing",
        }
    }
}
