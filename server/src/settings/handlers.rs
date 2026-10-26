//! Thin settings handlers for the routes the section registry does not
//! build: verify probes, sections with their own wire shapes or checks,
//! indexers, the library policy, Library Management and the section
//! prefs. Each answers one route: decode, call the service, return its
//! typed result. Secret values cross the wire as [`Masked`], so a
//! decrypted section cannot be returned by accident.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
};

use super::models::{
    DownloadPolicyView, FrontendCacheTTLs, IndexerReorderRequest, IndexerSavedResponse,
    IndexerTestResponse, JellyfinVerifyResponse, LibraryManagementProfileCopyRequest,
    LibraryManagementProfileCreateRequest, LibraryManagementProfileDeleteRequest,
    LibraryManagementProfileExportRequest, LibraryManagementProfileImportPreviewRequest,
    LibraryManagementProfileImportRequest, LibraryManagementProfileUpdateRequest,
    LibraryManagementSaveRequest, LibraryManagementSettingsImpactRequest, LibraryPathQuery,
    LibraryPathRequest, LibraryPolicyApplyRequest, LibraryPolicyImpactRequest,
    LibraryRestorableRootsResponse, LibraryRestoreRootsRequest, LibraryScanScheduleResponse,
    LibrarySettingsSaveRequest, MusicBrainzBindingRequest, MusicBrainzSettingsUpdate,
    MusicBrainzSettingsView, MusicBrainzVerifyRequest, OperationResult, PlexLibrarySectionInfo,
    PlexVerifyResponse, PolicySummaryResponse, ProwlarrTestResponse, SabnzbdTestResponse,
    SectionPrefsResponse, SectionPrefsUpdate, SourcePriorityOrder, TestConnectionResponse,
    UsenetSearchBackend, VerifyConnectionResponse,
};
use super::wiring::SettingsSetup;
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::runtime_config::Masked;
use droppedneedle_kernel::runtime_config::secret_sections::{
    EventsSettings, JellyfinConnection, LastFmSettings, ListenBrainzConnection,
    NavidromeConnection, NewznabIndexer, OidcConnection, PlexConnection, ProwlarrConnection,
    SabnzbdConnection, SlskdConnection, YouTubeConnection,
};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, LibraryManagementProfile, LibraryScanSchedule, SecuritySettings,
};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use droppedneedle_library::management::settings::{ActivationHealth, ChangeImpact, PresetDiff};
use droppedneedle_library::settings_models::{
    LibraryManagementProfileExportResponse, LibraryManagementProfileImportPreviewResponse,
    LibraryManagementProfileImportResponse, LibraryManagementProfileMutationResponse,
    LibraryManagementSettingsResponse, LibraryPathMappingReport, LibraryPolicyApplyPreviewResponse,
    LibraryPolicyImpactResponse, LibraryPolicyTreeResponse, LibrarySettingsResponse,
};

type JsonResult<T> = Result<Json<T>, ApiError>;

// --- schedule, security, source choices ------------------------------------------

/// Read the scan schedule plus the server timezone label.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/schedule",
    responses((status = 200, description = "Scan schedule", body = LibraryScanScheduleResponse))
)]
pub async fn get_schedule(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<LibraryScanScheduleResponse> {
    let schedule = settings.service().get_schedule()?;
    Ok(Json(settings.with_last_scan(schedule).await))
}

/// Save the scan schedule.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/library/schedule",
    request_body = LibraryScanSchedule,
    responses((status = 200, description = "Saved schedule", body = LibraryScanScheduleResponse))
)]
pub async fn put_schedule(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryScanSchedule>,
) -> JsonResult<LibraryScanScheduleResponse> {
    let schedule = settings.service().save_schedule(body).await?;
    Ok(Json(settings.with_last_scan(schedule).await))
}

