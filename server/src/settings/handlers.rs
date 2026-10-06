//! Thin settings handlers. Each answers one route: decode, call the
//! service, return its typed result. Sections cross the wire as their
//! config types; secret sections as [`Masked`], so a decrypted section
//! cannot be returned by accident.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Extension, Path, State},
};

use super::error::{SettingsError, ValidJson, ValidQuery};
use super::management::{ActivationHealth, ChangeImpact, PresetDiff};
use super::models::{
    AdvancedSettingsForm, DownloadPolicyView, FrontendCacheTTLs, IndexerReorderRequest,
    IndexerSavedResponse, IndexerTestResponse, JellyfinVerifyResponse,
    LibraryManagementProfileCopyRequest, LibraryManagementProfileCreateRequest,
    LibraryManagementProfileDeleteRequest, LibraryManagementProfileExportRequest,
    LibraryManagementProfileExportResponse, LibraryManagementProfileImportPreviewRequest,
    LibraryManagementProfileImportPreviewResponse, LibraryManagementProfileImportRequest,
    LibraryManagementProfileImportResponse, LibraryManagementProfileMutationResponse,
    LibraryManagementProfileUpdateRequest, LibraryManagementSaveRequest,
    LibraryManagementSettingsImpactRequest, LibraryManagementSettingsResponse,
    LibraryPathMappingReport, LibraryPathQuery, LibraryPathRequest,
    LibraryPolicyApplyPreviewResponse, LibraryPolicyApplyRequest, LibraryPolicyImpactRequest,
    LibraryPolicyImpactResponse, LibraryPolicyTreeResponse, LibraryRestorableRootsResponse,
    LibraryRestoreRootsRequest, LibraryScanScheduleResponse, LibrarySettingsResponse,
    LibrarySettingsSaveRequest, MusicBrainzBindingRequest, MusicBrainzSettingsUpdate,
    MusicBrainzSettingsView, MusicBrainzVerifyRequest, OperationResult, PlexLibrarySectionInfo,
    PlexVerifyResponse, PolicyImpactResponse, PolicySummaryResponse, ProwlarrTestResponse,
    SabnzbdTestResponse, SectionPrefsResponse, SectionPrefsUpdate, SourcePriorityOrder,
    TestConnectionResponse, UsenetSearchBackend, VerifyConnectionResponse,
};
use super::wiring::{AdminUser, SettingsSetup};
use crate::auth::session::middleware::CurrentSession;
use crate::runtime_config::Masked;
use crate::runtime_config::secret_sections::{
    EventsSettings, JellyfinConnection, LastFmSettings, ListenBrainzConnection,
    NavidromeConnection, NewznabIndexer, OidcConnection, PlexConnection, ProwlarrConnection,
    SabnzbdConnection, SlskdConnection, WrappedSettings, YouTubeConnection,
};
use crate::runtime_config::sections::{
    ConnectApps, DownloadPolicy, FilesystemWatcher, FreeMusic, GetIt, HomeSettings,
    LibraryManagementProfile, LibraryScanSchedule, PrimaryMusicSource, ScrobbleSettings,
    SecuritySettings, UserPreferences, WantedWatcher,
};

type JsonResult<T> = Result<Json<T>, SettingsError>;

// --- plain sections ------------------------------------------------------------

/// Read the release-type filters.
#[utoipa::path(
    get,
    path = "/api/v3/settings/preferences",
    responses((status = 200, description = "Release-type filters", body = UserPreferences))
)]
pub async fn get_preferences(State(settings): State<SettingsSetup>) -> JsonResult<UserPreferences> {
    settings.service().get().map(Json)
}

/// Save the release-type filters.
#[utoipa::path(
    put,
    path = "/api/v3/settings/preferences",
    request_body = UserPreferences,
    responses((status = 200, description = "Saved filters", body = UserPreferences))
)]
pub async fn put_preferences(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<UserPreferences>,
) -> JsonResult<UserPreferences> {
    settings.service().save(body).await.map(Json)
}

/// Read the scan schedule plus the server timezone label.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/schedule",
    responses((status = 200, description = "Scan schedule", body = LibraryScanScheduleResponse))
)]
pub async fn get_schedule(
    State(settings): State<SettingsSetup>,
) -> JsonResult<LibraryScanScheduleResponse> {
    settings.service().get_schedule().map(Json)
}

/// Save the scan schedule.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library/schedule",
    request_body = LibraryScanSchedule,
    responses((status = 200, description = "Saved schedule", body = LibraryScanScheduleResponse))
)]
pub async fn put_schedule(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryScanSchedule>,
) -> JsonResult<LibraryScanScheduleResponse> {
    settings.service().save_schedule(body).await.map(Json)
}

/// Read the filesystem poller knobs.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/watcher",
    responses((status = 200, description = "Filesystem poller knobs", body = FilesystemWatcher))
)]
pub async fn get_watcher(State(settings): State<SettingsSetup>) -> JsonResult<FilesystemWatcher> {
    settings.service().get().map(Json)
}

/// Save the filesystem poller knobs.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library/watcher",
    request_body = FilesystemWatcher,
    responses((status = 200, description = "Saved poller knobs", body = FilesystemWatcher))
)]
pub async fn put_watcher(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<FilesystemWatcher>,
) -> JsonResult<FilesystemWatcher> {
    settings.service().save(body).await.map(Json)
}

/// Read the scrobble targets.
#[utoipa::path(
    get,
    path = "/api/v3/settings/scrobble",
    responses((status = 200, description = "Scrobble targets", body = ScrobbleSettings))
)]
pub async fn get_scrobble(State(settings): State<SettingsSetup>) -> JsonResult<ScrobbleSettings> {
    settings.service().get().map(Json)
}

/// Save the scrobble targets.
#[utoipa::path(
    put,
    path = "/api/v3/settings/scrobble",
    request_body = ScrobbleSettings,
    responses((status = 200, description = "Saved targets", body = ScrobbleSettings))
)]
pub async fn put_scrobble(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ScrobbleSettings>,
) -> JsonResult<ScrobbleSettings> {
    settings.service().save(body).await.map(Json)
}

