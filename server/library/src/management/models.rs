//! Management job types: what a preview selects, what it plans for each
//! file, and what a job looks like to the pages that watch it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::reasons::ItemReason;
use crate::jobs::{ManagementPhase, WorkState};
use crate::operations::models::OperationJob;
use crate::publish::planner::PlanItem;
use droppedneedle_kernel::runtime_config::sections::{
    LibraryManagementProfile, LibraryManagementRootOverrides, NamingScript, TaggingScript,
};

/// Most ids one selection may name.
pub const MAX_SELECTION_IDS: usize = 5_000;

/// What a preview covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ManagementSelectionKind)]
pub enum SelectionKind {
    Roots,
    Artists,
    Albums,
    Tracks,
    Filter,
}

/// Catalog filter for a `filter` selection; empty matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ManagementCatalogFilterBody)]
pub struct CatalogFilter {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub from_year: Option<i64>,
    #[serde(default)]
    pub to_year: Option<i64>,
    #[serde(default)]
    pub artist_ids: Vec<String>,
    #[serde(default)]
    pub album_artist_only: bool,
}

/// A preview's scope: library folders, artists, albums, files, or a
/// catalog filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ManagementSelectionBody)]
pub struct Selection {
    pub kind: SelectionKind,
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub catalog_filter: Option<CatalogFilter>,
}

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobMode {
    /// Plan from a profile; applying turns it into `apply`.
    Preview,
    Apply,
    /// A preview made by automatic management, applied without asking.
    AutomaticApply,
    /// Put an earlier apply back.
    Undo,
    /// Put files back the way they were before they were first managed.
    BaselineRestore,
    /// Settle a file a preview could not place because another file
    /// already sits at its destination.
    DuplicateResolution,
}

impl JobMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Apply => "apply",
            Self::AutomaticApply => "automatic_apply",
            Self::Undo => "undo",
            Self::BaselineRestore => "baseline_restore",
            Self::DuplicateResolution => "duplicate_resolution",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "preview" => Some(Self::Preview),
            "apply" => Some(Self::Apply),
            "automatic_apply" => Some(Self::AutomaticApply),
            "undo" => Some(Self::Undo),
            "baseline_restore" => Some(Self::BaselineRestore),
            "duplicate_resolution" => Some(Self::DuplicateResolution),
            _ => None,
        }
    }

    /// Jobs that put files back rather than plan them from a profile.
    /// Their apply skips the profile and match checks: the publish seal
    /// already pins every file.
    pub fn restores(self) -> bool {
        matches!(self, Self::Undo | Self::BaselineRestore)
    }
}

/// Who asked for a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Manual,
    /// Automatic management after a download was imported.
    Acquisition,
    /// Automatic management after a dropped folder was imported.
    DropImport,
    /// Automatic management after a scan found and identified an album.
    ScanDiscovered,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Acquisition => "acquisition",
            Self::DropImport => "drop_import",
            Self::ScanDiscovered => "scan_discovered",
        }
    }
}

/// How a tag edit is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ManagementTagEditMode)]
pub enum TagEditMode {
    /// Write the value and keep it as an override, so later runs keep it.
    SaveOverride,
    /// Write the value once; a later run may change it again.
    WriteOnce,
    /// Drop the saved override and write the value the edition gives.
    ResetCanonical,
}

/// One edited field, with the override it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagEditField {
    pub field_name: String,
    /// `album` or `track`: album fields apply to every file of the album.
    pub subject_kind: String,
    /// The new values; empty clears the field.
    pub values: Vec<String>,
    pub override_id: Option<String>,
    pub expected_override_row_revision: Option<i64>,
    /// The kept value this edit replaces, as (value JSON, mode), so its
    /// undo can put it back.
    #[serde(default)]
    pub previous: Option<(String, String)>,
}

/// A manual tag edit a preview carries to apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagEditIntent {
    pub local_track_id: String,
    pub local_album_id: String,
    pub mode: TagEditMode,
    pub fields: Vec<TagEditField>,
}

/// What to do with a file whose destination is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ManagementDuplicateAction)]
pub enum DuplicateAction {
    /// Leave both files where they are.
    KeepExisting,
    /// Put the incoming file at another path next to the existing one.
    KeepIncomingAlternate,
    /// Move the existing file to the recycle bin and put the incoming one
    /// in its place.
    RecycleExistingKeepIncoming,
    /// Move the incoming file to the recycle bin and keep the existing one.
    RecycleIncomingKeepExisting,
}

