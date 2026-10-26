//! Wire shapes for the settings surface that are not config sections.
//!
//! Sections themselves cross the wire as their `runtime_config` types
//! (secret sections wrapped in [`Masked`]). What lives here is everything
//! else a route needs: verify verdicts, request envelopes with CAS tokens,
//! views that add a computed field to a section (flattened, so the JSON
//! stays one object), and the Library Management request shapes. The
//! library responses live in `library::settings_models`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use droppedneedle_kernel::runtime_config::Masked;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, LibraryManagement, LibraryManagementProfile, LibraryScanSchedule, MbSourceMode,
    MusicBrainzSettings, UsenetBackend,
};
use droppedneedle_library::settings_models::LibraryRestorableRoot;

// --- views over one section ----------------------------------------------------

/// GET payload: the persisted schedule plus the server's timezone label, so
/// the UI can show what "daily at HH:MM" is relative to, and when the last
/// scan finished, from the scan history. Both are computed per request and
/// never persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryScanScheduleResponse {
    /// The stored schedule.
    #[serde(flatten)]
    pub schedule: LibraryScanSchedule,
    /// Server-local timezone label for the daily-scan picker.
    pub server_timezone: String,
    /// When the last scan finished successfully, unix seconds.
    pub last_scan_at: Option<f64>,
}

/// Acquisition source try-order, e.g. `["soulseek", "usenet"]`. Bundled
/// sources are always present; well-formed `plugin:<name>` keys pass
/// through order-preserved; anything else is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SourcePriorityOrder {
    /// Try-order, best first.
    pub order: Vec<String>,
}

/// The active Usenet search backend (either/or): `"indexers"` for the
/// native Newznab priority list, `"prowlarr"` for the single Prowlarr
/// connection. Unknown values are a 400, never a silent reset.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct UsenetSearchBackend {
    /// Active backend.
    pub backend: UsenetBackend,
}

/// GET view of the acquisition policy: the stored policy plus the
/// read-only recipe verdict, recomputed on every read and never saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DownloadPolicyView {
    /// The stored policy.
    #[serde(flatten)]
    pub policy: DownloadPolicy,
    /// Read-only recipe verdict: `v1`, `v2`, `non_convertible`, `invalid`.
    pub quality_recipe_status: String,
    /// Recipe verdict detail, when any.
    pub quality_recipe_error: Option<String>,
    /// Why the recycle bin path is not used, when it is not: each library
    /// folder's `.recycle` takes its place. Read-only.
    #[serde(default)]
    pub recycle_bin_warning: Option<String>,
}

/// GET view of the MusicBrainz connection: the settled settings plus the
/// transient pending BrainzMash proposal, when one is staged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzSettingsView {
    /// The settled settings.
    #[serde(flatten)]
    pub settings: MusicBrainzSettings,
    /// Transient pending proposal, when one is staged.
    pub pending_brainzmash: Option<BrainzmashPendingProposal>,
}

// --- verify verdicts ------------------------------------------------------------

/// Generic connection-test verdict. Reachable/bad-credential distinctions
/// ride in the body, never as a leaked 5xx, and the URL or host is never
/// echoed back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VerifyConnectionResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
}

/// slskd test verdict: validity plus the reported version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TestConnectionResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
}

/// Prowlarr test verdict: validity plus version and member-indexer count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProwlarrTestResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Enabled member indexers, when listed.
    pub indexer_count: Option<i64>,
}

/// SABnzbd test verdict: version plus the category list (for the picker),
/// the SABnzbd-side completed dir (the mount hint), and the mount
/// diagnosis over the submitted mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SabnzbdTestResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Known categories.
    pub categories: Vec<String>,
    /// SABnzbd-side completed dir, when known.
    pub complete_dir: Option<String>,
    /// Whether the submitted mount holds files, when diagnosed.
    pub mount_has_files: Option<bool>,
    /// Sampled downloads resolving under the submitted mount.
    pub resolvable_downloads: Option<i64>,
    /// Sampled downloads.
    pub sampled_downloads: Option<i64>,
    /// Actionable mount guidance, when the mount looks wrong.
    pub mount_message: Option<String>,
}

/// One Jellyfin user, for the admin user picker after a verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JellyfinUserInfo {
    /// User id.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// Jellyfin verify verdict, with the user list on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JellyfinVerifyResponse {
    /// Whether the connection checked out.
    pub success: bool,
    /// Human summary.
    pub message: String,
    /// Server users (empty unless the probe succeeded).
    pub users: Vec<JellyfinUserInfo>,
}