/// Read the primary music source.
#[utoipa::path(
    get,
    path = "/api/v3/settings/primary-source",
    responses((status = 200, description = "Primary music source", body = PrimaryMusicSource))
)]
pub async fn get_primary_source(
    State(settings): State<SettingsSetup>,
) -> JsonResult<PrimaryMusicSource> {
    settings.service().get().map(Json)
}

/// Save the primary music source.
#[utoipa::path(
    put,
    path = "/api/v3/settings/primary-source",
    request_body = PrimaryMusicSource,
    responses((status = 200, description = "Saved source", body = PrimaryMusicSource))
)]
pub async fn put_primary_source(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<PrimaryMusicSource>,
) -> JsonResult<PrimaryMusicSource> {
    settings.service().save(body).await.map(Json)
}

/// Read the Last.fm switch and the instance app key pair (masked unless
/// unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/lastfm",
    responses((status = 200, description = "Last.fm settings", body = LastFmSettings))
)]
pub async fn get_lastfm(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<LastFmSettings>> {
    settings.service().get_masked().map(Json)
}

/// Save the Last.fm switch and the instance app key pair (a masked value
/// keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/lastfm",
    request_body = LastFmSettings,
    responses((status = 200, description = "Saved settings", body = LastFmSettings))
)]
pub async fn put_lastfm(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<LastFmSettings>>,
) -> JsonResult<Masked<LastFmSettings>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Read the free-music settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/free-music",
    responses((status = 200, description = "Free-music settings", body = FreeMusic))
)]
pub async fn get_free_music(State(settings): State<SettingsSetup>) -> JsonResult<FreeMusic> {
    settings.service().get().map(Json)
}

/// Save the free-music settings.
#[utoipa::path(
    put,
    path = "/api/v3/settings/free-music",
    request_body = FreeMusic,
    responses((status = 200, description = "Saved settings", body = FreeMusic))
)]
pub async fn put_free_music(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<FreeMusic>,
) -> JsonResult<FreeMusic> {
    settings.service().save(body).await.map(Json)
}

/// Read the store-region settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/get-it",
    responses((status = 200, description = "Store-region settings", body = GetIt))
)]
pub async fn get_get_it(State(settings): State<SettingsSetup>) -> JsonResult<GetIt> {
    settings.service().get().map(Json)
}

/// Save the store-region settings.
#[utoipa::path(
    put,
    path = "/api/v3/settings/get-it",
    request_body = GetIt,
    responses((status = 200, description = "Saved settings", body = GetIt))
)]
pub async fn put_get_it(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<GetIt>,
) -> JsonResult<GetIt> {
    settings.service().save(body).await.map(Json)
}

/// Read the home page settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/home",
    operation_id = "get_home_settings",
    responses((status = 200, description = "Home page settings", body = HomeSettings))
)]
pub async fn get_home(State(settings): State<SettingsSetup>) -> JsonResult<HomeSettings> {
    settings.service().get().map(Json)
}

/// Save the home page settings. The save clears the cached home rows.
#[utoipa::path(
    put,
    path = "/api/v3/settings/home",
    request_body = HomeSettings,
    responses(
        (status = 200, description = "Saved settings", body = HomeSettings),
        (status = 400, description = "A value is out of range")
    )
)]
pub async fn put_home(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<HomeSettings>,
) -> JsonResult<HomeSettings> {
    settings.service().save(body).await.map(Json)
}

/// Read the security posture settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/security",
    responses((status = 200, description = "Security settings", body = SecuritySettings))
)]
pub async fn get_security(State(settings): State<SettingsSetup>) -> JsonResult<SecuritySettings> {
    settings.service().get().map(Json)
}

/// Save the security posture settings.
#[utoipa::path(
    put,
    path = "/api/v3/settings/security",
    request_body = SecuritySettings,
    responses((status = 200, description = "Saved settings", body = SecuritySettings))
)]
pub async fn put_security(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SecuritySettings>,
) -> JsonResult<SecuritySettings> {
    settings.service().save(body).await.map(Json)
}

/// Check the submitted HIBP hash-list path (must exist and start with
/// a `40-char-SHA1:count` line).
#[utoipa::path(
    post,
    path = "/api/v3/settings/security/verify-hibp",
    request_body = SecuritySettings,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_hibp(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SecuritySettings>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_hibp(body).await.map(Json)
}

/// Read the inbound Connect Apps config.
#[utoipa::path(
    get,
    path = "/api/v3/settings/connect-apps",
    responses((status = 200, description = "Connect Apps config", body = ConnectApps))
)]
pub async fn get_connect_apps(State(settings): State<SettingsSetup>) -> JsonResult<ConnectApps> {
    settings.service().get().map(Json)
}

/// Save the inbound Connect Apps config.
#[utoipa::path(
    put,
    path = "/api/v3/settings/connect-apps",
    request_body = ConnectApps,
    responses((status = 200, description = "Saved config", body = ConnectApps))
)]
pub async fn put_connect_apps(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ConnectApps>,
) -> JsonResult<ConnectApps> {
    settings.service().save(body).await.map(Json)
}

/// Read the wanted watcher toggles.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/wanted",
    responses((status = 200, description = "Wanted watcher toggles", body = WantedWatcher))
)]
pub async fn get_wanted(State(settings): State<SettingsSetup>) -> JsonResult<WantedWatcher> {
    settings.service().get().map(Json)
}

/// Save the wanted watcher toggles.
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/wanted",
    request_body = WantedWatcher,
    responses((status = 200, description = "Saved toggles", body = WantedWatcher))
)]
pub async fn put_wanted(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<WantedWatcher>,
) -> JsonResult<WantedWatcher> {
    settings.service().save(body).await.map(Json)
}

/// Read the acquisition source try-order.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/source-priority",
    responses((status = 200, description = "Source try-order", body = SourcePriorityOrder))
)]
pub async fn get_source_priority(
    State(settings): State<SettingsSetup>,
) -> JsonResult<SourcePriorityOrder> {
    settings.service().get_source_priority().map(Json)
}