/// A duplicate resolution: the blocked file and what to do with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateIntent {
    pub source_job_id: String,
    pub source_plan_item_ordinal: i64,
    pub collision_kind: String,
    pub existing_root_id: String,
    pub existing_relative_path: String,
    pub existing_local_track_id: Option<String>,
    pub action: DuplicateAction,
    pub alternate_relative_path: Option<String>,
}

/// What a job carries besides its selection and profile, stored as the
/// snapshot's `intent_json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobIntent {
    /// Hash of the manual overrides the preview was asked with.
    pub overrides_revision: String,
    pub tag_edit: Option<TagEditIntent>,
    /// For an activation dry run: the library folder it proves.
    pub activation_root_id: Option<String>,
    pub duplicate: Option<DuplicateIntent>,
    /// For an edition's tag write: the edition the person saw.
    pub edition: Option<EditionPin>,
}

/// The edition a tag write was asked for, with the identity revision the
/// person saw. Planning writes nothing to an album whose identity moved
/// on, since a re-made identity starts its revision again at 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditionPin {
    pub local_album_id: String,
    pub release_mbid: String,
    pub identity_revision: i64,
}

/// Whether a planned file will be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    Eligible,
    /// Will be written, with something worth knowing (a field the format
    /// cannot hold, an unsupported option that is skipped).
    Warning,
    Blocked,
    Stale,
}

impl Eligibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Warning => "warning",
            Self::Blocked => "blocked",
            Self::Stale => "stale",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "eligible" => Self::Eligible,
            "warning" => Self::Warning,
            "stale" => Self::Stale,
            _ => Self::Blocked,
        }
    }

    pub fn writes(self) -> bool {
        matches!(self, Self::Eligible | Self::Warning)
    }
}

/// The profile a job runs with, frozen when the preview is made.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedProfile {
    pub profile: LibraryManagementProfile,
    pub naming_script: NamingScript,
    pub multi_disc_naming_script: Option<NamingScript>,
    pub tagging_scripts: Vec<TaggingScript>,
    /// The manual overrides the preview was asked with.
    #[serde(default)]
    pub overrides: Option<LibraryManagementRootOverrides>,
}

/// Revisions a preview is pinned to; apply refuses when any moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revisions {
    pub profile: String,
    pub settings: String,
    pub naming: String,
    pub policy: String,
    /// Hash of the manual overrides, so a changed override never applies.
    pub overrides: String,
}

/// One tag field change. Values are JSON so v2 history reads as stored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldMutation {
    pub name: String,
    /// `set` or `clear`.
    pub operation: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
}

/// One sidecar travelling with its album.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidecarMove {
    pub source_relative_path: String,
    pub destination_relative_path: String,
}

/// What a file's plan changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemDiff {
    pub field_mutations: Vec<FieldMutation>,
    pub tags_changed: bool,
    pub path_changed: bool,
    pub sidecars: Vec<SidecarMove>,
    /// Files written next to the audio from no source file.
    pub generated: Vec<GeneratedFile>,
    /// The embedded pictures change.
    pub artwork_changed: bool,
    /// Native tags outside the managed fields a scrub removes.
    pub scrubbed_raw_tags: Vec<ScrubbedTag>,
    /// Genre sources that could not be asked.
    pub deferred_sources: Vec<String>,
}

/// One native tag a scrub removes, with what it held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrubbedTag {
    pub key: String,
    /// `text`, or `binary` for an ID3 comment, link, private or object
    /// frame (its values then give each frame's size).
    pub value_kind: String,
    pub values: Vec<String>,
    pub value_count: usize,
    pub truncated: bool,
}

/// A file the plan writes from no source (a cover, a lyrics file).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneratedFile {
    /// `artwork` or `lyrics`.
    pub kind: String,
    pub destination_relative_path: String,
}

impl ItemDiff {
    pub fn changes_anything(&self) -> bool {
        self.tags_changed
            || self.path_changed
            || !self.sidecars.is_empty()
            || !self.generated.is_empty()
            || self.artwork_changed
    }
}

/// Facts about a file the pages show next to its plan, and its reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ItemFacts {
    pub audio_format: String,
    pub warnings: Vec<String>,
    pub blockers: Vec<String>,
    pub catalog_track_title: Option<String>,
    pub catalog_artist_name: Option<String>,
    pub catalog_album_title: Option<String>,
    pub catalog_track_number: Option<i64>,
    pub catalog_disc_number: Option<i64>,
    pub reason: Option<ItemReason>,
}

/// One desired field value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesiredField {
    pub name: String,
    pub value: serde_json::Value,
}

