//! Wire shapes the library settings and Library Management services
//! return: the library policy views and the Library Management profile
//! responses.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::management::settings::ProfileImportWarning;
use droppedneedle_kernel::runtime_config::Masked;
use droppedneedle_kernel::runtime_config::secret_sections::{IdentificationPolicy, TypedLibrary};
use droppedneedle_kernel::runtime_config::sections::{
    FieldMode, LibraryManagement, LibraryManagementProfile, NamingScript, TaggingScript,
};

/// GET view of the library settings: the settings (AcoustID key masked)
/// plus the policy revision, the reconciliation projection, and
/// non-blocking warnings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibrarySettingsResponse {
    /// Normalized settings, AcoustID key masked.
    #[serde(flatten)]
    #[schema(value_type = TypedLibrary)]
    pub settings: Masked<TypedLibrary>,
    /// Content revision (write CAS token).
    pub policy_revision: String,
    /// Whether reconciliation is still required.
    pub reconciliation_required: bool,
    /// `applied` or `awaiting_reconciliation`.
    pub reconciliation_state: String,
    /// Pending revision, when awaiting reconciliation.
    pub pending_policy_revision: Option<String>,
    /// Affected scope ids.
    pub affected_scope_ids: Vec<String>,
    /// Actions the save applied.
    pub actions_applied: Vec<String>,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
    /// Where the import naming script puts an example track; absent when
    /// the stored script places nothing.
    pub naming_example: Option<String>,
}

/// GET/PUT view of Library Management: the settings plus their content
/// revision (the write CAS token).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementSettingsResponse {
    /// The stored settings.
    #[serde(flatten)]
    pub settings: LibraryManagement,
    /// Settings content revision.
    pub settings_revision: String,
    /// Every tag field a profile can manage, in the editor's order, with
    /// the genre row last.
    pub managed_fields: Vec<ManagedFieldInfo>,
}

/// One row of a profile's field table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ManagedFieldInfo {
    /// The profile's name for the field.
    pub name: String,
    /// What the editor calls it.
    pub label: String,
    /// What MusicBrainz fact the value comes from. `None`: nothing gives
    /// one, so the file keeps its value unless a tagging script sets it,
    /// or replace with clear removes it.
    pub source: Option<String>,
    /// Whether merge is offered.
    pub mergeable: bool,
    /// Whether replace offers to clear the value when the source has none.
    pub clearable: bool,
    /// The modes the field offers.
    pub modes: Vec<FieldMode>,
}

/// Which kind of scope a policy-tree node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyNodeKind {
    /// A whole library root.
    Root,
    /// A path rule inside a root.
    Rule,
}

/// One policy-tree node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyTreeNode {
    /// Node id (root or rule id).
    pub id: String,
    /// Root or rule.
    pub kind: PolicyNodeKind,
    /// Display label.
    pub label: String,
    /// Absolute path for a root, the root-relative path for a rule.
    pub path: String,
    /// Policy set on this node.
    pub policy: IdentificationPolicy,
    /// Id the policy comes from.
    pub inherited_from_id: Option<String>,
    /// Whether the path exists on disk right now.
    pub available: bool,
    /// Indexed catalog files under the node, when the catalog is wired.
    pub indexed_file_count: Option<i64>,
    /// Indexed plus excluded catalog files under the node, when the
    /// catalog is wired.
    pub on_disk_file_count: Option<i64>,
    /// Child rule nodes.
    #[schema(no_recursion)]
    pub children: Vec<LibraryPolicyTreeNode>,
}

/// Policy-tree response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyTreeResponse {
    /// Content revision the tree was built from.
    pub policy_revision: String,
    /// Root nodes.
    pub roots: Vec<LibraryPolicyTreeNode>,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
}

/// Impact preview: how the candidate settings differ from the saved ones
/// and how much of the catalog the change touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyImpactResponse {
    /// Saved revision.
    pub current_policy_revision: String,
    /// Revision of the normalized candidate.
    pub proposed_policy_revision: String,
    /// The caller's expected revision no longer matches the saved one.
    pub stale: bool,
    /// Whether any root or rule changes its effective policy.
    pub reconciliation_required: bool,
    /// Root and rule ids whose effective policy changes.
    pub affected_scope_ids: Vec<String>,
    /// Indexed catalog files under the affected scopes, when wired.
    pub indexed_file_count: Option<i64>,
    /// Indexed plus excluded catalog files under the affected scopes,
    /// when wired.
    pub on_disk_file_count: Option<i64>,
    /// Some affected scope becomes excluded.
    pub content_will_become_unavailable: bool,
    /// Some affected scope stops being automatic, so queued
    /// identification there no longer applies.
    pub queued_work_will_be_cancelled: bool,
    /// Non-blocking warnings about the candidate.
    pub warnings: Vec<String>,
}