/// Check the submitted HIBP hash-list path (must exist and start with
/// a `40-char-SHA1:count` line).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/security/verify-hibp",
    request_body = SecuritySettings,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_hibp(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<SecuritySettings>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_hibp(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Read the acquisition source try-order.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/download-clients/source-priority",
    responses((status = 200, description = "Source try-order", body = SourcePriorityOrder))
)]
pub async fn get_source_priority(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<SourcePriorityOrder> {
    settings
        .service()
        .get_source_priority()
        .map(Json)
        .map_err(ApiError::from)
}

/// Save the acquisition source try-order.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/download-clients/source-priority",
    request_body = SourcePriorityOrder,
    responses((status = 200, description = "Saved try-order", body = SourcePriorityOrder))
)]
pub async fn put_source_priority(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<SourcePriorityOrder>,
) -> JsonResult<SourcePriorityOrder> {
    settings
        .service()
        .save_source_priority(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Read the active Usenet search backend.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/indexers/search-backend",
    responses((status = 200, description = "Search backend", body = UsenetSearchBackend))
)]
pub async fn get_search_backend(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<UsenetSearchBackend> {
    settings
        .service()
        .get_usenet_backend()
        .map(Json)
        .map_err(ApiError::from)
}

/// Save the active Usenet search backend. Unknown values are a 400.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/indexers/search-backend",
    request_body = UsenetSearchBackend,
    responses((status = 200, description = "Saved backend", body = UsenetSearchBackend))
)]
pub async fn put_search_backend(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<UsenetSearchBackend>,
) -> JsonResult<UsenetSearchBackend> {
    settings
        .service()
        .save_usenet_backend(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// --- library settings -------------------------------------------------------------

/// Read the typed library settings with the policy revision.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library",
    responses((status = 200, description = "Library settings", body = LibrarySettingsResponse))
)]
pub async fn get_library(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .service()
        .get_library()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Save the typed library settings. The expected revision must match
/// the stored one or the save is a 409. Dropping every root while the
/// catalog holds tracks is a 400.
#[utoipa::path(
    put,
    tag = "settings",
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
    _caller: Admin,
    ValidJson(body): ValidJson<LibrarySettingsSaveRequest>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .library_policy()
        .save(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Add one library root path. The path must be a directory on this
/// machine; re-adding is a silent no-op.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library/paths",
    request_body = LibraryPathRequest,
    responses((status = 200, description = "Library settings", body = LibrarySettingsResponse))
)]
pub async fn add_library_path(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryPathRequest>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .service()
        .add_library_path(&body.path)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Remove every library root at one path. Unknown paths are a silent
/// no-op; removing the last root while the catalog holds tracks is a 400.
#[utoipa::path(
    delete,
    tag = "settings",
    path = "/api/v3/settings/library/paths",
    params(("path" = String, Query, description = "Directory to remove")),
    responses(
        (status = 200, description = "Library settings", body = LibrarySettingsResponse),
        (status = 400, description = "Removing the last root would orphan the catalog")
    )
)]
pub async fn remove_library_path(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidQuery(query): ValidQuery<LibraryPathQuery>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .library_policy()
        .remove_path(&query.path)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The saved roots and their path rules as a tree, with catalog file
/// counts per node.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/policy-tree",
    responses((status = 200, description = "Policy tree", body = LibraryPolicyTreeResponse))
)]
pub async fn get_library_policy_tree(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<LibraryPolicyTreeResponse> {
    settings
        .library_policy()
        .policy_tree()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Preview what saving candidate library settings would change. Nothing
/// is saved; a stale expected revision only sets `stale`.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library/policy-impact",
    request_body = LibraryPolicyImpactRequest,
    responses(
        (status = 200, description = "Impact preview", body = LibraryPolicyImpactResponse),
        (status = 400, description = "Candidate settings are invalid")
    )
)]
pub async fn preview_library_policy_impact(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryPolicyImpactRequest>,
) -> JsonResult<LibraryPolicyImpactResponse> {
    settings
        .library_policy()
        .preview_impact(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Preview a reconcile of saved policy scopes: how many catalog files
/// it would revisit.
#[utoipa::path(
    post,
    tag = "settings",
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
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryPolicyApplyRequest>,
) -> JsonResult<LibraryPolicyApplyPreviewResponse> {
    settings
        .library_policy()
        .preview_apply(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Roots the catalog still holds tracks for that the settings no longer
/// list.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/restorable-roots",
    responses(
        (status = 200, description = "Restorable roots", body = LibraryRestorableRootsResponse),
        (status = 503, description = "Catalog reads are unwired")
    )
)]
pub async fn get_restorable_library_roots(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<LibraryRestorableRootsResponse> {
    settings
        .library_policy()
        .restorable_roots()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Put every removed root back into the settings.
#[utoipa::path(
    post,
    tag = "settings",
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
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryRestoreRootsRequest>,
) -> JsonResult<LibrarySettingsResponse> {
    settings
        .library_policy()
        .restore_roots(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Dry run: map every catalog file path to a saved root.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/path-mapping",
    responses(
        (status = 200, description = "Path mapping report", body = LibraryPathMappingReport),
        (status = 503, description = "Catalog reads are unwired")
    )
)]
pub async fn get_library_path_mapping(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<LibraryPathMappingReport> {
    settings
        .library_policy()
        .path_mapping()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// --- cache TTLs ---------------------------------------------------------------------

/// Read the advanced settings the web app reads at start.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/cache-ttls",
    responses((status = 200, description = "Web app settings", body = FrontendCacheTTLs))
)]
pub async fn get_cache_ttls(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<FrontendCacheTTLs> {
    settings
        .service()
        .get_cache_ttls()
        .map(Json)
        .map_err(ApiError::from)
}

// --- connection probes ---------------------------------------------------------------

/// Test the submitted Jellyfin values (a masked key tests the stored
/// one). The user list rides along on success for the admin picker.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/jellyfin/verify",
    request_body = JellyfinConnection,
    responses((status = 200, description = "Verify verdict", body = JellyfinVerifyResponse))
)]
pub async fn verify_jellyfin(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<JellyfinConnection>>,
) -> JsonResult<JellyfinVerifyResponse> {
    settings
        .service()
        .verify_jellyfin(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted Navidrome values (a masked password tests the
/// stored one).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/navidrome/verify",
    request_body = NavidromeConnection,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_navidrome(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<NavidromeConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_navidrome(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted Plex values (a masked token tests the stored
/// one). Music libraries ride along on success.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/plex/verify",
    request_body = PlexConnection,
    responses((status = 200, description = "Verify verdict", body = PlexVerifyResponse))
)]
pub async fn verify_plex(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<PlexConnection>>,
) -> JsonResult<PlexVerifyResponse> {
    settings
        .service()
        .verify_plex(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// List Plex music libraries over the stored connection. Unconfigured
/// is a 400; an unreachable Plex is a 502.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/plex/libraries",
    responses(
        (status = 200, description = "Music libraries", body = Vec<PlexLibrarySectionInfo>),
        (status = 400, description = "Plex is not configured"),
        (status = 502, description = "Plex is unreachable")
    )
)]
pub async fn get_plex_libraries(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<Vec<PlexLibrarySectionInfo>> {
    settings
        .service()
        .plex_libraries()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted ListenBrainz values (a masked token tests the
/// stored one). Rate limiting answers 429.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/listenbrainz/verify",
    request_body = ListenBrainzConnection,
    responses(
        (status = 200, description = "Verify verdict", body = VerifyConnectionResponse),
        (status = 429, description = "ListenBrainz is rate-limiting this server")
    )
)]
pub async fn verify_listenbrainz(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<ListenBrainzConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_listenbrainz(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted YouTube key (a masked key tests the stored one).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/youtube/verify",
    request_body = YouTubeConnection,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_youtube(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<YouTubeConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_youtube(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted Ticketmaster key (a masked key tests the stored one).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/events/test-ticketmaster",
    request_body = EventsSettings,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn test_ticketmaster(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<EventsSettings>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_ticketmaster(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted Skiddle key (a masked key tests the stored one).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/events/test-skiddle",
    request_body = EventsSettings,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn test_skiddle(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<EventsSettings>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_skiddle(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted OIDC issuer (fetches its discovery document).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/oidc/verify",
    request_body = OidcConnection,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_oidc(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<OidcConnection>>,
) -> JsonResult<VerifyConnectionResponse> {
    settings
        .service()
        .verify_oidc(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted slskd values (a masked key tests the stored one).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/download-client/test",
    request_body = SlskdConnection,
    responses((status = 200, description = "Verify verdict", body = TestConnectionResponse))
)]
pub async fn test_slskd(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<SlskdConnection>>,
) -> JsonResult<TestConnectionResponse> {
    settings
        .service()
        .verify_slskd(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Read the SABnzbd connection (key masked unless unset).
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/download-clients/sabnzbd",
    responses((status = 200, description = "SABnzbd connection", body = SabnzbdConnection))
)]
pub async fn get_sabnzbd(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<Masked<SabnzbdConnection>> {
    settings
        .service()
        .get_sabnzbd()
        .map(Json)
        .map_err(ApiError::from)
}

/// Save the SABnzbd connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/download-clients/sabnzbd",
    request_body = SabnzbdConnection,
    responses((status = 200, description = "Saved connection", body = SabnzbdConnection))
)]
pub async fn put_sabnzbd(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<SabnzbdConnection>>,
) -> JsonResult<Masked<SabnzbdConnection>> {
    settings
        .service()
        .save_sabnzbd(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted SABnzbd values (a masked key tests the stored
/// one). The submitted downloads mount is diagnosed, not the stored one.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/download-clients/sabnzbd/test",
    request_body = SabnzbdConnection,
    responses((status = 200, description = "Verify verdict", body = SabnzbdTestResponse))
)]
pub async fn test_sabnzbd(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<SabnzbdConnection>>,
) -> JsonResult<SabnzbdTestResponse> {
    settings
        .service()
        .verify_sabnzbd(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Test the submitted Prowlarr values (a masked key tests the stored one).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/prowlarr/test",
    request_body = ProwlarrConnection,
    responses((status = 200, description = "Verify verdict", body = ProwlarrTestResponse))
)]
pub async fn test_prowlarr(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<ProwlarrConnection>>,
) -> JsonResult<ProwlarrTestResponse> {
    settings
        .service()
        .verify_prowlarr(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// --- download policy --------------------------------------------------------------

/// Read the acquisition policy plus its recipe verdict.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/download-clients/policy",
    responses((status = 200, description = "Acquisition policy", body = DownloadPolicyView))
)]
pub async fn get_policy(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<DownloadPolicyView> {
    let view = settings.service().get_policy()?;
    Ok(Json(settings.with_bin_warning(view).await))
}

/// Save the acquisition policy. Read-only verdict fields in the body are
/// ignored.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/download-clients/policy",
    request_body = DownloadPolicy,
    responses((status = 200, description = "Saved policy", body = DownloadPolicyView))
)]
pub async fn put_policy(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<DownloadPolicy>,
) -> JsonResult<DownloadPolicyView> {
    let view = settings.service().save_policy(body).await?;
    Ok(Json(settings.with_bin_warning(view).await))
}

/// Safe policy summary for the settings page: the contract sentence
/// plus the source-mode label only.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/download-clients/policy-summary",
    responses((status = 200, description = "Policy summary", body = PolicySummaryResponse))
)]
pub async fn get_policy_summary(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<PolicySummaryResponse> {
    settings
        .service()
        .policy_summary()
        .map(Json)
        .map_err(ApiError::from)
}

// --- indexers ---------------------------------------------------------------------

/// List the configured Newznab indexers (keys masked unless unset).
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/indexers",
    responses((status = 200, description = "Configured indexers", body = Vec<NewznabIndexer>))
)]
pub async fn list_indexers(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<Vec<Masked<NewznabIndexer>>> {
    settings
        .service()
        .list_indexers()
        .map(Json)
        .map_err(ApiError::from)
}

/// Save one indexer (create when the id is blank, else update).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/indexers",
    request_body = NewznabIndexer,
    responses((status = 200, description = "Saved indexer id", body = IndexerSavedResponse))
)]
pub async fn create_indexer(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<NewznabIndexer>>,
) -> JsonResult<IndexerSavedResponse> {
    settings
        .service()
        .save_indexer(body, None)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Save one indexer; the path id wins over the body id.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/indexers/{id}",
    params(("id" = String, Path, description = "Indexer id")),
    request_body = NewznabIndexer,
    responses((status = 200, description = "Saved indexer id", body = IndexerSavedResponse))
)]
pub async fn update_indexer(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<Masked<NewznabIndexer>>,
) -> JsonResult<IndexerSavedResponse> {
    settings
        .service()
        .save_indexer(body, Some(id))
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Delete one indexer. Unknown ids are a silent no-op.
#[utoipa::path(
    delete,
    tag = "settings",
    path = "/api/v3/settings/indexers/{id}",
    params(("id" = String, Path, description = "Indexer id")),
    responses((status = 200, description = "Deleted", body = OperationResult))
)]
pub async fn delete_indexer(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(id): Path<String>,
) -> JsonResult<OperationResult> {
    settings.service().delete_indexer(&id).await?;
    Ok(Json(OperationResult { success: true }))
}

/// Persist a dragged-card priority order.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/indexers/reorder",
    request_body = IndexerReorderRequest,
    responses((status = 200, description = "Reordered", body = OperationResult))
)]
pub async fn reorder_indexers(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
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
    tag = "settings",
    path = "/api/v3/settings/indexers/test",
    request_body = NewznabIndexer,
    responses((status = 200, description = "Caps verdict", body = IndexerTestResponse))
)]
pub async fn test_indexer(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Masked<NewznabIndexer>>,
) -> JsonResult<IndexerTestResponse> {
    settings
        .service()
        .verify_indexer(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// --- musicbrainz + brainzmash -------------------------------------------------------

/// Read the settled MusicBrainz connection plus the transient pending echo.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/musicbrainz",
    responses((status = 200, description = "MusicBrainz settings", body = MusicBrainzSettingsView))
)]
pub async fn get_musicbrainz(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<MusicBrainzSettingsView> {
    settings.lifecycle().get().map(Json).map_err(ApiError::from)
}

/// Persist one normalized source change (BrainzMash moves through the
/// consent-bound flow, never a direct update).
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/musicbrainz",
    request_body = MusicBrainzSettingsUpdate,
    responses((status = 200, description = "Saved settings", body = MusicBrainzSettingsView))
)]
pub async fn put_musicbrainz(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<MusicBrainzSettingsUpdate>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .save_update(&body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Stage a BrainzMash proposal without probing upstream.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/musicbrainz/brainzmash/stage",
    responses((status = 200, description = "Staged settings", body = MusicBrainzSettingsView))
)]
pub async fn stage_brainzmash(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .stage()
        .await
        .map(|(view, _)| Json(view))
        .map_err(ApiError::from)
}

/// Record consent for the exact staged proposal. A stale proposal or
/// an outdated disclosure is a 409.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/musicbrainz/brainzmash/consent",
    request_body = MusicBrainzBindingRequest,
    responses(
        (status = 200, description = "Consented settings", body = MusicBrainzSettingsView),
        (status = 409, description = "Proposal is stale or outdated")
    )
)]
pub async fn consent_brainzmash(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<MusicBrainzBindingRequest>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .consent(&body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Verify a BrainzMash binding or probe a plain tier. A failed probe is
/// a 502; alternative probes are a 409 while BrainzMash is active.
#[utoipa::path(
    post,
    tag = "settings",
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
    _caller: Admin,
    ValidJson(body): ValidJson<MusicBrainzVerifyRequest>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .verify(settings.service().probes(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Promote the exact verified proposal to the active binding.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/musicbrainz/activate",
    request_body = MusicBrainzBindingRequest,
    responses(
        (status = 200, description = "Activated settings", body = MusicBrainzSettingsView),
        (status = 409, description = "Proposal is stale, unconsented, or unverified")
    )
)]
pub async fn activate_brainzmash(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<MusicBrainzBindingRequest>,
) -> JsonResult<MusicBrainzSettingsView> {
    settings
        .lifecycle()
        .activate(&body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// --- library management -------------------------------------------------------------

/// Read the Library Management settings with their revision.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library-management",
    responses((status = 200, description = "Library Management settings", body = LibraryManagementSettingsResponse))
)]
pub async fn get_library_management(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<LibraryManagementSettingsResponse> {
    settings
        .management(|service| service.get_settings())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Save the Library Management settings under CAS. Giving an automatic
/// root new write scope needs a current confirmed dry run.
#[utoipa::path(
    put,
    tag = "settings",
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
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryManagementSaveRequest>,
) -> JsonResult<LibraryManagementSettingsResponse> {
    settings
        .management(move |service| {
            service.save_settings(body.settings, &body.expected_settings_revision)
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Dry-run activation health for the active automatic roots.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library-management/activation-health",
    responses((status = 200, description = "Activation health", body = ActivationHealth))
)]
pub async fn get_library_management_activation_health(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
) -> JsonResult<ActivationHealth> {
    settings
        .management(|service| service.activation_health())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Classify a candidate without saving it.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/impact",
    request_body = LibraryManagementSettingsImpactRequest,
    responses((status = 200, description = "Change impact", body = ChangeImpact))
)]
pub async fn preview_library_management_impact(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryManagementSettingsImpactRequest>,
) -> JsonResult<ChangeImpact> {
    settings
        .management(move |service| {
            service.preview_impact(body.settings, body.expected_settings_revision.as_deref())
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Validate a candidate: the same check and verdict as the impact
/// preview (v2 serves both paths).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/validate",
    request_body = LibraryManagementSettingsImpactRequest,
    responses((status = 200, description = "Change impact", body = ChangeImpact))
)]
pub async fn validate_library_management(
    State(settings): State<SettingsSetup>,
    caller: Admin,
    ValidJson(body): ValidJson<LibraryManagementSettingsImpactRequest>,
) -> JsonResult<ChangeImpact> {
    preview_library_management_impact(State(settings), caller, ValidJson(body)).await
}

/// Read one profile.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles/{profile_id}",
    params(("profile_id" = String, Path, description = "Profile id")),
    responses((status = 200, description = "Profile", body = LibraryManagementProfile))
)]
pub async fn get_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(profile_id): Path<String>,
) -> JsonResult<LibraryManagementProfile> {
    settings
        .management(move |service| service.get_profile(&profile_id))
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Create a profile as a copy of the default profile.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles",
    request_body = LibraryManagementProfileCreateRequest,
    responses((status = 200, description = "Created profile", body = LibraryManagementProfileMutationResponse))
)]
pub async fn create_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
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
        .map_err(ApiError::from)
}