/// The tags a file should end up with, plus the publish plan apply runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DesiredDocument {
    pub fields: Vec<DesiredField>,
    /// What apply publishes; `None` when the file is blocked or has
    /// nothing to change.
    pub plan: Option<PlanItem>,
    /// A file to move to the recycle bin just before this one is
    /// published (a duplicate resolution that keeps the new file).
    pub recycle_first: Option<RecycleFirst>,
    /// The cover art the plan embeds or writes, as the pages show it.
    pub artwork: Vec<ArtworkChoice>,
}

/// One piece of cover art a plan writes (v2's artwork choice).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtworkChoice {
    /// `embedded_art` or `external_art`.
    pub output_kind: String,
    /// `front`.
    pub image_type: String,
    /// Where it came from (`cover_art_archive`).
    pub source: String,
    pub mime_type: String,
    /// The bytes, in the blob store.
    pub blob_sha256: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    /// For a cover file: where it goes.
    pub destination_relative_path: Option<String>,
}

/// The file in the way of a duplicate resolution, as the preview saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecycleFirst {
    pub root_id: String,
    pub relative_path: String,
    pub sha256: String,
    /// The library track it is, when the catalog lists it.
    pub local_track_id: Option<String>,
}

/// One planned file.
#[derive(Debug, Clone)]
pub struct PlannedItem {
    pub ordinal: i64,
    pub bundle_ordinal: i64,
    pub album_id: String,
    pub album_revision: i64,
    pub track_id: Option<String>,
    pub source_root: String,
    pub source_rel: String,
    pub dest_root: Option<String>,
    pub dest_rel: Option<String>,
    pub eligibility: Eligibility,
    pub estimated_bytes: u64,
    pub fingerprint: String,
    pub desired: DesiredDocument,
    pub diff: ItemDiff,
    pub facts: ItemFacts,
}

impl PlannedItem {
    pub fn reason(&self) -> Option<&ItemReason> {
        self.facts.reason.as_ref()
    }

    pub fn block(&mut self, reason: ItemReason) {
        self.eligibility = Eligibility::Blocked;
        self.facts.blockers.push(reason.message.clone());
        self.facts.reason = Some(reason);
        self.desired.plan = None;
    }
}

/// What a preview counted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ManagementSummaryView)]
pub struct Summary {
    #[serde(default)]
    pub selected_item_count: Option<i64>,
    #[schema(required = true)]
    #[serde(default)]
    pub item_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub bundle_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub eligible_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub warning_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub blocked_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub stale_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub no_change_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub tag_change_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub artwork_change_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub path_change_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub sidecar_change_count: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub estimated_temporary_bytes: i64,
    #[schema(required = true)]
    #[serde(default)]
    pub expanded_track_count: i64,
    /// Files per reason code.
    #[schema(required = true)]
    #[serde(default)]
    pub reasons: BTreeMap<String, i64>,
    /// Files per library folder.
    #[schema(required = true)]
    #[serde(default)]
    pub roots: BTreeMap<String, i64>,
    /// Files per audio format.
    #[schema(required = true)]
    #[serde(default)]
    pub formats: BTreeMap<String, i64>,
    #[schema(required = true)]
    #[serde(default)]
    pub deferred_sources: BTreeMap<String, i64>,
    #[schema(required = true)]
    #[serde(default)]
    pub metadata_snapshot_ids: Vec<String>,
}

impl Summary {
    pub fn count(&mut self, item: &PlannedItem) {
        self.item_count += 1;
        match item.eligibility {
            Eligibility::Eligible => self.eligible_count += 1,
            Eligibility::Warning => self.warning_count += 1,
            Eligibility::Blocked => self.blocked_count += 1,
            Eligibility::Stale => self.stale_count += 1,
        }
        if item.eligibility.writes() {
            if !item.diff.changes_anything() {
                self.no_change_count += 1;
            }
            if item.diff.tags_changed {
                self.tag_change_count += 1;
            }
            if item.diff.path_changed {
                self.path_change_count += 1;
            }
            if !item.diff.sidecars.is_empty() {
                self.sidecar_change_count += 1;
            }
            if item.diff.artwork_changed
                || item
                    .diff
                    .generated
                    .iter()
                    .any(|file| file.kind == "artwork")
            {
                self.artwork_change_count += 1;
            }
            self.estimated_temporary_bytes += i64::try_from(item.estimated_bytes).unwrap_or(0);
        }
        if let Some(reason) = item.reason() {
            *self.reasons.entry(reason.code.clone()).or_default() += 1;
        }
        *self.roots.entry(item.source_root.clone()).or_default() += 1;
        *self
            .formats
            .entry(item.facts.audio_format.clone())
            .or_default() += 1;
    }
}