/// Apply preview: how many catalog files a reconcile of the chosen
/// scopes would revisit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyApplyPreviewResponse {
    /// Saved revision the preview was built from.
    pub policy_revision: String,
    /// The scope ids as requested.
    pub scope_ids: Vec<String>,
    /// Catalog files under the chosen scopes.
    pub estimated_file_count: i64,
    /// Some chosen scope is excluded.
    pub content_will_become_unavailable: bool,
    /// Whether the last save cancelled queued work. Saves never cancel
    /// queued work on this server, so this is always false.
    pub queued_work_was_cancelled_on_save: bool,
}

/// A root the catalog still holds tracks for but the settings no
/// longer list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryRestorableRoot {
    /// Root id the catalog rows carry.
    pub root_id: String,
    /// Root path recovered from the catalog rows.
    pub path: String,
    /// Catalog files under the root.
    pub indexed_file_count: i64,
}

/// Where a mapped path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathMappingSource {
    /// A catalog track file.
    LibraryFile,
}

/// Why a path did not map to a root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathMappingError {
    /// The path sits under more than one root.
    Ambiguous,
    /// The path sits under no root.
    OutOfRoot,
}

/// One catalog path and the root it maps to under the saved settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPathMappingItem {
    /// Source kind.
    pub source_kind: PathMappingSource,
    /// Track id.
    pub source_id: String,
    /// Absolute file path the catalog holds.
    pub absolute_path: String,
    /// Root the path maps to, when it maps.
    pub root_id: Option<String>,
    /// Path relative to that root, when it maps.
    pub relative_path: Option<String>,
    /// Why it did not map.
    pub error: Option<PathMappingError>,
}

/// Dry run: does every catalog path map to exactly one saved root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPathMappingReport {
    /// Saved revision the report was built from.
    pub policy_revision: String,
    /// Paths checked.
    pub source_count: i64,
    /// Paths that map to one root.
    pub mapped_count: i64,
    /// Paths under more than one root.
    pub ambiguous_count: i64,
    /// Paths under no root.
    pub out_of_root_count: i64,
    /// Some path does not map.
    pub blocking: bool,
    /// Every checked path, ordered by track id.
    pub items: Vec<LibraryPathMappingItem>,
}

/// Profile-mutation acknowledgement: the saved profile plus the fresh
/// revision (a second read, so concurrent saves surface as staleness on
/// the next write instead of silently winning).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileMutationResponse {
    /// Saved profile.
    pub profile: LibraryManagementProfile,
    /// Fresh settings revision.
    pub settings_revision: String,
}

/// Exported bundle: a portable document plus a share code, both pinned
/// by the bundle hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileExportResponse {
    /// Suggested download filename.
    pub filename: String,
    /// Bundle MIME type.
    pub mime_type: String,
    /// Portable document (JSON).
    pub document: String,
    /// Share code (`DNLP1:...`).
    pub share_code: String,
    /// Bundle hash (import pins to the reviewed hash).
    pub bundle_hash: String,
    /// Settings revision at export time.
    pub settings_revision: String,
}

/// Import-preview response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportPreviewResponse {
    /// Materialized profile (names resolved against stored settings).
    pub profile: LibraryManagementProfile,
    /// Reviewed bundle hash (the import must pin to this).
    pub bundle_hash: String,
    /// Settings revision at preview time.
    pub settings_revision: String,
    /// Naming scripts the bundle carries.
    pub naming_scripts: Vec<NamingScript>,
    /// Tagging scripts the bundle carries.
    pub tagging_scripts: Vec<TaggingScript>,
    /// Capability aspects the profile uses.
    pub aspects: Vec<String>,
    /// Import warnings.
    pub warnings: Vec<ProfileImportWarning>,
}

/// Import-confirm response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportResponse {
    /// Imported profile.
    pub profile: LibraryManagementProfile,
    /// New settings revision.
    pub settings_revision: String,
    /// Naming scripts the import added.
    pub naming_scripts: Vec<NamingScript>,
    /// Tagging scripts the import added.
    pub tagging_scripts: Vec<TaggingScript>,
}