/// Copy one profile under a new name.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles/{profile_id}/copy",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileCopyRequest,
    responses((status = 200, description = "Copied profile", body = LibraryManagementProfileMutationResponse))
)]
pub async fn copy_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileCopyRequest>,
) -> JsonResult<LibraryManagementProfileMutationResponse> {
    settings
        .management(move |service| {
            service.copy_profile(&profile_id, &body.name, &body.expected_settings_revision)
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Replace one profile. The body id must match the path id.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles/{profile_id}",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileUpdateRequest,
    responses((status = 200, description = "Saved profile", body = LibraryManagementProfileMutationResponse))
)]
pub async fn update_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileUpdateRequest>,
) -> JsonResult<LibraryManagementProfileMutationResponse> {
    settings
        .management(move |service| {
            service.update_profile(&profile_id, body.profile, &body.expected_settings_revision)
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Delete one custom profile that is neither the default nor assigned.
#[utoipa::path(
    delete,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles/{profile_id}",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileDeleteRequest,
    responses((status = 200, description = "Settings after the delete", body = LibraryManagementSettingsResponse))
)]
pub async fn delete_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileDeleteRequest>,
) -> JsonResult<LibraryManagementSettingsResponse> {
    settings
        .management(move |service| {
            service.delete_profile(&profile_id, &body.expected_settings_revision)
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Which groups of a preset-tracking profile differ from the preset.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles/{profile_id}/preset-diff",
    params(("profile_id" = String, Path, description = "Profile id")),
    responses((status = 200, description = "Preset diff", body = PresetDiff))
)]
pub async fn get_library_management_preset_diff(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(profile_id): Path<String>,
) -> JsonResult<PresetDiff> {
    settings
        .management(move |service| service.preset_diff(&profile_id))
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Export one profile as a share bundle.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/profiles/{profile_id}/export",
    params(("profile_id" = String, Path, description = "Profile id")),
    request_body = LibraryManagementProfileExportRequest,
    responses((status = 200, description = "Share bundle", body = LibraryManagementProfileExportResponse))
)]
pub async fn export_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    Path(profile_id): Path<String>,
    ValidJson(body): ValidJson<LibraryManagementProfileExportRequest>,
) -> JsonResult<LibraryManagementProfileExportResponse> {
    settings
        .management(move |service| {
            service.export_profile(&profile_id, &body.expected_settings_revision)
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Preview importing a shared profile (document or share code).
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/profile-imports/preview",
    request_body = LibraryManagementProfileImportPreviewRequest,
    responses((status = 200, description = "Import preview", body = LibraryManagementProfileImportPreviewResponse))
)]
pub async fn preview_library_management_profile_import(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<LibraryManagementProfileImportPreviewRequest>,
) -> JsonResult<LibraryManagementProfileImportPreviewResponse> {
    settings
        .management(move |service| {
            service.preview_profile_import(&body.content, &body.expected_settings_revision)
        })
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Import a reviewed shared profile.
#[utoipa::path(
    post,
    tag = "settings",
    path = "/api/v3/settings/library-management/profile-imports",
    request_body = LibraryManagementProfileImportRequest,
    responses(
        (status = 200, description = "Imported profile", body = LibraryManagementProfileImportResponse),
        (status = 409, description = "Settings or the bundle changed since review")
    )
)]
pub async fn import_library_management_profile(
    State(settings): State<SettingsSetup>,
    _caller: Admin,
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
        .map_err(ApiError::from)
}