/// Save the acquisition source try-order.
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/source-priority",
    request_body = SourcePriorityOrder,
    responses((status = 200, description = "Saved try-order", body = SourcePriorityOrder))
)]
pub async fn put_source_priority(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SourcePriorityOrder>,
) -> JsonResult<SourcePriorityOrder> {
    settings
        .service()
        .save_source_priority(body)
        .await
        .map(Json)
}

/// Read the active Usenet search backend.
#[utoipa::path(
    get,
    path = "/api/v3/settings/indexers/search-backend",
    responses((status = 200, description = "Search backend", body = UsenetSearchBackend))
)]
pub async fn get_search_backend(
    State(settings): State<SettingsSetup>,
) -> JsonResult<UsenetSearchBackend> {
    settings.service().get_usenet_backend().map(Json)
}

/// Save the active Usenet search backend. Unknown values are a 400.
#[utoipa::path(
    put,
    path = "/api/v3/settings/indexers/search-backend",
    request_body = UsenetSearchBackend,
    responses((status = 200, description = "Saved backend", body = UsenetSearchBackend))
)]
pub async fn put_search_backend(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<UsenetSearchBackend>,
) -> JsonResult<UsenetSearchBackend> {
    settings.service().save_usenet_backend(body).await.map(Json)
}

// --- library settings -------------------------------------------------------------

/// Read the typed library settings with the policy revision.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library",
    responses((status = 200, description = "Library settings", body = LibrarySettingsResponse))
)]
pub async fn get_library(
    State(settings): State<SettingsSetup>,
) -> JsonResult<LibrarySettingsResponse> {
    settings.service().get_library().await.map(Json)
}

/// Save the typed library settings. The expected revision must match
/// the stored one or the save is a 409. Dropping every root while the
/// catalog holds tracks is a 400.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library",
    request_body = LibrarySettingsSaveRequest,
    responses(
        (status = 200, description = "Saved library settings", body = LibrarySettingsResponse),
        (status = 400, description = "Invalid settings, or every root removed while the catalog holds tracks"),
        (status = 409, description = "Expected revision is stale")
    )
)]
pub async fn put_library(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibrarySettingsSaveRequest>,
) -> JsonResult<LibrarySettingsResponse> {
    settings.library_policy().save(body).await.map(Json)
}

/// Add one library root path. The path must be a directory on this
/// machine; re-adding is a silent no-op.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library/paths",
    request_body = LibraryPathRequest,
    responses((status = 200, description = "Library settings", body = LibrarySettingsResponse))
)]
pub async fn add_library_path(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryPathRequest>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .service()
        .add_library_path(&body.path)
        .await
        .map(Json)
}

/// Remove every library root at one path. Unknown paths are a silent
/// no-op; removing the last root while the catalog holds tracks is a 400.
#[utoipa::path(
    delete,
    path = "/api/v3/settings/library/paths",
    params(("path" = String, Query, description = "Directory to remove")),
    responses(
        (status = 200, description = "Library settings", body = LibrarySettingsResponse),
        (status = 400, description = "Removing the last root would orphan the catalog")
    )
)]
pub async fn remove_library_path(
    State(settings): State<SettingsSetup>,
    ValidQuery(query): ValidQuery<LibraryPathQuery>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .library_policy()
        .remove_path(&query.path)
        .await
        .map(Json)
}

/// The saved roots and their path rules as a tree, with catalog file
/// counts per node.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/policy-tree",
    responses((status = 200, description = "Policy tree", body = LibraryPolicyTreeResponse))
)]
pub async fn get_library_policy_tree(
    State(settings): State<SettingsSetup>,
) -> JsonResult<LibraryPolicyTreeResponse> {
    settings.library_policy().policy_tree().await.map(Json)
}

/// Preview what saving candidate library settings would change. Nothing
/// is saved; a stale expected revision only sets `stale`.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library/policy-impact",
    request_body = LibraryPolicyImpactRequest,
    responses(
        (status = 200, description = "Impact preview", body = LibraryPolicyImpactResponse),
        (status = 400, description = "Candidate settings are invalid")
    )
)]
pub async fn preview_library_policy_impact(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryPolicyImpactRequest>,
) -> JsonResult<LibraryPolicyImpactResponse> {
    settings
        .library_policy()
        .preview_impact(body)
        .await
        .map(Json)
}

/// Preview a reconcile of saved policy scopes: how many catalog files
/// it would revisit.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library/policy-apply-preview",
    request_body = LibraryPolicyApplyRequest,
    responses(
        (status = 200, description = "Apply preview", body = LibraryPolicyApplyPreviewResponse),
        (status = 400, description = "A scope id no longer exists"),
        (status = 409, description = "Expected revision is stale"),
        (status = 503, description = "Catalog reads are unwired")
    )
)]
pub async fn preview_library_policy_apply(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryPolicyApplyRequest>,
) -> JsonResult<LibraryPolicyApplyPreviewResponse> {
    settings
        .library_policy()
        .preview_apply(body)
        .await
        .map(Json)
}

/// Roots the catalog still holds tracks for that the settings no longer
/// list.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/restorable-roots",
    responses(
        (status = 200, description = "Restorable roots", body = LibraryRestorableRootsResponse),
        (status = 503, description = "Catalog reads are unwired")
    )
)]
pub async fn get_restorable_library_roots(
    State(settings): State<SettingsSetup>,
) -> JsonResult<LibraryRestorableRootsResponse> {
    settings.library_policy().restorable_roots().await.map(Json)
}

/// Put every removed root back into the settings.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library/restore-roots",
    request_body = LibraryRestoreRootsRequest,
    responses(
        (status = 200, description = "Saved library settings", body = LibrarySettingsResponse),
        (status = 400, description = "Nothing to restore, or a restored path is invalid"),
        (status = 409, description = "Expected revision is stale"),
        (status = 503, description = "Catalog reads are unwired")
    )
)]
pub async fn restore_library_roots(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryRestoreRootsRequest>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .library_policy()
        .restore_roots(body)
        .await
        .map(Json)
}

