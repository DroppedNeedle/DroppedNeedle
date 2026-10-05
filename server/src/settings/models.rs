//! Wire shapes for the settings surface that are not config sections.
//!
//! Sections themselves cross the wire as their `runtime_config` types
//! (secret sections wrapped in [`Masked`]). What lives here is everything
//! else a route needs: verify verdicts, request envelopes with CAS tokens,
//! views that add a computed field to a section (flattened, so the JSON
//! stays one object), and the Library Management request and response
//! shapes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::runtime_config::Masked;
use crate::runtime_config::secret_sections::{
    AdvancedSettings, IdentificationPolicy, TypedLibrary,
};
use crate::runtime_config::sections::{
    DownloadPolicy, LibraryManagement, LibraryManagementProfile, LibraryScanSchedule, MbSourceMode,
    MusicBrainzSettings, NamingScript, TaggingScript, UsenetBackend,
};
use crate::settings::management::ProfileImportWarning;

// --- views over one section ----------------------------------------------------

/// GET payload: the persisted schedule plus the server's timezone label, so
/// the UI can show what "daily at HH:MM" is relative to. The label is
/// computed per request and never persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryScanScheduleResponse {
    /// The stored schedule.
    #[serde(flatten)]
    pub schedule: LibraryScanSchedule,
    /// Server-local timezone label for the daily-scan picker.
    pub server_timezone: String,
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
/// summary sentence plus the source-mode label only, no admin internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicySummaryResponse {
    /// Backend-composed contract sentence.
    pub summary: String,
    /// Source-mode label.
    pub source_mode: String,
    /// Whether a down-level image reproduces acquisition behavior.
    pub legacy_rollback_compatible: bool,
    /// Read-only recipe verdict: `v1`, `v2`, `non_convertible`, `invalid`.
    pub quality_recipe_status: String,
    /// Recipe verdict detail, when any.
    pub quality_recipe_error: Option<String>,
}

/// Admin preview of an unsaved policy against persisted state:
/// persisted-state bucket counts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyImpactResponse {
    /// Manual search jobs that would re-resolve.
    pub manual_search_jobs: i64,
    /// Queued rows with no attempts yet.
    pub queued_without_attempts: i64,
    /// Rows awaiting review.
    pub awaiting_review: i64,
    /// Remote-queued zero-byte rows.
    pub remote_queued_zero_byte: i64,
    /// Transferring rows (immutable under the new policy).
    pub transferring_immutable: i64,
    /// Held reviews.
    pub held_reviews: i64,
    /// Whether a down-level image would preserve acquisition behavior.
    pub legacy_representable: bool,
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
    /// Concurrent searches.
    pub concurrent_searches: i64,
    /// Community-tier disclosure acknowledged.
    pub community_acknowledged: Option<bool>,
}

