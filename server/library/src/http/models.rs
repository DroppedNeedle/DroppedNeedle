//! Library HTTP models: request and response DTOs.
//!
//! An engine type that already is the wire shape goes on the wire as it
//! is; these views are for the shapes that differ.

use std::collections::HashMap;

use droppedneedle_kernel::page::Page;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use super::operations::ReasonView;
use crate::identify::models::{EvidenceClass, ReviewState};
use crate::identity::DecisionSource;
use crate::publish::publisher::PublishOutcome;
use crate::scan::models::{
    Counters, EffectivePolicy, FailureCode, RequestedControl, ScanKind, ScanPhase, ScanState,
    ScanTrigger, Verdict,
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
    /// The MusicBrainz recording the file matched, when it matched one.
    pub recording_mbid: Option<String>,
    /// The release's track it matched, when it matched one.
    pub release_track_mbid: Option<String>,
}

/// A release every selected review lists.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SharedCandidateView {
    /// Accept the selection with this key.
    pub candidate_key: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub album_title: String,
    pub album_artist_name: String,
    /// Its weakest match score across the selected reviews.
    pub lowest_score: f64,
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
    /// Goes up whenever the review's state, candidates, reason or album
    /// changes. Every action on the review sends the one it was decided
    /// on.
    pub row_revision: i64,
}

/// Reviews matching a filter apart from its state, by state.
#[derive(Debug, Clone, Copy, Default, Serialize, ToSchema)]
pub struct ReviewStateCounts {
    pub pending: u64,
    pub approved: u64,
    pub rejected: u64,
    pub dismissed: u64,
    pub excluded: u64,
}

/// One page of reviews, with the reviews matching the filter as its
/// total.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReviewsResponse {
    #[serde(flatten)]
    pub page: Page<ReviewView>,
    /// Reviews matching the filter apart from its reason, by reason.
    pub reason_counts: Vec<ReasonCount>,
    /// Reviews matching the filter apart from its state, by state.
    pub state_counts: ReviewStateCounts,
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
    /// Releases every selected pending, unpinned review lists, best
    /// weakest score first; accept one for them all with its
    /// `candidate_key`. Empty when they share none.
    pub shared_candidates: Vec<SharedCandidateView>,
}

/// Reviews one reason held back.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SkippedReviews {
    pub reason: ReasonView,
    pub count: u32,
}

/// An action on one review, decided on the revision the caller read.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReviewActionBody {
    /// The review's `row_revision` when it was read. A review that changed
    /// since is refused with `STALE_REVISION`.
    pub expected_revision: i64,
}

/// Start identification again from a review.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReviewRetryBody {
    /// The review's `row_revision` when it was read.
    pub expected_revision: i64,
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
    /// The review's `row_revision` when it was read.
    pub expected_revision: i64,
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
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
    /// Files per page, 1 to 100 (default 50); more is read as 100.
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
