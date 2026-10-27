//! Library HTTP models: request and response DTOs.
//!
//! An engine type that already is the wire shape goes on the wire as it
//! is; these views are for the shapes that differ.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use super::operations::ReasonView;
use crate::identify::models::{
    EvidenceClass, IdentifyKind, JobState as IdentifyJobState, ReviewState,
};
use crate::identity::DecisionSource;
use crate::publish::publisher::PublishOutcome;
use crate::scan::models::{
    Counters, Disposition, EffectivePolicy, FailureCode, RequestedControl, ScanKind, ScanPhase,
    ScanState, ScanTrigger, Verdict,
};

/// Which roots a scan run covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AggregateScope {
    /// Every scheduled root.
    All,
    /// Chosen roots or path rules.
    Selected,
}

impl AggregateScope {
    /// The stored label: `all`, and anything else as `selected`, as the
    /// activity feed reads it.
    pub fn from_stored(label: &str) -> Self {
        if label == "all" {
            Self::All
        } else {
            Self::Selected
        }
    }
}

/// One library root.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RootView {
    /// Stable root id.
    pub id: String,
    /// Absolute root directory.
    pub path: String,
    pub policy: EffectivePolicy,
}

/// Root registry listing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RootsResponse {
    /// Configured roots.
    pub roots: Vec<RootView>,
    /// Whether the local library is enabled.
    pub enabled: bool,
    /// Policy revision of the roots and rules, as the library settings
    /// show it. Send it back as `expected_policy_revision`.
    pub policy_revision: String,
    /// Managed-write bundles not yet settled: cleanup still pending, or
    /// recovery could not finish (their root is gone or excluded, a file
    /// operation failed). The background maintenance retries them; their
    /// tracks take no new managed writes until then.
    pub held_publish_bundles: Vec<String>,
}

/// Add a library root. The path must exist and be absolute; the id
/// defaults to a fresh uuid.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AddRootBody {
    /// Stable root id (default: fresh id).
    pub id: Option<String>,
    /// Absolute root directory.
    pub path: String,
    /// Effective policy (default: `automatic`).
    pub policy: Option<EffectivePolicy>,
}

/// Trigger a scan over one root or every scheduled root.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ScanBody {
    /// Root id to scan (default: every scheduled root).
    pub root_id: Option<String>,
}

/// Scan request answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanResponse {
    /// Run id (fresh or coalesced).
    pub run_id: String,
    pub disposition: Disposition,
    /// Run state after the request.
    pub state: ScanState,
}

/// One scan run.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunView {
    /// Run id.
    pub id: String,
    /// Run kind.
    pub kind: ScanKind,
    /// What triggered the run.
    pub trigger: ScanTrigger,
    /// Run state.
    pub state: ScanState,
    /// Current phase.
    pub phase: ScanPhase,
    pub aggregate_scope: AggregateScope,
    /// Progress counters.
    pub counters: Counters,
    /// Queue time (unix seconds).
    pub queued_at: f64,
    /// Start time, when started.
    pub started_at: Option<f64>,
    /// Last update time.
    pub updated_at: f64,
    /// Terminal time, when terminal.
    pub terminal_at: Option<f64>,
    /// Terminal code, when terminal.
    pub terminal_code: Option<FailureCode>,
    /// What the terminal code means, in plain words.
    pub terminal_message: Option<String>,
    /// What to do about the terminal code.
    pub terminal_action: Option<String>,
    /// Who asked for the run, when someone did.
    pub requested_by_user_id: Option<String>,
    /// Phase a paused run picks up from.
    pub resume_phase: Option<ScanPhase>,
    /// Pending control.
    pub requested_control: RequestedControl,
    /// Requests folded into this run.
    pub coalesced_request_count: i64,
    /// Row revision; pause, resume and stop send it back.
    pub row_revision: u64,
    /// Event revision.
    pub event_revision: u64,
    /// Seconds spent per phase.
    pub phase_timings: HashMap<String, f64>,
}