// --- dropped sections (410) ------------------------------------------------------

/// The legacy catalog-sync section is gone (one-shot import only now).
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/sync",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_library_sync(_caller: Admin) -> JsonResult<serde_json::Value> {
    super::validator::ensure_kept_section("library_sync_settings")?;
    Ok(Json(serde_json::Value::Null))
}

/// PUT twin of the dropped catalog-sync route: gone, not moved.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/library/sync",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_library_sync_put(caller: Admin) -> JsonResult<serde_json::Value> {
    dropped_library_sync(caller).await
}

// --- section prefs (per-user) ------------------------------------------------------

/// Read the caller's section visibility prefs (all three pages).
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/me/section-prefs",
    responses((status = 200, description = "Section prefs", body = SectionPrefsResponse))
)]
pub async fn get_section_prefs(
    State(settings): State<SettingsSetup>,
    User(caller): User,
) -> JsonResult<SectionPrefsResponse> {
    let prefs = settings.prefs().ok_or_else(|| SettingsError::Unavailable {
        cause: "Section prefs need the database.".to_owned(),
    })?;
    let lastfm_master = settings
        .service()
        .get_masked::<LastFmSettings>()?
        .into_inner()
        .enabled;
    super::section_prefs::full_response(
        prefs.store.as_ref(),
        prefs.links.as_ref(),
        caller.user_id(),
        lastfm_master,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// Replace one page of the caller's section prefs. Unknown pages and
/// unknown keys are a 400; only the updated page is returned.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/me/section-prefs",
    request_body = SectionPrefsUpdate,
    responses(
        (status = 200, description = "Updated page", body = SectionPrefsResponse),
        (status = 503, description = "Section prefs are unwired")
    )
)]
pub async fn put_section_prefs(
    State(settings): State<SettingsSetup>,
    User(caller): User,
    ValidJson(body): ValidJson<SectionPrefsUpdate>,
) -> JsonResult<SectionPrefsResponse> {
    let prefs = settings.prefs().ok_or_else(|| SettingsError::Unavailable {
        cause: "Section prefs need the database.".to_owned(),
    })?;
    let lastfm_master = settings
        .service()
        .get_masked::<LastFmSettings>()?
        .into_inner()
        .enabled;
    let items = super::section_prefs::save_page(
        prefs.store.as_ref(),
        prefs.links.as_ref(),
        caller.user_id(),
        &body,
        lastfm_master,
    )
    .await?;
    Ok(Json(SectionPrefsResponse {
        pages: BTreeMap::from([(body.page.clone(), items)]),
    }))
}
