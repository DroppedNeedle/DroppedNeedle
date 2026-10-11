//! Management job types: what a preview selects, what it plans for each
//! file, and what a job looks like to the pages that watch it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::reasons::ItemReason;
use crate::library::operations::models::OperationJob;
use crate::library::publish::planner::PlanItem;
use crate::runtime_config::sections::{
    LibraryManagementProfile, LibraryManagementRootOverrides, NamingScript, TaggingScript,
};

/// Most ids one selection may name.
pub const MAX_SELECTION_IDS: usize = 5_000;

/// What a preview covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionKind {
    Roots,
    Artists,
    Albums,
    Tracks,
    Filter,
}

/// Catalog filter for a `filter` selection; empty matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogFilter {
    pub search: Option<String>,
    pub genre: Option<String>,
    pub from_year: Option<i64>,
    pub to_year: Option<i64>,
    pub artist_ids: Vec<String>,
    pub album_artist_only: bool,
}

/// A preview's scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Put an earlier apply back.
    Undo,
}

impl JobMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Apply => "apply",
            Self::Undo => "undo",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "preview" => Some(Self::Preview),
            "apply" => Some(Self::Apply),
            "undo" => Some(Self::Undo),
            _ => None,
        }
    }
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Planning,
    Ready,
    Applying,
    Undoing,
    Complete,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Ready => "ready",
            Self::Applying => "applying",
            Self::Undoing => "undoing",
            Self::Complete => "complete",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "planning" => Some(Self::Planning),
            "ready" => Some(Self::Ready),
            "applying" => Some(Self::Applying),
            "undoing" => Some(Self::Undoing),
            "complete" => Some(Self::Complete),
            // v2 baseline restores and other phases this version does not
            // run read as finished history.
            _ => None,
        }
    }
}

/// Whether a planned file will be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl ItemDiff {
    pub fn changes_anything(&self) -> bool {
        self.tags_changed || self.path_changed || !self.sidecars.is_empty()
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub selected_item_count: Option<i64>,
    pub item_count: i64,
    pub bundle_count: i64,
    pub eligible_count: i64,
    pub warning_count: i64,
    pub blocked_count: i64,
    pub stale_count: i64,
    pub no_change_count: i64,
    pub tag_change_count: i64,
    pub artwork_change_count: i64,
    pub path_change_count: i64,
    pub sidecar_change_count: i64,
    pub estimated_temporary_bytes: i64,
    pub expanded_track_count: i64,
    pub reasons: BTreeMap<String, i64>,
    pub roots: BTreeMap<String, i64>,
    pub formats: BTreeMap<String, i64>,
    pub deferred_sources: BTreeMap<String, i64>,
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
    pub phase: String,
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

/// One file's result after apply.
#[derive(Debug, Clone)]
pub struct ResultItem {
    pub plan: PlannedItem,
    /// `pending`, `running`, `succeeded`, `failed`, `skipped`, or
    /// `not_scheduled`.
    pub work_state: String,
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
    pub phase: String,
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