/// Current plus recent scan runs.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanRunsResponse {
    /// Live runs.
    pub current: Vec<ScanRunView>,
    /// Recent terminal runs.
    pub history: Vec<ScanRunView>,
}

/// One scope inside a run.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanScopeView {
    /// Root id.
    pub root_id: String,
    /// Path relative to the root.
    pub relative_path: String,
    /// Effective policy for the scope.
    pub effective_policy: EffectivePolicy,
}

/// One discovered file inside a run, with its scan-assigned track id.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScanFileView {
    /// Root id.
    pub root_id: String,
    /// Path relative to the root.
    pub relative_path: String,
    /// Scan-assigned stable track id, once indexed.
    pub track_id: Option<String>,
    /// Classify verdict.
    pub verdict: Verdict,
}

/// Run detail: the run, its scopes, and its discovered files.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunDetailResponse {
    /// The run.
    pub run: ScanRunView,
    /// Scopes in the run.
    pub scopes: Vec<ScanScopeView>,
    /// Discovered files (bounded to the latest 500).
    pub files: Vec<ScanFileView>,
}

/// Enqueue one album for identification.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct IdentifyBody {
    /// Catalog album id, as `GET /library/albums` lists it.
    pub album_id: String,
    /// Job kind (default: `manual`).
    pub kind: Option<IdentifyKind>,
}

/// Enqueue answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IdentifyResponse {
    /// Job id.
    pub job_id: String,
    /// Local album id.
    pub album_id: String,
    /// Job state after enqueue.
    pub state: IdentifyJobState,
}

/// One scored candidate inside a review.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CandidateView {
    /// Candidate key (approve by this).
    pub candidate_key: String,
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release MBID, when the candidate names an exact edition.
    pub release_mbid: Option<String>,
    /// Candidate album title.
    pub album_title: String,
    /// Candidate album artist.
    pub album_artist_name: String,
    /// Match score: one minus the distance.
    pub score: f64,
    /// Matcher distance, 0 for a perfect match. Albums identify on their
    /// own at 0.20 or less; reviews show candidates up to 0.35.
    pub distance: f64,
    /// What the distance is made of (`album`, `artist`, `tracks`,
    /// `missing_tracks`, `unmatched_tracks`, `album_id`, `year`,
    /// `media_count`), largest share first.
    pub penalties: Vec<PenaltyView>,
    /// Reason code.
    pub reason_code: String,
    /// Supported track count.
    pub supported_tracks: usize,
    /// Contradictory track count.
    pub contradictory_tracks: usize,
    /// How each of the album's files fits this release.
    pub track_evidence: Vec<TrackEvidenceView>,
}

/// How one file fits a candidate release.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TrackEvidenceView {
    pub local_track_id: String,
    /// The file's title as the library shows it.
    pub title: String,
    pub classification: EvidenceClass,
    /// What the verdict rests on (`recording_id`, `title`, `length`, ...).
    pub evidence_kinds: Vec<String>,
}

/// How many reviews one reason holds.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReasonCount {
    /// The reason code as filed (filter by this).
    pub reason_code: String,
    pub reason: ReasonView,
    pub count: u64,
}

/// One penalty's share of a candidate's distance.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PenaltyView {
    /// Penalty name.
    pub name: String,
    /// Its share of the distance.
    pub share: f64,
}

/// One identification review and the album it is about.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewView {
    /// Review id.
    pub id: String,
    /// Local album id.
    pub album_id: String,
    /// Album title, as the library shows it.
    pub album_title: String,
    /// Album artist, as the library shows it.
    pub artist_name: String,
    /// Reason code the review was filed with.
    pub reason_code: String,
    /// Why the review exists and what to do, in plain words.
    pub reason: ReasonView,
    /// Review state.
    pub state: ReviewState,
    /// Resolving user, once settled.
    pub resolved_by: Option<String>,
    /// Selected candidate key, once approved.
    pub selected_candidate_key: Option<String>,
    /// Scored candidates, best first.
    pub candidates: Vec<CandidateView>,
    /// When the review was filed (Unix milliseconds).
    pub created_ms: i64,
    /// When it last changed (Unix milliseconds).
    pub updated_ms: i64,
    /// The album's current match was picked by a person.
    pub manual_identity: bool,
}