/// One Plex music-library section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlexLibrarySectionInfo {
    /// Section key.
    pub key: String,
    /// Section title.
    pub title: String,
}

/// Plex verify verdict, with music libraries on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlexVerifyResponse {
    /// Whether the connection checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
    /// Music libraries (empty unless the probe succeeded).
    pub libraries: Vec<PlexLibrarySectionInfo>,
}

/// Indexer caps-test verdict. `supports_audio_search` tells the user
/// whether structured music search will be used or the `t=search`
/// fallback; `suggested_url` is a one-click fix when the URL looks like
/// the site homepage but `/api` answers as a real endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexerTestResponse {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Whether structured music search is advertised.
    pub supports_audio_search: bool,
    /// Advertised category count.
    pub category_count: i64,
    /// One-click `/api` fix, when the submitted URL was the homepage.
    pub suggested_url: Option<String>,
}

// --- download policy and indexers ------------------------------------------------

/// Safe, signed-in-user projection of the acquisition policy: the quality
/// summary sentence and the recipe verdict only, no admin internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicySummaryResponse {
    /// Backend-composed contract sentence.
    pub summary: String,
    /// Whether a down-level image reproduces acquisition behavior.
    pub legacy_rollback_compatible: bool,
    /// Read-only recipe verdict: `v1`, `v2`, `non_convertible`, `invalid`.
    pub quality_recipe_status: String,
    /// Recipe verdict detail, when any.
    pub quality_recipe_error: Option<String>,
}

/// Indexer save acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexerSavedResponse {
    /// Saved indexer id.
    pub id: String,
}

/// Dragged-card priority order (1-based on save).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IndexerReorderRequest {
    /// Indexer ids in the new order.
    pub ordered_ids: Vec<String>,
}

/// Bare success acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OperationResult {
    /// Whether the operation succeeded.
    pub success: bool,
}

// --- musicbrainz ------------------------------------------------------------------

/// Transient pending BrainzMash proposal (process memory, echoed so the
/// UI can drive consent/verify/activate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BrainzmashPendingProposal {
    /// Pinned endpoint.
    pub endpoint: String,
    /// Proposed access revision.
    pub access_revision: String,
    /// Proposed source identity.
    pub source_id: String,
    /// Proposed source generation.
    pub generation: i64,
    /// Proposed disclosure version.
    pub disclosure_version: String,
    /// Consent recorded.
    pub consented: bool,
    /// Endpoint verified.
    pub verified: bool,
}

impl Default for BrainzmashPendingProposal {
    fn default() -> Self {
        Self {
            endpoint: "https://api.brainzmash.cc/ws/2".to_owned(),
            access_revision: String::new(),
            source_id: String::new(),
            generation: 1,
            disclosure_version: "brainzmash-v1".to_owned(),
            consented: false,
            verified: false,
        }
    }
}

/// Client-submitted MusicBrainz source change. BrainzMash is never a
/// direct update: it moves through stage/consent/verify/activate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MusicBrainzSettingsUpdate {
    /// Desired source tier.
    pub source_mode: MbSourceMode,
    /// API base for mirror/community tiers.
    pub api_url: Option<String>,
    /// Requests per second.
    pub rate_limit: f64,
    /// Community-tier disclosure acknowledged.
    pub community_acknowledged: Option<bool>,
}

impl Default for MusicBrainzSettingsUpdate {
    fn default() -> Self {
        Self {
            source_mode: MbSourceMode::Official,
            api_url: None,
            rate_limit: 1.0,
            community_acknowledged: Some(false),
        }
    }
}

/// Consent-bound BrainzMash binding request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MusicBrainzBindingRequest {
    /// Proposed access revision.
    pub access_revision: String,
    /// Proposed source identity.
    pub source_id: String,
    /// Proposed source generation.
    pub generation: i64,
    /// Proposed disclosure version.
    pub disclosure_version: String,
}

/// Verify payload: a BrainzMash consent binding or a plain source
/// update. The binding is tried first: it pins the exact staged
/// proposal, while an update only names a tier to probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum MusicBrainzVerifyRequest {
    /// Consent-bound BrainzMash verification.
    Binding(MusicBrainzBindingRequest),
    /// Plain tier probe (never BrainzMash).
    Update(MusicBrainzSettingsUpdate),
}

// --- library settings -------------------------------------------------------------