impl Default for MusicBrainzSettingsUpdate {
    fn default() -> Self {
        Self {
            source_mode: MbSourceMode::Official,
            api_url: None,
            rate_limit: 1.0,
            concurrent_searches: 6,
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

/// One policy-tree node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyTreeNode {
    /// Node id (root or rule id).
    pub id: String,
    /// `root` or `rule`.
    pub kind: String,
    /// Display label.
    pub label: String,
    /// Absolute path.
    pub path: String,
    /// Effective policy.
    pub policy: IdentificationPolicy,
    /// Id the policy inherits from, when any.
    pub inherited_from_id: Option<String>,
    /// Whether the path is currently available.
    pub available: bool,
    /// Indexed file count, when the catalog port is wired.
    pub indexed_file_count: Option<i64>,
    /// On-disk file count, when the catalog port is wired.
    pub on_disk_file_count: Option<i64>,
    /// Child rule nodes.
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

/// Impact preview request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyImpactRequest {
    /// Full candidate settings.
    #[schema(value_type = TypedLibrary)]
    pub settings: Masked<TypedLibrary>,
    /// Compare-and-swap token from the last GET, if any.
    pub expected_policy_revision: Option<String>,
}

/// Impact preview: revision change plus affected scopes and counts.
/// Without the pending-policy machinery (library-engine follow-up),
/// reconciliation fields project the applied state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryPolicyImpactResponse {
    /// Current stored revision.
    pub current_policy_revision: String,
    /// Normalized candidate revision.
    pub proposed_policy_revision: String,
    /// The caller's revision was already stale.
    pub stale: bool,
    /// Whether the change needs reconciliation.
    pub reconciliation_required: bool,
    /// Affected scope ids.
    pub affected_scope_ids: Vec<String>,
    /// Indexed file count under the affected scopes, when wired.
    pub indexed_file_count: Option<i64>,
    /// On-disk file count under the affected scopes, when wired.
    pub on_disk_file_count: Option<i64>,
    /// Whether catalog content becomes unavailable.
    pub content_will_become_unavailable: bool,
    /// Whether queued work is cancelled.
    pub queued_work_will_be_cancelled: bool,
    /// Non-blocking warnings.
    pub warnings: Vec<String>,
}

// --- advanced settings ------------------------------------------------------------

/// Advanced tunables in the form's units. This is not a copy of the
/// section: the form shows human units (hours, minutes, seconds) while
/// the section stores seconds and milliseconds, v2's
/// `AdvancedSettingsFrontend` contract minus the dropped internal-tuning
/// fields. It is a closed allowlist: unknown JSON fields are rejected at
/// decode (400), so a client holding a dropped tuning field learns it is
/// gone instead of believing it saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, default)]
pub struct AdvancedSettingsForm {
    /// Album cache TTL for library albums, hours (1-168).
    pub cache_ttl_album_library: i64,
    /// Album cache TTL for non-library albums, hours (1-24).
    pub cache_ttl_album_non_library: i64,
    /// Artist cache TTL for library artists, hours (1-168).
    pub cache_ttl_artist_library: i64,
    /// Artist cache TTL for non-library artists, hours (1-168).
    pub cache_ttl_artist_non_library: i64,
    /// Discovery cache TTL for library artists, hours (1-168).
    pub cache_ttl_artist_discovery_library: i64,
    /// Discovery cache TTL for non-library artists, hours (1-168).
    pub cache_ttl_artist_discovery_non_library: i64,
    /// Search cache TTL, minutes (1-1440).
    pub cache_ttl_search: i64,
    /// Jellyfin recently-played TTL, minutes (1-60).
    pub cache_ttl_jellyfin_recently_played: i64,
    /// Jellyfin favorites TTL, minutes (1-60).
    pub cache_ttl_jellyfin_favorites: i64,
    /// Jellyfin genres TTL, minutes (1-1440).
    pub cache_ttl_jellyfin_genres: i64,
    /// Jellyfin library-stats TTL, minutes (1-60).
    pub cache_ttl_jellyfin_library_stats: i64,
    /// Navidrome albums TTL, minutes (1-60).
    pub cache_ttl_navidrome_albums: i64,
    /// Navidrome artists TTL, minutes (1-60).
    pub cache_ttl_navidrome_artists: i64,
    /// Navidrome recent TTL, minutes (1-60).
    pub cache_ttl_navidrome_recent: i64,
    /// Navidrome favorites TTL, minutes (1-60).
    pub cache_ttl_navidrome_favorites: i64,
    /// Navidrome search TTL, minutes (1-60).
    pub cache_ttl_navidrome_search: i64,
    /// Navidrome genres TTL, minutes (1-1440).
    pub cache_ttl_navidrome_genres: i64,
    /// Navidrome stats TTL, minutes (1-60).
    pub cache_ttl_navidrome_stats: i64,
    /// Plex albums TTL, minutes (1-60).
    pub cache_ttl_plex_albums: i64,
    /// Plex search TTL, minutes (1-60).
    pub cache_ttl_plex_search: i64,
    /// Plex genres TTL, minutes (1-1440).
    pub cache_ttl_plex_genres: i64,
    /// Plex stats TTL, minutes (1-60).
    pub cache_ttl_plex_stats: i64,
    /// Outbound HTTP timeout, seconds (5-60).
    pub http_timeout: i64,
    /// Outbound connect timeout, seconds (1-30).
    pub http_connect_timeout: i64,
    /// Outbound pool size (50-500).
    pub http_max_connections: i64,
    /// Artist-image batch size (1-20).
    pub batch_artist_images: i64,
    /// Album batch size (1-20).
    pub batch_albums: i64,
    /// Artist delay, seconds (0-5).
    pub delay_artist: f64,
    /// Album delay, seconds (0-5).
    pub delay_albums: f64,
    /// Memory-cache entries (1000-100000).
    pub memory_cache_max_entries: i64,
    /// Memory-cache cleanup cadence, seconds (60-3600).
    pub memory_cache_cleanup_interval: i64,
    /// Cover memory-cache entries (16-2048).
    pub cover_memory_cache_max_entries: i64,
    /// Cover memory-cache MB (1-1024).
    pub cover_memory_cache_max_size_mb: i64,
    /// Disk-cache cleanup cadence, minutes (1-60).
    pub disk_cache_cleanup_interval: i64,
    /// Recent-metadata cap, MB (100-5000).
    pub recent_metadata_max_size_mb: i64,
    /// Recent-covers cap, MB (100-10000).
    pub recent_covers_max_size_mb: i64,
    /// Persistent-metadata TTL, hours (1-168).
    pub persistent_metadata_ttl_hours: i64,
    /// Discover queue size (1-20).
    pub discover_queue_size: i64,
    /// Discover queue TTL, hours (1-168).
    pub discover_queue_ttl: i64,
    /// Discover queue auto-generate.
    pub discover_queue_auto_generate: bool,
    /// Discover queue polling, seconds (1-30).
    pub discover_queue_polling_interval: i64,
    /// Discover seed artists (1-10).
    pub discover_queue_seed_artists: i64,
    /// Discover wildcard slots (0-10).
    pub discover_queue_wildcard_slots: i64,
    /// Discover genre-affinity weight (0-1).
    pub discover_picks_genre_affinity_weight: f64,
    /// Discover picks count (4-30).
    pub discover_picks_count: i64,
    /// Home frontend TTL, minutes (1-60).
    pub frontend_ttl_home: i64,
    /// Discover frontend TTL, minutes (1-1440).
    pub frontend_ttl_discover: i64,
    /// Library frontend TTL, minutes (1-60).
    pub frontend_ttl_library: i64,
    /// Recently-added frontend TTL, minutes (1-60).
    pub frontend_ttl_recently_added: i64,
    /// Discover-queue frontend TTL, minutes (60-10080).
    pub frontend_ttl_discover_queue: i64,
    /// Search frontend TTL, minutes (1-60).
    pub frontend_ttl_search: i64,
    /// Local-files sidebar TTL, minutes (1-60).
    pub frontend_ttl_local_files_sidebar: i64,
    /// Jellyfin sidebar TTL, minutes (1-60).
    pub frontend_ttl_jellyfin_sidebar: i64,
    /// Plex sidebar TTL, minutes (1-60).
    pub frontend_ttl_plex_sidebar: i64,
    /// Playlist-sources frontend TTL, minutes (1-60).
    pub frontend_ttl_playlist_sources: i64,
    /// AudioDB provider switch.
    pub audiodb_enabled: bool,
    /// AudioDB name-search fallback.
    pub audiodb_name_search_fallback: bool,
    /// Serve remote images directly.
    pub direct_remote_images_enabled: bool,
    /// Prefer local cover art.
    pub prefer_local_cover_art: bool,
    /// AudioDB API key (masked unless unset).
    pub audiodb_api_key: String,
    /// AudioDB found TTL, hours (1-720).
    pub cache_ttl_audiodb_found: i64,
    /// AudioDB not-found TTL, hours (1-168).
    pub cache_ttl_audiodb_not_found: i64,
    /// AudioDB library TTL, hours (24-720).
    pub cache_ttl_audiodb_library: i64,
    /// Genre section TTL, hours (1-168).
    pub genre_section_ttl: i64,
    /// Request history retention, days (30-3650).
    pub request_history_retention_days: i64,
    /// Ignored-releases retention, days (30-3650).
    pub ignored_releases_retention_days: i64,
    /// Orphan-cover demote cadence, hours (1-168).
    pub orphan_cover_demote_interval_hours: i64,
    /// Store prune cadence, hours (1-168).
    pub store_prune_interval_hours: i64,
    /// Sync stall timeout, minutes (2-30).
    pub sync_stall_timeout_minutes: i64,
    /// Sync max timeout, hours (1-48).
    pub sync_max_timeout_hours: i64,
    /// Request concurrency (1-5).
    pub request_concurrency: i64,
}

impl AdvancedSettingsForm {
    /// Stored units floored back to the form's human units (v2
    /// `from_backend`: `// 3600`, `// 60`, `// 60000`, `// 1000`; the rest
    /// pass through). Pass a masked section: the AudioDB key is copied
    /// as it is.
    #[must_use]
    pub fn from_section(section: &AdvancedSettings) -> Self {
        Self {
            cache_ttl_album_library: section.cache_ttl_album_library / 3600,
            cache_ttl_album_non_library: section.cache_ttl_album_non_library / 3600,
            cache_ttl_artist_library: section.cache_ttl_artist_library / 3600,
            cache_ttl_artist_non_library: section.cache_ttl_artist_non_library / 3600,
            cache_ttl_artist_discovery_library: section.cache_ttl_artist_discovery_library / 3600,
            cache_ttl_artist_discovery_non_library: section.cache_ttl_artist_discovery_non_library
                / 3600,
            cache_ttl_search: section.cache_ttl_search / 60,
            cache_ttl_jellyfin_recently_played: section.cache_ttl_jellyfin_recently_played / 60,
            cache_ttl_jellyfin_favorites: section.cache_ttl_jellyfin_favorites / 60,
            cache_ttl_jellyfin_genres: section.cache_ttl_jellyfin_genres / 60,
            cache_ttl_jellyfin_library_stats: section.cache_ttl_jellyfin_library_stats / 60,
            cache_ttl_navidrome_albums: section.cache_ttl_navidrome_albums / 60,
            cache_ttl_navidrome_artists: section.cache_ttl_navidrome_artists / 60,
            cache_ttl_navidrome_recent: section.cache_ttl_navidrome_recent / 60,
            cache_ttl_navidrome_favorites: section.cache_ttl_navidrome_favorites / 60,
            cache_ttl_navidrome_search: section.cache_ttl_navidrome_search / 60,
            cache_ttl_navidrome_genres: section.cache_ttl_navidrome_genres / 60,
            cache_ttl_navidrome_stats: section.cache_ttl_navidrome_stats / 60,
            cache_ttl_plex_albums: section.cache_ttl_plex_albums / 60,
            cache_ttl_plex_search: section.cache_ttl_plex_search / 60,
            cache_ttl_plex_genres: section.cache_ttl_plex_genres / 60,
            cache_ttl_plex_stats: section.cache_ttl_plex_stats / 60,
            http_timeout: section.http_timeout,
            http_connect_timeout: section.http_connect_timeout,
            http_max_connections: section.http_max_connections,
            batch_artist_images: section.batch_artist_images,
            batch_albums: section.batch_albums,
            delay_artist: section.delay_artist,
            delay_albums: section.delay_albums,
            memory_cache_max_entries: section.memory_cache_max_entries,
            memory_cache_cleanup_interval: section.memory_cache_cleanup_interval,
            cover_memory_cache_max_entries: section.cover_memory_cache_max_entries,
            cover_memory_cache_max_size_mb: section.cover_memory_cache_max_size_mb,
            disk_cache_cleanup_interval: section.disk_cache_cleanup_interval / 60,
            recent_metadata_max_size_mb: section.recent_metadata_max_size_mb,
            recent_covers_max_size_mb: section.recent_covers_max_size_mb,
            persistent_metadata_ttl_hours: section.persistent_metadata_ttl_hours,
            discover_queue_size: section.discover_queue_size,
            discover_queue_ttl: section.discover_queue_ttl / 3600,
            discover_queue_auto_generate: section.discover_queue_auto_generate,
            discover_queue_polling_interval: section.discover_queue_polling_interval / 1000,
            discover_queue_seed_artists: section.discover_queue_seed_artists,
            discover_queue_wildcard_slots: section.discover_queue_wildcard_slots,
            discover_picks_genre_affinity_weight: section.discover_picks_genre_affinity_weight,
            discover_picks_count: section.discover_picks_count,
            frontend_ttl_home: section.frontend_ttl_home / 60000,
            frontend_ttl_discover: section.frontend_ttl_discover / 60000,
            frontend_ttl_library: section.frontend_ttl_library / 60000,
            frontend_ttl_recently_added: section.frontend_ttl_recently_added / 60000,
            frontend_ttl_discover_queue: section.frontend_ttl_discover_queue / 60000,
            frontend_ttl_search: section.frontend_ttl_search / 60000,
            frontend_ttl_local_files_sidebar: section.frontend_ttl_local_files_sidebar / 60000,
            frontend_ttl_jellyfin_sidebar: section.frontend_ttl_jellyfin_sidebar / 60000,
            frontend_ttl_plex_sidebar: section.frontend_ttl_plex_sidebar / 60000,
            frontend_ttl_playlist_sources: section.frontend_ttl_playlist_sources / 60000,
            audiodb_enabled: section.audiodb_enabled,
            audiodb_name_search_fallback: section.audiodb_name_search_fallback,
            direct_remote_images_enabled: section.direct_remote_images_enabled,
            prefer_local_cover_art: section.prefer_local_cover_art,
            audiodb_api_key: section.audiodb_api_key.expose().to_owned(),
            cache_ttl_audiodb_found: section.cache_ttl_audiodb_found / 3600,
            cache_ttl_audiodb_not_found: section.cache_ttl_audiodb_not_found / 3600,
            cache_ttl_audiodb_library: section.cache_ttl_audiodb_library / 3600,
            genre_section_ttl: section.genre_section_ttl / 3600,
            request_history_retention_days: section.request_history_retention_days,
            ignored_releases_retention_days: section.ignored_releases_retention_days,
            orphan_cover_demote_interval_hours: section.orphan_cover_demote_interval_hours,
            store_prune_interval_hours: section.store_prune_interval_hours,
            sync_stall_timeout_minutes: section.sync_stall_timeout_minutes,
            sync_max_timeout_hours: section.sync_max_timeout_hours,
            request_concurrency: section.request_concurrency,
        }
    }