/// Dry run: map every catalog file path to a saved root.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/path-mapping",
    responses(
        (status = 200, description = "Path mapping report", body = LibraryPathMappingReport),
        (status = 503, description = "Catalog reads are unwired")
    )
)]
pub async fn get_library_path_mapping(
    State(settings): State<SettingsSetup>,
) -> JsonResult<LibraryPathMappingReport> {
    settings.library_policy().path_mapping().await.map(Json)
}

// --- advanced + cache TTLs ---------------------------------------------------------

/// Read the advanced tunables in form units.
#[utoipa::path(
    get,
    path = "/api/v3/settings/advanced",
    responses((status = 200, description = "Advanced tunables", body = AdvancedSettingsForm))
)]
pub async fn get_advanced(
    State(settings): State<SettingsSetup>,
) -> JsonResult<AdvancedSettingsForm> {
    settings.service().get_advanced().map(Json)
}

/// Save the advanced tunables. Unknown fields are a 400: the shape is a
/// closed allowlist.
#[utoipa::path(
    put,
    path = "/api/v3/settings/advanced",
    request_body = AdvancedSettingsForm,
    responses((status = 200, description = "Saved tunables", body = AdvancedSettingsForm))
)]
pub async fn put_advanced(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<AdvancedSettingsForm>,
) -> JsonResult<AdvancedSettingsForm> {
    settings.service().save_advanced(body).await.map(Json)
}

/// Read the frontend cache TTLs in stored units (milliseconds).
#[utoipa::path(
    get,
    path = "/api/v3/settings/cache-ttls",
    responses((status = 200, description = "Frontend cache TTLs", body = FrontendCacheTTLs))
)]
pub async fn get_cache_ttls(
    State(settings): State<SettingsSetup>,
) -> JsonResult<FrontendCacheTTLs> {
    settings.service().get_cache_ttls().map(Json)
}

// --- connections (secret sections) --------------------------------------------------

/// Read the Jellyfin connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/jellyfin",
    responses((status = 200, description = "Jellyfin connection", body = JellyfinConnection))
)]
pub async fn get_jellyfin(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<JellyfinConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the Jellyfin connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/jellyfin",
    request_body = JellyfinConnection,
    responses((status = 200, description = "Saved connection", body = JellyfinConnection))
)]
pub async fn put_jellyfin(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<JellyfinConnection>>,
) -> JsonResult<Masked<JellyfinConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted Jellyfin values (a masked key tests the stored
/// one). The user list rides along on success for the admin picker.
#[utoipa::path(
    post,
    path = "/api/v3/settings/jellyfin/verify",
    request_body = JellyfinConnection,
    responses((status = 200, description = "Verify verdict", body = JellyfinVerifyResponse))
)]
pub async fn verify_jellyfin(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<JellyfinConnection>>,
) -> JsonResult<JellyfinVerifyResponse> {
    settings.service().verify_jellyfin(body).await.map(Json)
}

/// Read the Navidrome connection (password masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/navidrome",
    responses((status = 200, description = "Navidrome connection", body = NavidromeConnection))
)]
pub async fn get_navidrome(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<NavidromeConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the Navidrome connection (a masked password keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/navidrome",
    request_body = NavidromeConnection,
    responses((status = 200, description = "Saved connection", body = NavidromeConnection))
)]
pub async fn put_navidrome(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<NavidromeConnection>>,
) -> JsonResult<Masked<NavidromeConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted Navidrome values (a masked password tests the
/// stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/navidrome/verify",
    request_body = NavidromeConnection,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_navidrome(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<NavidromeConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_navidrome(body).await.map(Json)
}

/// Read the Plex connection (token masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/plex",
    responses((status = 200, description = "Plex connection", body = PlexConnection))
)]
pub async fn get_plex(State(settings): State<SettingsSetup>) -> JsonResult<Masked<PlexConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the Plex connection (a masked token keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/plex",
    request_body = PlexConnection,
    responses((status = 200, description = "Saved connection", body = PlexConnection))
)]
pub async fn put_plex(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<PlexConnection>>,
) -> JsonResult<Masked<PlexConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted Plex values (a masked token tests the stored
/// one). Music libraries ride along on success.
#[utoipa::path(
    post,
    path = "/api/v3/settings/plex/verify",
    request_body = PlexConnection,
    responses((status = 200, description = "Verify verdict", body = PlexVerifyResponse))
)]
pub async fn verify_plex(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<PlexConnection>>,
) -> JsonResult<PlexVerifyResponse> {
    settings.service().verify_plex(body).await.map(Json)
}

/// List Plex music libraries over the stored connection. Unconfigured
/// is a 400; an unreachable Plex is a 502.
#[utoipa::path(
    get,
    path = "/api/v3/settings/plex/libraries",
    responses(
        (status = 200, description = "Music libraries", body = Vec<PlexLibrarySectionInfo>),
        (status = 400, description = "Plex is not configured"),
        (status = 502, description = "Plex is unreachable")
    )
)]
pub async fn get_plex_libraries(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Vec<PlexLibrarySectionInfo>> {
    settings.service().plex_libraries().await.map(Json)
}

/// Read the ListenBrainz connection (token masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/listenbrainz",
    responses((status = 200, description = "ListenBrainz connection", body = ListenBrainzConnection))
)]
pub async fn get_listenbrainz(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<ListenBrainzConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the ListenBrainz connection (a masked token keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/listenbrainz",
    request_body = ListenBrainzConnection,
    responses((status = 200, description = "Saved connection", body = ListenBrainzConnection))
)]
pub async fn put_listenbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<ListenBrainzConnection>>,
) -> JsonResult<Masked<ListenBrainzConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted ListenBrainz values (a masked token tests the
/// stored one). Rate limiting answers 429.
#[utoipa::path(
    post,
    path = "/api/v3/settings/listenbrainz/verify",
    request_body = ListenBrainzConnection,
    responses(
        (status = 200, description = "Verify verdict", body = VerifyConnectionResponse),
        (status = 429, description = "ListenBrainz is rate-limiting this server")
    )
)]
pub async fn verify_listenbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<ListenBrainzConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_listenbrainz(body).await.map(Json)
}