/// PUT body: full settings plus the required CAS token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibrarySettingsSaveRequest {
    /// Full candidate settings (a masked AcoustID key keeps the stored one).
    #[schema(value_type = TypedLibrary)]
    pub settings: Masked<TypedLibrary>,
    /// Compare-and-swap token from the last GET.
    pub expected_policy_revision: String,
}

/// Add-one-library-path body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPathRequest {
    /// Directory to add as a library root.
    pub path: String,
}

/// Remove-one-library-path query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPathQuery {
    /// Directory to remove.
    pub path: String,
}

/// Impact preview request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyImpactRequest {
    /// Full candidate settings.
    #[schema(value_type = TypedLibrary)]
    pub settings: Masked<TypedLibrary>,
    /// Revision the page loaded, if any. A mismatch sets `stale`.
    #[serde(default)]
    pub expected_policy_revision: Option<String>,
}

/// Apply-preview request: which saved scopes to reconcile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyApplyRequest {
    /// Root or rule ids; empty means every root.
    pub scope_ids: Vec<String>,
    /// Revision the page loaded; a mismatch is a 409.
    pub expected_policy_revision: String,
}

/// Removed roots that can be put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryRestorableRootsResponse {
    /// Saved revision (the token a restore must send).
    pub policy_revision: String,
    /// Restorable roots, sorted by id.
    pub restorable_roots: Vec<LibraryRestorableRoot>,
}

/// Restore request: put every removed root back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryRestoreRootsRequest {
    /// Revision the page loaded; a mismatch is a 409.
    pub expected_policy_revision: String,
    /// Path overrides by root id, for roots whose files moved.
    #[serde(default)]
    pub paths: Option<BTreeMap<String, String>>,
}

// --- advanced settings ------------------------------------------------------------

/// The advanced settings the web app reads at start. The browser's cache
/// lifetimes and the queue's polling interval are fixed in the web app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FrontendCacheTTLs {
    /// Discover-queue auto-generate switch.
    pub discover_queue_auto_generate: bool,
}

impl Default for FrontendCacheTTLs {
    fn default() -> Self {
        Self {
            discover_queue_auto_generate: true,
        }
    }
}

// --- library management -----------------------------------------------------------

/// PUT body: full settings plus the required CAS token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementSaveRequest {
    /// Full candidate settings.
    pub settings: LibraryManagement,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Impact/validate request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementSettingsImpactRequest {
    /// Full candidate settings.
    pub settings: LibraryManagement,
    /// Compare-and-swap token from the last GET, if any.
    pub expected_settings_revision: Option<String>,
}

/// Create-profile request (clones the default profile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileCreateRequest {
    /// Name for the new profile.
    pub name: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
    /// Description for the new profile.
    #[serde(default)]
    pub description: String,
}

/// Copy-profile request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileCopyRequest {
    /// Name for the copy.
    pub name: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Update-profile request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileUpdateRequest {
    /// Full replacement profile (id must match the path).
    pub profile: LibraryManagementProfile,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Delete-profile request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileDeleteRequest {
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Export request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileExportRequest {
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Import-preview request (accepts a document or a share code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportPreviewRequest {
    /// Bundle document or share code.
    pub content: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

/// Import-confirm request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportRequest {
    /// Bundle document or share code.
    pub content: String,
    /// Bundle hash the admin reviewed.
    pub reviewed_bundle_hash: String,
    /// Name for the imported profile.
    pub name: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
}

// --- section prefs ----------------------------------------------------------------

/// One toggleable UI section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionPrefItem {
    /// Section key.
    pub key: String,
    /// Display title.
    pub title: String,
    /// Description.
    pub description: String,
    /// Layout zone.
    pub zone: String,
    /// User toggle.
    pub enabled: bool,
    /// Backend availability (requires + linked services).
    pub available: bool,
    /// Service requirement, when any.
    pub requires: Option<String>,
}

/// Section prefs by page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SectionPrefsResponse {
    /// Sections per page (`home`, `discover`, `sidebar`).
    pub pages: BTreeMap<String, Vec<SectionPrefItem>>,
}

/// One section toggle update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionPrefUpdateItem {
    /// Section key.
    pub key: String,
    /// New toggle value.
    pub enabled: bool,
}

/// Section prefs update (one page per call).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionPrefsUpdate {
    /// Page to update (`home`, `discover`, `sidebar`).
    pub page: String,
    /// Toggles to apply.
    pub sections: Vec<SectionPrefUpdateItem>,
}