    /// Form units scaled to stored units (v2 `to_backend`: `* 3600`,
    /// `* 60`, `* 60000`, `* 1000`; the rest pass through). The AudioDB
    /// key follows the mask rule on save.
    #[must_use]
    pub fn into_section(self) -> Masked<AdvancedSettings> {
        Masked::from(AdvancedSettings {
            cache_ttl_album_library: self.cache_ttl_album_library * 3600,
            cache_ttl_album_non_library: self.cache_ttl_album_non_library * 3600,
            cache_ttl_artist_library: self.cache_ttl_artist_library * 3600,
            cache_ttl_artist_non_library: self.cache_ttl_artist_non_library * 3600,
            cache_ttl_artist_discovery_library: self.cache_ttl_artist_discovery_library * 3600,
            cache_ttl_artist_discovery_non_library: self.cache_ttl_artist_discovery_non_library
                * 3600,
            cache_ttl_search: self.cache_ttl_search * 60,
            cache_ttl_jellyfin_recently_played: self.cache_ttl_jellyfin_recently_played * 60,
            cache_ttl_jellyfin_favorites: self.cache_ttl_jellyfin_favorites * 60,
            cache_ttl_jellyfin_genres: self.cache_ttl_jellyfin_genres * 60,
            cache_ttl_jellyfin_library_stats: self.cache_ttl_jellyfin_library_stats * 60,
            cache_ttl_navidrome_albums: self.cache_ttl_navidrome_albums * 60,
            cache_ttl_navidrome_artists: self.cache_ttl_navidrome_artists * 60,
            cache_ttl_navidrome_recent: self.cache_ttl_navidrome_recent * 60,
            cache_ttl_navidrome_favorites: self.cache_ttl_navidrome_favorites * 60,
            cache_ttl_navidrome_search: self.cache_ttl_navidrome_search * 60,
            cache_ttl_navidrome_genres: self.cache_ttl_navidrome_genres * 60,
            cache_ttl_navidrome_stats: self.cache_ttl_navidrome_stats * 60,
            cache_ttl_plex_albums: self.cache_ttl_plex_albums * 60,
            cache_ttl_plex_search: self.cache_ttl_plex_search * 60,
            cache_ttl_plex_genres: self.cache_ttl_plex_genres * 60,
            cache_ttl_plex_stats: self.cache_ttl_plex_stats * 60,
            http_timeout: self.http_timeout,
            http_connect_timeout: self.http_connect_timeout,
            http_max_connections: self.http_max_connections,
            batch_artist_images: self.batch_artist_images,
            batch_albums: self.batch_albums,
            delay_artist: self.delay_artist,
            delay_albums: self.delay_albums,
            memory_cache_max_entries: self.memory_cache_max_entries,
            memory_cache_cleanup_interval: self.memory_cache_cleanup_interval,
            cover_memory_cache_max_entries: self.cover_memory_cache_max_entries,
            cover_memory_cache_max_size_mb: self.cover_memory_cache_max_size_mb,
            disk_cache_cleanup_interval: self.disk_cache_cleanup_interval * 60,
            recent_metadata_max_size_mb: self.recent_metadata_max_size_mb,
            recent_covers_max_size_mb: self.recent_covers_max_size_mb,
            persistent_metadata_ttl_hours: self.persistent_metadata_ttl_hours,
            discover_queue_size: self.discover_queue_size,
            discover_queue_ttl: self.discover_queue_ttl * 3600,
            discover_queue_auto_generate: self.discover_queue_auto_generate,
            discover_queue_polling_interval: self.discover_queue_polling_interval * 1000,
            discover_queue_seed_artists: self.discover_queue_seed_artists,
            discover_queue_wildcard_slots: self.discover_queue_wildcard_slots,
            discover_picks_genre_affinity_weight: self.discover_picks_genre_affinity_weight,
            discover_picks_count: self.discover_picks_count,
            frontend_ttl_home: self.frontend_ttl_home * 60000,
            frontend_ttl_discover: self.frontend_ttl_discover * 60000,
            frontend_ttl_library: self.frontend_ttl_library * 60000,
            frontend_ttl_recently_added: self.frontend_ttl_recently_added * 60000,
            frontend_ttl_discover_queue: self.frontend_ttl_discover_queue * 60000,
            frontend_ttl_search: self.frontend_ttl_search * 60000,
            frontend_ttl_local_files_sidebar: self.frontend_ttl_local_files_sidebar * 60000,
            frontend_ttl_jellyfin_sidebar: self.frontend_ttl_jellyfin_sidebar * 60000,
            frontend_ttl_plex_sidebar: self.frontend_ttl_plex_sidebar * 60000,
            frontend_ttl_playlist_sources: self.frontend_ttl_playlist_sources * 60000,
            audiodb_enabled: self.audiodb_enabled,
            audiodb_name_search_fallback: self.audiodb_name_search_fallback,
            direct_remote_images_enabled: self.direct_remote_images_enabled,
            prefer_local_cover_art: self.prefer_local_cover_art,
            audiodb_api_key: self.audiodb_api_key.into(),
            cache_ttl_audiodb_found: self.cache_ttl_audiodb_found * 3600,
            cache_ttl_audiodb_not_found: self.cache_ttl_audiodb_not_found * 3600,
            cache_ttl_audiodb_library: self.cache_ttl_audiodb_library * 3600,
            genre_section_ttl: self.genre_section_ttl * 3600,
            request_history_retention_days: self.request_history_retention_days,
            ignored_releases_retention_days: self.ignored_releases_retention_days,
            orphan_cover_demote_interval_hours: self.orphan_cover_demote_interval_hours,
            store_prune_interval_hours: self.store_prune_interval_hours,
            sync_stall_timeout_minutes: self.sync_stall_timeout_minutes,
            sync_max_timeout_hours: self.sync_max_timeout_hours,
            request_concurrency: self.request_concurrency,
        })
    }
}

/// Defaults are the stored defaults in form units.
impl Default for AdvancedSettingsForm {
    fn default() -> Self {
        Self::from_section(&AdvancedSettings::default())
    }
}

/// Frontend cache TTLs in backend units (milliseconds), verbatim from
/// the stored advanced section. The SPA reads this one endpoint instead
/// of the whole advanced surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FrontendCacheTTLs {
    /// Home TTL, ms.
    pub home: i64,
    /// Discover TTL, ms.
    pub discover: i64,
    /// Library TTL, ms.
    pub library: i64,
    /// Recently-added TTL, ms.
    pub recently_added: i64,
    /// Discover-queue TTL, ms.
    pub discover_queue: i64,
    /// Search TTL, ms.
    pub search: i64,
    /// Local-files sidebar TTL, ms.
    pub local_files_sidebar: i64,
    /// Jellyfin sidebar TTL, ms.
    pub jellyfin_sidebar: i64,
    /// Plex sidebar TTL, ms.
    pub plex_sidebar: i64,
    /// Playlist-sources TTL, ms.
    pub playlist_sources: i64,
    /// Discover-queue polling interval, ms.
    pub discover_queue_polling_interval: i64,
    /// Discover-queue auto-generate switch.
    pub discover_queue_auto_generate: bool,
}

impl Default for FrontendCacheTTLs {
    fn default() -> Self {
        Self {
            home: 300000,
            discover: 1800000,
            library: 300000,
            recently_added: 300000,
            discover_queue: 86400000,
            search: 300000,
            local_files_sidebar: 120000,
            jellyfin_sidebar: 120000,
            plex_sidebar: 120000,
            playlist_sources: 900000,
            discover_queue_polling_interval: 4000,
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

/// Export request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileExportRequest {
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
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

/// Import-preview request (accepts a document or a share code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LibraryManagementProfileImportPreviewRequest {
    /// Bundle document or share code.
    pub content: String,
    /// Compare-and-swap token from the last GET.
    pub expected_settings_revision: String,
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