/// Read the YouTube connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/youtube",
    responses((status = 200, description = "YouTube connection", body = YouTubeConnection))
)]
pub async fn get_youtube(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<YouTubeConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the YouTube connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/youtube",
    request_body = YouTubeConnection,
    responses((status = 200, description = "Saved connection", body = YouTubeConnection))
)]
pub async fn put_youtube(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<YouTubeConnection>>,
) -> JsonResult<Masked<YouTubeConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted YouTube key (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/youtube/verify",
    request_body = YouTubeConnection,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_youtube(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<YouTubeConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_youtube(body).await.map(Json)
}

/// Read the events sources (keys masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/events",
    responses((status = 200, description = "Events sources", body = EventsSettings))
)]
pub async fn get_events(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<EventsSettings>> {
    settings.service().get_masked().map(Json)
}

/// Save the events sources (masked keys keep the stored ones) and kick
/// the sweep.
#[utoipa::path(
    put,
    path = "/api/v3/settings/events",
    request_body = EventsSettings,
    responses((status = 200, description = "Saved sources", body = EventsSettings))
)]
pub async fn put_events(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<EventsSettings>>,
) -> JsonResult<Masked<EventsSettings>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted Ticketmaster key (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/events/test-ticketmaster",
    request_body = EventsSettings,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn test_ticketmaster(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<EventsSettings>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_ticketmaster(body).await.map(Json)
}

/// Test the submitted Skiddle key (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/events/test-skiddle",
    request_body = EventsSettings,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn test_skiddle(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<EventsSettings>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_skiddle(body).await.map(Json)
}

/// Read the wrapped settings (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/wrapped",
    responses((status = 200, description = "Wrapped settings", body = WrappedSettings))
)]
pub async fn get_wrapped(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<WrappedSettings>> {
    settings.service().get_masked().map(Json)
}

/// Save the wrapped settings (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/wrapped",
    request_body = WrappedSettings,
    responses((status = 200, description = "Saved settings", body = WrappedSettings))
)]
pub async fn put_wrapped(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<WrappedSettings>>,
) -> JsonResult<Masked<WrappedSettings>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Read the OIDC connection (secret masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/oidc",
    responses((status = 200, description = "OIDC connection", body = OidcConnection))
)]
pub async fn get_oidc(State(settings): State<SettingsSetup>) -> JsonResult<Masked<OidcConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the OIDC connection (a masked secret keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/oidc",
    request_body = OidcConnection,
    responses((status = 200, description = "Saved connection", body = OidcConnection))
)]
pub async fn put_oidc(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<OidcConnection>>,
) -> JsonResult<Masked<OidcConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted OIDC issuer (fetches its discovery document).
#[utoipa::path(
    post,
    path = "/api/v3/settings/oidc/verify",
    request_body = OidcConnection,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_oidc(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<OidcConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings.service().verify_oidc(body).await.map(Json)
}

/// Read the slskd connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-client/config",
    responses((status = 200, description = "slskd connection", body = SlskdConnection))
)]
pub async fn get_slskd(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<SlskdConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the slskd connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-client/config",
    request_body = SlskdConnection,
    responses((status = 200, description = "Saved connection", body = SlskdConnection))
)]
pub async fn put_slskd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<SlskdConnection>>,
) -> JsonResult<Masked<SlskdConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted slskd values (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/download-client/test",
    request_body = SlskdConnection,
    responses((status = 200, description = "Verify verdict", body = TestConnectionResponse))
)]
pub async fn test_slskd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<SlskdConnection>>,
) -> JsonResult<TestConnectionResponse> {
    settings.service().verify_slskd(body).await.map(Json)
}

/// Read the SABnzbd connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/sabnzbd",
    responses((status = 200, description = "SABnzbd connection", body = SabnzbdConnection))
)]
pub async fn get_sabnzbd(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<SabnzbdConnection>> {
    settings.service().get_sabnzbd().map(Json)
}

/// Save the SABnzbd connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/sabnzbd",
    request_body = SabnzbdConnection,
    responses((status = 200, description = "Saved connection", body = SabnzbdConnection))
)]
pub async fn put_sabnzbd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<SabnzbdConnection>>,
) -> JsonResult<Masked<SabnzbdConnection>> {
    settings.service().save_sabnzbd(body).await.map(Json)
}

/// Test the submitted SABnzbd values (a masked key tests the stored
/// one). The submitted downloads mount is diagnosed, not the stored one.
#[utoipa::path(
    post,
    path = "/api/v3/settings/download-clients/sabnzbd/test",
    request_body = SabnzbdConnection,
    responses((status = 200, description = "Verify verdict", body = SabnzbdTestResponse))
)]
pub async fn test_sabnzbd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<SabnzbdConnection>>,
) -> JsonResult<SabnzbdTestResponse> {
    settings.service().verify_sabnzbd(body).await.map(Json)
}

/// Read the Prowlarr connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/prowlarr/config",
    responses((status = 200, description = "Prowlarr connection", body = ProwlarrConnection))
)]
pub async fn get_prowlarr(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Masked<ProwlarrConnection>> {
    settings.service().get_masked().map(Json)
}

/// Save the Prowlarr connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/prowlarr/config",
    request_body = ProwlarrConnection,
    responses((status = 200, description = "Saved connection", body = ProwlarrConnection))
)]
pub async fn put_prowlarr(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<ProwlarrConnection>>,
) -> JsonResult<Masked<ProwlarrConnection>> {
    settings.service().save_masked(body).await.map(Json)
}

/// Test the submitted Prowlarr values (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/prowlarr/test",
    request_body = ProwlarrConnection,
    responses((status = 200, description = "Verify verdict", body = ProwlarrTestResponse))
)]
pub async fn test_prowlarr(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<ProwlarrConnection>>,
) -> JsonResult<ProwlarrTestResponse> {
    settings.service().verify_prowlarr(body).await.map(Json)
}