/// A management job as the pages read it.
#[derive(Debug, Clone)]
pub struct JobDetail {
    pub job: OperationJob,
    pub mode: String,
    pub origin: String,
    pub phase: ManagementPhase,
    pub profile_id: String,
    pub profile_name: String,
    pub profile_revision: String,
    pub settings_revision: String,
    pub policy_revision: String,
    pub catalog_revision: i64,
    pub target_root_id: Option<String>,
    pub linked_job_id: Option<String>,
    pub selection: serde_json::Value,
    pub summary: Summary,
    pub preview_created_at: Option<f64>,
    pub expires_at: Option<f64>,
    pub expired: bool,
    pub stale_reasons: Vec<String>,
    pub ready_for_confirmation: bool,
    pub lease_expires_at: Option<f64>,
    pub heartbeat_at: Option<f64>,
    pub worker_stalled: bool,
    pub undo_available_count: i64,
    /// Files whose undo period has passed.
    pub undo_expired_count: i64,
    /// When the first remaining undo runs out.
    pub undo_expires_at: Option<f64>,
    /// Files of this job with a saved original to restore.
    pub baseline_available_count: i64,
    /// For an activation dry run: the settings revision it proves.
    pub proposed_settings_revision: Option<String>,
    /// Media servers told to rescan after the apply.
    pub external_refreshes: Vec<serde_json::Value>,
}

/// One page of planned files.
#[derive(Debug, Clone)]
pub struct ItemPage {
    pub items: Vec<PlannedItem>,
    pub next_after_ordinal: Option<i64>,
    pub has_more: bool,
}

/// Filters for the planned-file page.
#[derive(Debug, Clone, Default)]
pub struct ItemFilter {
    pub eligibility: Option<Eligibility>,
    pub reason_code: Option<String>,
    pub root_id: Option<String>,
    pub album_id: Option<String>,
    pub audio_format: Option<String>,
    /// `tags`, `path`, `sidecars`, or `no_change`.
    pub change_kind: Option<String>,
}

/// Where one planned file's write is after apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    /// No write was planned for it.
    NotScheduled,
}

impl From<WorkState> for ResultState {
    fn from(state: WorkState) -> Self {
        match state {
            WorkState::Pending => Self::Pending,
            WorkState::Running => Self::Running,
            WorkState::Succeeded => Self::Succeeded,
            WorkState::Failed => Self::Failed,
            WorkState::Skipped => Self::Skipped,
        }
    }
}

/// One file's result after apply.
#[derive(Debug, Clone)]
pub struct ResultItem {
    pub plan: PlannedItem,
    pub work_state: ResultState,
    pub failure: Option<ItemReason>,
    pub result: serde_json::Value,
}

/// One page of results.
#[derive(Debug, Clone)]
pub struct ResultPage {
    pub items: Vec<ResultItem>,
    pub next_after_ordinal: Option<i64>,
    pub has_more: bool,
}

/// One history row.
#[derive(Debug, Clone)]
pub struct HistoryItem {
    pub job: OperationJob,
    pub mode: String,
    pub origin: String,
    pub phase: ManagementPhase,
    pub profile_id: String,
    pub profile_name: String,
    pub profile_revision: String,
    pub target_root_id: Option<String>,
    pub selection: serde_json::Value,
    pub summary: Summary,
    pub expires_at: Option<f64>,
}

/// History filters.
#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    pub limit: i64,
    pub cursor: Option<(f64, String)>,
    pub origin: Option<String>,
    pub profile_id: Option<String>,
    pub root_id: Option<String>,
    pub state: Option<String>,
    pub mode: Option<String>,
    pub created_from: Option<f64>,
    pub created_to: Option<f64>,
}

/// A newly made (or replayed) preview and the token that applies it.
#[derive(Debug, Clone)]
pub struct PreviewHandle {
    pub job_id: String,
    pub preview_token: String,
    pub created_at: f64,
    pub expires_at: f64,
    pub existing: bool,
}

/// Why a management request was refused.
#[derive(Debug, thiserror::Error)]
pub enum ManagementError {
    #[error("{0}")]
    NotFound(&'static str),
    #[error("{0}")]
    Invalid(String),
    /// Valid request against state that moved or does not allow it.
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("store: {0}")]
    Store(String),
    /// The database was too busy to answer, or is shutting down.
    #[error("busy: {0}")]
    Busy(String),
}

impl From<droppedneedle_kernel::db::DbError> for ManagementError {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        use droppedneedle_kernel::db::DbError;
        match error {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Busy(error.to_string()),
            other => Self::Store(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for ManagementError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}

impl From<serde_json::Error> for ManagementError {
    fn from(error: serde_json::Error) -> Self {
        Self::Store(error.to_string())
    }
}