/// One page of reviews.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewsResponse {
    /// Reviews on this page, most recently changed first.
    pub reviews: Vec<ReviewView>,
    /// Reviews matching the filter.
    pub total: u64,
    /// Reviews matching the filter apart from its reason, by reason.
    pub reason_counts: Vec<ReasonCount>,
    pub limit: u32,
    pub offset: u32,
}

/// What a "retry all" started.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewRetryAllResponse {
    /// True when nothing started: the counts are what a real run would do.
    pub preview: bool,
    /// Albums whose identification started again (or would).
    pub started: u32,
    /// Reviews that stayed open, grouped by why.
    pub skipped: Vec<SkippedReviews>,
}

/// What a bulk review change would do.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewBulkPreviewResponse {
    /// Reviews the selection matched.
    pub matched: u64,
    /// Reviews the change would apply to.
    pub eligible: u32,
    /// Reviews it would leave alone, grouped by why.
    pub skipped: Vec<SkippedReviews>,
    /// Pass back to apply exactly this change.
    pub preview_token: String,
}

/// What a bulk review change did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewBulkResponse {
    /// Reviews changed.
    pub applied: u32,
    /// Reviews left alone, grouped by why.
    pub skipped: Vec<SkippedReviews>,
}

/// Reviews one reason held back.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SkippedReviews {
    pub reason: ReasonView,
    pub count: u32,
}

/// Start identification again from a review.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ReviewRetryBody {
    /// Makes a retried request return the same job (single review only).
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Confirms a one-off MusicBrainz lookup for files under a Local
    /// metadata policy.
    #[serde(default)]
    pub confirm_local_metadata: bool,
}

/// Approve a review with the curator's chosen candidate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ApproveBody {
    /// Candidate key from the review.
    pub candidate_key: String,
}

/// Sealed identity after a review resolves.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IdentityView {
    /// Release MBID, when an exact edition is sealed.
    pub release_mbid: Option<String>,
    /// Release-group MBID, when sealed.
    pub release_group_mbid: Option<String>,
    /// Who decided.
    pub decision_source: DecisionSource,
}

/// Review resolution answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewResolveResponse {
    /// The settled review.
    pub review: ReviewView,
    /// Sealed identity (approvals only).
    pub identity: Option<IdentityView>,
}

/// One file inside a management preview.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManageItemBody {
    /// Source root id.
    pub root_id: String,
    /// Source path relative to the root.
    pub rel_path: String,
    /// Destination path relative to the root (organize only;
    /// retag writes in place).
    pub dest_rel: Option<String>,
    /// Managed-field updates by Picard tag-set name: `title`,
    /// `title_sort`, `artist`, `artists`, `artist_sort`, `album`,
    /// `album_sort`, `album_artist`, `album_artist_sort`, `genre`,
    /// `compilation` (`1` or `0`), `track_number`, `total_tracks`,
    /// `disc_number`, `total_discs`, `disc_subtitle`, `date`, `original_date`,
    /// `release_status`, `release_country`, `release_type`, `media`,
    /// `label`, `catalog_number`, `barcode`, `asin`, and the
    /// `musicbrainz_*` ids (`recording`, `release_track`, `release`,
    /// `release_group`, `artist`, `album_artist`). A retag also writes
    /// the full set from the album's identified release; these win.
    pub managed_updates: HashMap<String, Vec<String>>,
}

/// What a management preview does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManageKind {
    /// Write the tags in place.
    Retag,
    /// Move the files.
    Organize,
}

/// How one planned file is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManageFileKind {
    /// Written in place.
    SamePath,
    Move,
}

/// Build a sealed management preview. Identity always resolves from
/// the accepted identify rows for `album_id`; tracks without an
/// accepted exact mapping block the preview loudly.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManagePreviewBody {
    pub kind: ManageKind,
    /// Local album id carrying the accepted identity.
    pub album_id: String,
    /// Files in the bundle.
    pub items: Vec<ManageItemBody>,
}