// --- download policy --------------------------------------------------------------

/// Read the acquisition policy plus its recipe verdict.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/policy",
    responses((status = 200, description = "Acquisition policy", body = DownloadPolicyView))
)]
pub async fn get_policy(State(settings): State<SettingsSetup>) -> JsonResult<DownloadPolicyView> {
    settings.service().get_policy().map(Json)
}

/// Save the acquisition policy. Read-only verdict fields in the body are
/// ignored.
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/policy",
    request_body = DownloadPolicy,
    responses((status = 200, description = "Saved policy", body = DownloadPolicyView))
)]
pub async fn put_policy(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<DownloadPolicy>,
) -> JsonResult<DownloadPolicyView> {
    settings.service().save_policy(body).await.map(Json)
}

/// Safe policy summary for the settings page: the contract sentence
/// plus the source-mode label only.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/policy-summary",
    responses((status = 200, description = "Policy summary", body = PolicySummaryResponse))
)]
pub async fn get_policy_summary(
    State(settings): State<SettingsSetup>,
) -> JsonResult<PolicySummaryResponse> {
    settings.service().policy_summary().map(Json)
}

/// Impact preview of an unsaved policy body against persisted rows.
/// Needs the database; unwired states answer 503.
#[utoipa::path(
    post,
    path = "/api/v3/settings/download-clients/policy/impact",
    request_body = DownloadPolicy,
    responses(
        (status = 200, description = "Impact preview", body = PolicyImpactResponse),
        (status = 503, description = "Bucket counts are unwired")
    )
)]
pub async fn post_policy_impact(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<DownloadPolicy>,
) -> JsonResult<PolicyImpactResponse> {
    let buckets = settings
        .buckets()
        .ok_or_else(|| SettingsError::Unavailable {
            message: "Policy impact needs the database.".to_owned(),
        })?;
    settings
        .service()
        .policy_impact(buckets.as_ref(), body)
        .await
        .map(Json)
}

// --- indexers ---------------------------------------------------------------------

/// List the configured Newznab indexers (keys masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/indexers",
    responses((status = 200, description = "Configured indexers", body = Vec<NewznabIndexer>))
)]
pub async fn list_indexers(
    State(settings): State<SettingsSetup>,
) -> JsonResult<Vec<Masked<NewznabIndexer>>> {
    settings.service().list_indexers().map(Json)
}

/// Save one indexer (create when the id is blank, else update).
#[utoipa::path(
    post,
    path = "/api/v3/settings/indexers",
    request_body = NewznabIndexer,
    responses((status = 200, description = "Saved indexer id", body = IndexerSavedResponse))
)]
pub async fn create_indexer(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<NewznabIndexer>>,
) -> JsonResult<IndexerSavedResponse> {
    settings.service().save_indexer(body, None).await.map(Json)
}

/// Save one indexer; the path id wins over the body id.
#[utoipa::path(
    put,
    path = "/api/v3/settings/indexers/{id}",
    params(("id" = String, Path, description = "Indexer id")),
    request_body = NewznabIndexer,
    responses((status = 200, description = "Saved indexer id", body = IndexerSavedResponse))
)]
pub async fn update_indexer(
    State(settings): State<SettingsSetup>,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<Masked<NewznabIndexer>>,
) -> JsonResult<IndexerSavedResponse> {
    settings
        .service()
        .save_indexer(body, Some(id))
        .await
        .map(Json)
}

/// Delete one indexer. Unknown ids are a silent no-op.
#[utoipa::path(
    delete,
    path = "/api/v3/settings/indexers/{id}",
    params(("id" = String, Path, description = "Indexer id")),
    responses((status = 200, description = "Deleted", body = OperationResult))
)]
pub async fn delete_indexer(
    State(settings): State<SettingsSetup>,
    Path(id): Path<String>,
) -> JsonResult<OperationResult> {
    settings.service().delete_indexer(&id).await?;
    Ok(Json(OperationResult { success: true }))
}

/// Persist a dragged-card priority order.
#[utoipa::path(
    post,
    path = "/api/v3/settings/indexers/reorder",
    request_body = IndexerReorderRequest,
    responses((status = 200, description = "Reordered", body = OperationResult))
)]
pub async fn reorder_indexers(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<IndexerReorderRequest>,
) -> JsonResult<OperationResult> {
    settings
        .service()
        .reorder_indexers(body.ordered_ids)
        .await?;
    Ok(Json(OperationResult { success: true }))
}

/// Test one indexer's caps with the submitted URL and key (a masked key
/// tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/indexers/test",
    request_body = NewznabIndexer,
    responses((status = 200, description = "Caps verdict", body = IndexerTestResponse))
)]
pub async fn test_indexer(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<Masked<NewznabIndexer>>,
) -> JsonResult<IndexerTestResponse> {
    settings.service().verify_indexer(body).await.map(Json)
}

// --- musicbrainz + brainzmash -------------------------------------------------------

/// Read the settled MusicBrainz connection plus the transient pending echo.
#[utoipa::path(
    get,
    path = "/api/v3/settings/musicbrainz",
    responses((status = 200, description = "MusicBrainz settings", body = MusicBrainzSettingsView))
)]
pub async fn get_musicbrainz(
    State(settings): State<SettingsSetup>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings.lifecycle().get().map(Json)
}

/// Persist one normalized source change (BrainzMash moves through the
/// consent-bound flow, never a direct update).
#[utoipa::path(
    put,
    path = "/api/v3/settings/musicbrainz",
    request_body = MusicBrainzSettingsUpdate,
    responses((status = 200, description = "Saved settings", body = MusicBrainzSettingsView))
)]
pub async fn put_musicbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<MusicBrainzSettingsUpdate>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings.lifecycle().save_update(&body).await.map(Json)
}