/// One planned file inside a preview.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManageFileView {
    /// Stable local track id.
    pub track_id: String,
    /// Source `root/rel`.
    pub source: String,
    /// Destination `root/rel`.
    pub dest: String,
    pub kind: ManageFileKind,
    /// For a retag, the tags the write changes, in display order: album,
    /// album artist, title, artist, track, disc, date, label, catalog
    /// number, barcode, then `musicbrainz_ids`. Unchanged tags are left
    /// out. Empty for a move.
    pub changes: Vec<ManageTagChange>,
}

/// One tag a retag would change.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManageTagChange {
    /// Tag field name (`album`, `track_number`, ...), or `musicbrainz_ids`
    /// for every MusicBrainz id field together.
    pub field: String,
    /// The file's value now, several values joined with "; "; null when
    /// absent. Always null for `musicbrainz_ids`.
    pub from: Option<String>,
    /// The value after the write; null when the write removes the tag.
    /// For `musicbrainz_ids`, how many id fields change and stay, such as
    /// "4 changed, 2 kept".
    pub to: Option<String>,
}

/// Sealed preview answer. The token is single-use and short-lived.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManagePreviewResponse {
    /// Single-use confirmation token.
    pub preview_token: String,
    /// Expiry day (unix day, exclusive).
    pub expires_day: i64,
    /// Bundle id the apply will publish under.
    pub bundle_id: String,
    /// Planned files.
    pub files: Vec<ManageFileView>,
}

/// Apply a sealed preview.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManageApplyBody {
    /// Preview token from the preview answer.
    pub preview_token: String,
}

/// One applied file.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManageAppliedFile {
    /// Stable local track id.
    pub track_id: String,
    /// Adopted root id.
    pub root_id: String,
    /// Adopted path relative to the root.
    pub rel_path: String,
}

/// Apply answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManageApplyResponse {
    /// Published bundle id.
    pub bundle_id: String,
    /// Whether cleanup finished.
    pub outcome: PublishOutcome,
    /// Applied files.
    pub files: Vec<ManageAppliedFile>,
}

/// Undo one published bundle as a new previewed operation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ManageUndoBody {
    /// Source operation bundle id.
    pub bundle_id: String,
}

/// Undo answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ManageUndoResponse {
    /// Undo operation bundle id.
    pub bundle_id: String,
    /// Whether cleanup finished.
    pub outcome: PublishOutcome,
    /// Restored track ids.
    pub restored: Vec<String>,
}

/// Which plugin's files to list.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PluginFilesQuery {
    /// Plugin name.
    pub plugin: String,
    /// Cursor from the previous page.
    pub cursor: Option<String>,
    /// Files per page, 1 to 100 (default 50).
    pub limit: Option<u32>,
}

/// Whether a plugin's file is still in the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginFileState {
    /// Added and not taken back.
    Added,
    /// Taken back by an undo.
    TakenBack,
}

/// One file a plugin added to the library.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PluginFileView {
    /// The change that added it; undo it to take the file back.
    pub bundle_id: String,
    /// Library folder id.
    pub root_id: String,
    /// Path under the library folder.
    pub relative_path: String,
    /// Size in bytes.
    pub size: u64,
    /// When it was added, in unix milliseconds.
    pub added_at: i64,
    pub state: PluginFileState,
}

/// A page of a plugin's files, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PluginFilesPage {
    pub items: Vec<PluginFileView>,
    /// Cursor for the next page, when there is one.
    pub next_cursor: Option<String>,
}

/// Restore tracks to their immutable first-management baselines.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BaselineRestoreBody {
    /// Track ids to restore.
    pub track_ids: Vec<String>,
}

/// Baseline restore answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BaselineRestoreResponse {
    /// Restore operation bundle id.
    pub bundle_id: String,
    /// Whether cleanup finished.
    pub outcome: PublishOutcome,
    /// Restored track ids.
    pub restored: Vec<String>,
}