/// Stage a BrainzMash proposal without probing upstream.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/brainzmash/stage",
    responses((status = 200, description = "Staged settings", body = MusicBrainzSettingsView))
)]
pub async fn stage_brainzmash(
    State(settings): State<SettingsSetup>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .stage()
        .await
        .map(|(view, _)| Json(view))
}

/// Record consent for the exact staged proposal. A stale proposal or
/// an outdated disclosure is a 409.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/brainzmash/consent",
    request_body = MusicBrainzBindingRequest,
    responses(
        (status = 200, description = "Consented settings", body = MusicBrainzSettingsView),
        (status = 409, description = "Proposal is stale or outdated")
    )
)]
pub async fn consent_brainzmash(
    State(settings): State<SettingsSetup>,
    Extension(admin): Extension<AdminUser>,
    ValidJson(body): ValidJson<MusicBrainzBindingRequest>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .consent(&body, &admin.user_id)
        .map(Json)
}

/// Verify a BrainzMash binding or probe a plain tier. A failed probe is
/// a 502; alternative probes are a 409 while BrainzMash is active.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/verify",
    request_body = MusicBrainzVerifyRequest,
    responses(
        (status = 200, description = "Verified settings", body = MusicBrainzSettingsView),
        (status = 409, description = "Proposal is stale, or BrainzMash is active"),
        (status = 502, description = "Probe failed")
    )
)]
pub async fn verify_musicbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<MusicBrainzVerifyRequest>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .verify(settings.service().probes(), &body)
        .await
        .map(Json)
}

/// Promote the exact verified proposal to the active binding.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/activate",
    request_body = MusicBrainzBindingRequest,
    responses(
        (status = 200, description = "Activated settings", body = MusicBrainzSettingsView),
        (status = 409, description = "Proposal is stale, unconsented, or unverified")
    )
)]
pub async fn activate_brainzmash(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<MusicBrainzBindingRequest>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings.lifecycle().activate(&body).await.map(Json)
}

// --- library management -------------------------------------------------------------

/// Read the Library Management settings with their revision.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library-management",
    responses((status = 200, description = "Library Management settings", body = LibraryManagementSettingsResponse))
)]
pub async fn get_library_management(
    State(settings): State<SettingsSetup>,
) -> JsonResult<LibraryManagementSettingsResponse> {
    settings
        .management(|service| service.get_settings())
        .await
        .map(Json)
}

/// Save the Library Management settings under CAS. Giving an automatic
/// root new write scope needs a current confirmed dry run.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library-management",
    request_body = LibraryManagementSaveRequest,
    responses(
        (status = 200, description = "Saved settings", body = LibraryManagementSettingsResponse),
        (status = 400, description = "Invalid settings, or a dry run is required"),
        (status = 409, description = "Expected revision is stale")
    )
)]
pub async fn put_library_management(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryManagementSaveRequest>,
) -> JsonResult<LibraryManagementSettingsResponse> {
    settings
        .management(move |service| {
            service.save_settings(body.settings, &body.expected_settings_revision)
        })
        .await
        .map(Json)
}

/// Dry-run activation health for the active automatic roots.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library-management/activation-health",
    responses((status = 200, description = "Activation health", body = ActivationHealth))
)]
pub async fn get_library_management_activation_health(
    State(settings): State<SettingsSetup>,
) -> JsonResult<ActivationHealth> {
    settings
        .management(|service| service.activation_health())
        .await
        .map(Json)
}

/// Classify a candidate without saving it.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/impact",
    request_body = LibraryManagementSettingsImpactRequest,
    responses((status = 200, description = "Change impact", body = ChangeImpact))
)]
pub async fn preview_library_management_impact(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryManagementSettingsImpactRequest>,
) -> JsonResult<ChangeImpact> {
    settings
        .management(move |service| {
            service.preview_impact(body.settings, body.expected_settings_revision.as_deref())
        })
        .await
        .map(Json)
}

/// Validate a candidate: the same check and verdict as the impact
/// preview (v2 serves both paths).
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/validate",
    request_body = LibraryManagementSettingsImpactRequest,
    responses((status = 200, description = "Change impact", body = ChangeImpact))
)]
pub async fn validate_library_management(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryManagementSettingsImpactRequest>,
) -> JsonResult<ChangeImpact> {
    preview_library_management_impact(State(settings), ValidJson(body)).await
}

/// Read one profile.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library-management/profiles/{profile_id}",
    params(("profile_id" = String, Path, description = "Profile id")),
    responses((status = 200, description = "Profile", body = LibraryManagementProfile))
)]
pub async fn get_library_management_profile(
    State(settings): State<SettingsSetup>,
    Path(profile_id): Path<String>,
) -> JsonResult<LibraryManagementProfile> {
    settings
        .management(move |service| service.get_profile(&profile_id))
        .await
        .map(Json)
}

/// Create a profile as a copy of the default profile.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/profiles",
    request_body = LibraryManagementProfileCreateRequest,
    responses((status = 200, description = "Created profile", body = LibraryManagementProfileMutationResponse))
)]
pub async fn create_library_management_profile(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryManagementProfileCreateRequest>,
) -> JsonResult<LibraryManagementProfileMutationResponse> {
    settings
        .management(move |service| {
            service.create_profile(
                &body.name,
                &body.description,
                &body.expected_settings_revision,
            )
        })
        .await
        .map(Json)
}

/// Copy one profile under a new name.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/profiles/{profile_id}/copy",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileCopyRequest,
    responses((status = 200, description = "Copied profile", body = LibraryManagementProfileMutationResponse))
)]
pub async fn copy_library_management_profile(
    State(settings): State<SettingsSetup>,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileCopyRequest>,
) -> JsonResult<LibraryManagementProfileMutationResponse> {
    settings
        .management(move |service| {
            service.copy_profile(&profile_id, &body.name, &body.expected_settings_revision)
        })
        .await
        .map(Json)
}

/// Replace one profile. The body id must match the path id.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library-management/profiles/{profile_id}",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileUpdateRequest,
    responses((status = 200, description = "Saved profile", body = LibraryManagementProfileMutationResponse))
)]
pub async fn update_library_management_profile(
    State(settings): State<SettingsSetup>,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileUpdateRequest>,
) -> JsonResult<LibraryManagementProfileMutationResponse> {
    settings
        .management(move |service| {
            service.update_profile(&profile_id, body.profile, &body.expected_settings_revision)
        })
        .await
        .map(Json)
}

/// Delete one custom profile that is neither the default nor assigned.
#[utoipa::path(
    delete,
    path = "/api/v3/settings/library-management/profiles/{profile_id}",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileDeleteRequest,
    responses((status = 200, description = "Settings after the delete", body = LibraryManagementSettingsResponse))
)]
pub async fn delete_library_management_profile(
    State(settings): State<SettingsSetup>,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileDeleteRequest>,
) -> JsonResult<LibraryManagementSettingsResponse> {
    settings
        .management(move |service| {
            service.delete_profile(&profile_id, &body.expected_settings_revision)
        })
        .await
        .map(Json)
}

/// Which groups of a preset-tracking profile differ from the preset.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library-management/profiles/{profile_id}/preset-diff",
    params(("profile_id" = String, Path, description = "Profile id")),
    responses((status = 200, description = "Preset diff", body = PresetDiff))
)]
pub async fn get_library_management_preset_diff(
    State(settings): State<SettingsSetup>,
    Path(profile_id): Path<String>,
) -> JsonResult<PresetDiff> {
    settings
        .management(move |service| service.preset_diff(&profile_id))
        .await
        .map(Json)
}

/// Export one profile as a share bundle.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/profiles/{profile_id}/export",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileExportRequest,
    responses((status = 200, description = "Share bundle", body = LibraryManagementProfileExportResponse))
)]
pub async fn export_library_management_profile(
    State(settings): State<SettingsSetup>,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileExportRequest>,
) -> JsonResult<LibraryManagementProfileExportResponse> {
    settings
        .management(move |service| {
            service.export_profile(&profile_id, &body.expected_settings_revision)
        })
        .await
        .map(Json)
}

/// Preview importing a shared profile (document or share code).
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/profile-imports/preview",
    request_body = LibraryManagementProfileImportPreviewRequest,
    responses((status = 200, description = "Import preview", body = LibraryManagementProfileImportPreviewResponse))
)]
pub async fn preview_library_management_profile_import(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryManagementProfileImportPreviewRequest>,
) -> JsonResult<LibraryManagementProfileImportPreviewResponse> {
    settings
        .management(move |service| {
            service.preview_profile_import(&body.content, &body.expected_settings_revision)
        })
        .await
        .map(Json)
}

/// Import a reviewed shared profile.
#[utoipa::path(
    post,
    path = "/api/v3/settings/library-management/profile-imports",
    request_body = LibraryManagementProfileImportRequest,
    responses(
        (status = 200, description = "Imported profile", body = LibraryManagementProfileImportResponse),
        (status = 409, description = "Settings or the bundle changed since review")
    )
)]
pub async fn import_library_management_profile(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryManagementProfileImportRequest>,
) -> JsonResult<LibraryManagementProfileImportResponse> {
    settings
        .management(move |service| {
            service.import_profile(
                &body.content,
                &body.reviewed_bundle_hash,
                &body.name,
                &body.expected_settings_revision,
            )
        })
        .await
        .map(Json)
}

// --- dropped sections (410) ------------------------------------------------------

/// The legacy catalog-sync section is gone (one-shot import only now).
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/sync",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_library_sync() -> JsonResult<serde_json::Value> {
    super::validator::ensure_kept_section("library_sync_settings")?;
    Ok(Json(serde_json::Value::Null))
}

/// PUT twin of the dropped catalog-sync route: gone, not moved.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library/sync",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_library_sync_put() -> JsonResult<serde_json::Value> {
    dropped_library_sync().await
}

// --- section prefs (per-user) ------------------------------------------------------

/// Read the caller's section visibility prefs (all three pages).
#[utoipa::path(
    get,
    path = "/api/v3/me/section-prefs",
    responses((status = 200, description = "Section prefs", body = SectionPrefsResponse))
)]
pub async fn get_section_prefs(
    State(settings): State<SettingsSetup>,
    Extension(session): Extension<CurrentSession>,
) -> JsonResult<SectionPrefsResponse> {
    let prefs = settings.prefs().ok_or_else(|| SettingsError::Unavailable {
        message: "Section prefs need the database.".to_owned(),
    })?;
    let lastfm_master = settings
        .service()
        .get_masked::<LastFmSettings>()?
        .into_inner()
        .enabled;
    super::section_prefs::full_response(
        prefs.store.as_ref(),
        prefs.links.as_ref(),
        &*settings.service().ids,
        &session.user_id,
        lastfm_master,
    )
    .await
    .map(Json)
}

/// Replace one page of the caller's section prefs. Unknown pages and
/// unknown keys are a 400; only the updated page is returned.
#[utoipa::path(
    put,
    path = "/api/v3/me/section-prefs",
    request_body = SectionPrefsUpdate,
    responses(
        (status = 200, description = "Updated page", body = SectionPrefsResponse),
        (status = 503, description = "Section prefs are unwired")
    )
)]
pub async fn put_section_prefs(
    State(settings): State<SettingsSetup>,
    Extension(session): Extension<CurrentSession>,
    ValidJson(body): ValidJson<SectionPrefsUpdate>,
) -> JsonResult<SectionPrefsResponse> {
    let prefs = settings.prefs().ok_or_else(|| SettingsError::Unavailable {
        message: "Section prefs need the database.".to_owned(),
    })?;
    let lastfm_master = settings
        .service()
        .get_masked::<LastFmSettings>()?
        .into_inner()
        .enabled;
    let items = super::section_prefs::save_page(
        prefs.store.as_ref(),
        prefs.links.as_ref(),
        &*settings.service().ids,
        &session.user_id,
        &body,
        lastfm_master,
    )
    .await?;
    Ok(Json(SectionPrefsResponse {
        pages: BTreeMap::from([(body.page.clone(), items)]),
    }))
}
