//! Thin settings handlers. Each answers one route and returns typed
//! errors; section logic lives in the service, probes behind the
//! verify port.
//!
//! Verify endpoints test the submitted values, never the stored
//! config, so Test works before the first save and reflects edits. A
//! masked secret resolves to the stored one first; an empty secret
//! fails the verdict without a network round trip.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Extension, Path, State},
};

use super::error::{SettingsError, ValidJson, ValidQuery};
use super::models::{
    AdvancedSettingsDto, ConnectAppsDto, DownloadPolicyDto, EventsSettingsDto,
    FilesystemWatcherDto, FreeMusicDto, FrontendCacheTTLs, GetItDto, IndexerReorderRequest,
    IndexerSavedResponse, IndexerTestResponse, JellyfinConnectionDto, JellyfinUserInfo,
    JellyfinVerifyResponse, LastFmSettingsDto, LibraryPathQuery, LibraryPathRequest,
    LibraryScanScheduleDto, LibraryScanScheduleResponse, LibrarySettingsResponse,
    LibrarySettingsSaveRequest, ListenBrainzConnectionDto, MusicBrainzBindingRequest,
    MusicBrainzSettingsDto, MusicBrainzSettingsUpdate, MusicBrainzVerifyRequest,
    NavidromeConnectionDto, NewznabIndexerDto, OidcConnectionDto, OperationResult,
    PlexConnectionDto, PlexLibrarySectionInfo, PlexVerifyResponse, PolicyImpactResponse,
    PolicySummaryResponse, PrimaryMusicSourceDto, ProwlarrConnectionDto, ProwlarrTestResponse,
    SabnzbdConnectionDto, SabnzbdTestResponse, ScrobbleSettingsDto, SectionPrefsResponse,
    SectionPrefsUpdate, SecuritySettingsDto, SlskdConnectionDto, SourcePriorityDto,
    TestConnectionResponse, UsenetSearchBackendDto, UserPreferencesDto, VerifyConnectionResponse,
    WantedWatcherDto, WrappedSettingsDto, YouTubeConnectionDto,
};
use super::verify::{
    SKIDDLE_BASE_URL, TICKETMASTER_BASE_URL, YOUTUBE_BASE_URL, require_service_url,
};
use super::wiring::{AdminUser, SettingsSetup};
use crate::auth::session::middleware::CurrentSession;

// --- preferences -------------------------------------------------------------

/// Read the release-type filters.
#[utoipa::path(
    get,
    path = "/api/v3/settings/preferences",
    responses((status = 200, description = "Release-type filters", body = UserPreferencesDto))
)]
pub async fn get_preferences(
    State(settings): State<SettingsSetup>,
) -> Result<Json<UserPreferencesDto>, SettingsError> {
    settings.service().get_preferences().map(Json)
}

/// Save the release-type filters.
#[utoipa::path(
    put,
    path = "/api/v3/settings/preferences",
    request_body = UserPreferencesDto,
    responses((status = 200, description = "Saved filters", body = UserPreferencesDto))
)]
pub async fn put_preferences(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<UserPreferencesDto>,
) -> Result<Json<UserPreferencesDto>, SettingsError> {
    settings.service().save_preferences(&body).await.map(Json)
}

// --- library schedule + watcher ----------------------------------------------

/// Read the scan schedule plus the server timezone label.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/schedule",
    responses((status = 200, description = "Scan schedule", body = LibraryScanScheduleResponse))
)]
pub async fn get_schedule(
    State(settings): State<SettingsSetup>,
) -> Result<Json<LibraryScanScheduleResponse>, SettingsError> {
    settings.service().get_schedule().map(Json)
}

/// Save the scan schedule.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library/schedule",
    request_body = LibraryScanScheduleDto,
    responses((status = 200, description = "Saved schedule", body = LibraryScanScheduleResponse))
)]
pub async fn put_schedule(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibraryScanScheduleDto>,
) -> Result<Json<LibraryScanScheduleResponse>, SettingsError> {
    settings.service().save_schedule(&body).await.map(Json)
}

/// Read the filesystem poller knobs.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/watcher",
    responses((status = 200, description = "Filesystem poller knobs", body = FilesystemWatcherDto))
)]
pub async fn get_watcher(
    State(settings): State<SettingsSetup>,
) -> Result<Json<FilesystemWatcherDto>, SettingsError> {
    settings.service().get_watcher().map(Json)
}

/// Save the filesystem poller knobs.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library/watcher",
    request_body = FilesystemWatcherDto,
    responses((status = 200, description = "Saved poller knobs", body = FilesystemWatcherDto))
)]
pub async fn put_watcher(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<FilesystemWatcherDto>,
) -> Result<Json<FilesystemWatcherDto>, SettingsError> {
    settings.service().save_watcher(&body).await.map(Json)
}

// --- library settings --------------------------------------------------------

/// Read the typed library settings with the policy revision.
#[utoipa::path(
    get,
    path = "/api/v3/settings/library",
    responses((status = 200, description = "Library settings", body = LibrarySettingsResponse))
)]
pub async fn get_library(
    State(settings): State<SettingsSetup>,
) -> Result<Json<LibrarySettingsResponse>, SettingsError> {
    settings.service().get_library().map(Json)
}

/// Save the typed library settings. The expected revision must match
/// the stored one or the save is a 409.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library",
    request_body = LibrarySettingsSaveRequest,
    responses(
        (status = 200, description = "Saved library settings", body = LibrarySettingsResponse),
        (status = 409, description = "Expected revision is stale")
    )
)]
pub async fn put_library(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LibrarySettingsSaveRequest>,
) -> Result<Json<LibrarySettingsResponse>, SettingsError> {
    settings.service().save_library(&body).await.map(Json)
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
) -> Result<Json<LibrarySettingsResponse>, SettingsError> {
    settings
        .service()
        .add_library_path(&body.path)
        .await
        .map(Json)
}

/// Remove every library root at one path. Unknown paths are a silent
/// no-op.
#[utoipa::path(
    delete,
    path = "/api/v3/settings/library/paths",
    params(("path" = String, Query, description = "Directory to remove")),
    responses((status = 200, description = "Library settings", body = LibrarySettingsResponse))
)]
pub async fn remove_library_path(
    State(settings): State<SettingsSetup>,
    ValidQuery(query): ValidQuery<LibraryPathQuery>,
) -> Result<Json<LibrarySettingsResponse>, SettingsError> {
    settings
        .service()
        .remove_library_path(&query.path)
        .await
        .map(Json)
}

// --- advanced + cache TTLs ---------------------------------------------------

/// Read the advanced tunables in frontend units.
#[utoipa::path(
    get,
    path = "/api/v3/settings/advanced",
    responses((status = 200, description = "Advanced tunables", body = AdvancedSettingsDto))
)]
pub async fn get_advanced(
    State(settings): State<SettingsSetup>,
) -> Result<Json<AdvancedSettingsDto>, SettingsError> {
    settings.service().get_advanced().map(Json)
}

/// Save the advanced tunables. Unknown fields are a 400: the shape is
/// a closed allowlist.
#[utoipa::path(
    put,
    path = "/api/v3/settings/advanced",
    request_body = AdvancedSettingsDto,
    responses((status = 200, description = "Saved tunables", body = AdvancedSettingsDto))
)]
pub async fn put_advanced(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<AdvancedSettingsDto>,
) -> Result<Json<AdvancedSettingsDto>, SettingsError> {
    settings.service().save_advanced(&body).await.map(Json)
}

/// Read the frontend cache TTLs in backend units (milliseconds).
#[utoipa::path(
    get,
    path = "/api/v3/settings/cache-ttls",
    responses((status = 200, description = "Frontend cache TTLs", body = FrontendCacheTTLs))
)]
pub async fn get_cache_ttls(
    State(settings): State<SettingsSetup>,
) -> Result<Json<FrontendCacheTTLs>, SettingsError> {
    settings.service().get_cache_ttls().map(Json)
}

// --- mask resolution ---------------------------------------------------------

/// Resolve one submitted secret: the mask sentinel means "keep the
/// stored value", anything else is tested as submitted.
fn resolve_secret(submitted: &str, mask: &str, stored: &str) -> String {
    if submitted == mask {
        stored.to_owned()
    } else {
        submitted.to_owned()
    }
}

// --- jellyfin ----------------------------------------------------------------

/// Read the Jellyfin connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/jellyfin",
    responses((status = 200, description = "Jellyfin connection", body = JellyfinConnectionDto))
)]
pub async fn get_jellyfin(
    State(settings): State<SettingsSetup>,
) -> Result<Json<JellyfinConnectionDto>, SettingsError> {
    settings.service().get_jellyfin().map(Json)
}

/// Save the Jellyfin connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/jellyfin",
    request_body = JellyfinConnectionDto,
    responses((status = 200, description = "Saved connection", body = JellyfinConnectionDto))
)]
pub async fn put_jellyfin(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<JellyfinConnectionDto>,
) -> Result<Json<JellyfinConnectionDto>, SettingsError> {
    settings.service().save_jellyfin(&body).await.map(Json)
}

/// Test the submitted Jellyfin values (a masked key tests the stored
/// one). The user list rides along on success for the admin picker.
#[utoipa::path(
    post,
    path = "/api/v3/settings/jellyfin/verify",
    request_body = JellyfinConnectionDto,
    responses((status = 200, description = "Verify verdict", body = JellyfinVerifyResponse))
)]
pub async fn verify_jellyfin(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<JellyfinConnectionDto>,
) -> Result<Json<JellyfinVerifyResponse>, SettingsError> {
    use crate::runtime_config::mask::JELLYFIN_API_KEY_MASK;
    let url = require_service_url(&body.jellyfin_url, "Jellyfin URL")?;
    let stored = settings.service().get_jellyfin_raw()?;
    let key = resolve_secret(
        &body.api_key,
        JELLYFIN_API_KEY_MASK,
        stored.api_key.expose(),
    );
    let verdict = settings.probes().jellyfin(&url, &key).await;
    Ok(Json(JellyfinVerifyResponse {
        success: verdict.valid,
        message: verdict.message,
        users: verdict
            .users
            .into_iter()
            .map(|(id, name)| JellyfinUserInfo { id, name })
            .collect(),
    }))
}

// --- navidrome ---------------------------------------------------------------

/// Read the Navidrome connection (password masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/navidrome",
    responses((status = 200, description = "Navidrome connection", body = NavidromeConnectionDto))
)]
pub async fn get_navidrome(
    State(settings): State<SettingsSetup>,
) -> Result<Json<NavidromeConnectionDto>, SettingsError> {
    settings.service().get_navidrome().map(Json)
}

/// Save the Navidrome connection (a masked password keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/navidrome",
    request_body = NavidromeConnectionDto,
    responses((status = 200, description = "Saved connection", body = NavidromeConnectionDto))
)]
pub async fn put_navidrome(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<NavidromeConnectionDto>,
) -> Result<Json<NavidromeConnectionDto>, SettingsError> {
    settings.service().save_navidrome(&body).await.map(Json)
}

/// Test the submitted Navidrome values (a masked password tests the
/// stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/navidrome/verify",
    request_body = NavidromeConnectionDto,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_navidrome(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<NavidromeConnectionDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    use crate::runtime_config::mask::NAVIDROME_PASSWORD_MASK;
    let url = require_service_url(&body.navidrome_url, "Navidrome URL")?;
    let stored = settings.service().get_navidrome_raw()?;
    let password = resolve_secret(
        &body.password,
        NAVIDROME_PASSWORD_MASK,
        stored.password.expose(),
    );
    let verdict = settings
        .probes()
        .navidrome(&url, &body.username, &password)
        .await;
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

// --- plex --------------------------------------------------------------------

/// Read the Plex connection (token masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/plex",
    responses((status = 200, description = "Plex connection", body = PlexConnectionDto))
)]
pub async fn get_plex(
    State(settings): State<SettingsSetup>,
) -> Result<Json<PlexConnectionDto>, SettingsError> {
    settings.service().get_plex().map(Json)
}

/// Save the Plex connection (a masked token keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/plex",
    request_body = PlexConnectionDto,
    responses((status = 200, description = "Saved connection", body = PlexConnectionDto))
)]
pub async fn put_plex(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<PlexConnectionDto>,
) -> Result<Json<PlexConnectionDto>, SettingsError> {
    settings.service().save_plex(&body).await.map(Json)
}

/// Test the submitted Plex values (a masked token tests the stored
/// one). Music libraries ride along on success.
#[utoipa::path(
    post,
    path = "/api/v3/settings/plex/verify",
    request_body = PlexConnectionDto,
    responses((status = 200, description = "Verify verdict", body = PlexVerifyResponse))
)]
pub async fn verify_plex(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<PlexConnectionDto>,
) -> Result<Json<PlexVerifyResponse>, SettingsError> {
    use crate::runtime_config::mask::PLEX_TOKEN_MASK;
    let url = require_service_url(&body.plex_url, "Plex URL")?;
    let stored = settings.service().get_plex_raw()?;
    let token = resolve_secret(
        &body.plex_token,
        PLEX_TOKEN_MASK,
        stored.plex_token.expose(),
    );
    let verdict = settings.probes().plex(&url, &token).await;
    Ok(Json(PlexVerifyResponse {
        valid: verdict.valid,
        message: verdict.message,
        libraries: verdict
            .libraries
            .into_iter()
            .map(|(key, title)| PlexLibrarySectionInfo { key, title })
            .collect(),
    }))
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
) -> Result<Json<Vec<PlexLibrarySectionInfo>>, SettingsError> {
    let stored = settings.service().get_plex_raw()?;
    let libraries = settings
        .probes()
        .plex_libraries(&stored.plex_url, stored.plex_token.expose())
        .await
        .map_err(|message| {
            if message == "Plex is not configured." {
                SettingsError::InvalidInput { message }
            } else {
                SettingsError::Upstream { message }
            }
        })?;
    Ok(Json(
        libraries
            .into_iter()
            .map(|(key, title)| PlexLibrarySectionInfo { key, title })
            .collect(),
    ))
}

// --- listenbrainz ------------------------------------------------------------

/// Read the ListenBrainz connection (token masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/listenbrainz",
    responses((status = 200, description = "ListenBrainz connection", body = ListenBrainzConnectionDto))
)]
pub async fn get_listenbrainz(
    State(settings): State<SettingsSetup>,
) -> Result<Json<ListenBrainzConnectionDto>, SettingsError> {
    settings.service().get_listenbrainz().map(Json)
}

/// Save the ListenBrainz connection (a masked token keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/listenbrainz",
    request_body = ListenBrainzConnectionDto,
    responses((status = 200, description = "Saved connection", body = ListenBrainzConnectionDto))
)]
pub async fn put_listenbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ListenBrainzConnectionDto>,
) -> Result<Json<ListenBrainzConnectionDto>, SettingsError> {
    settings.service().save_listenbrainz(&body).await.map(Json)
}

/// Test the submitted ListenBrainz values (a masked token tests the
/// stored one). Rate limiting answers 429.
#[utoipa::path(
    post,
    path = "/api/v3/settings/listenbrainz/verify",
    request_body = ListenBrainzConnectionDto,
    responses(
        (status = 200, description = "Verify verdict", body = VerifyConnectionResponse),
        (status = 429, description = "ListenBrainz is rate-limiting this server")
    )
)]
pub async fn verify_listenbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ListenBrainzConnectionDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    use crate::providers::listenbrainz::DEFAULT_BASE_URL as LISTENBRAINZ_BASE_URL;
    use crate::runtime_config::mask::LISTENBRAINZ_TOKEN_MASK;
    let stored = settings.service().get_listenbrainz_raw()?;
    let token = resolve_secret(
        &body.user_token,
        LISTENBRAINZ_TOKEN_MASK,
        stored.user_token.expose(),
    );
    let verdict = settings
        .probes()
        .listenbrainz(LISTENBRAINZ_BASE_URL, &body.username, &token)
        .await;
    if verdict.rate_limited {
        return Err(SettingsError::RateLimited {
            message: "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
                .to_owned(),
        });
    }
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

// --- youtube -----------------------------------------------------------------

/// Read the YouTube connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/youtube",
    responses((status = 200, description = "YouTube connection", body = YouTubeConnectionDto))
)]
pub async fn get_youtube(
    State(settings): State<SettingsSetup>,
) -> Result<Json<YouTubeConnectionDto>, SettingsError> {
    settings.service().get_youtube().map(Json)
}

/// Save the YouTube connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/youtube",
    request_body = YouTubeConnectionDto,
    responses((status = 200, description = "Saved connection", body = YouTubeConnectionDto))
)]
pub async fn put_youtube(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<YouTubeConnectionDto>,
) -> Result<Json<YouTubeConnectionDto>, SettingsError> {
    settings.service().save_youtube(&body).await.map(Json)
}

/// Test the submitted YouTube key (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/youtube/verify",
    request_body = YouTubeConnectionDto,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_youtube(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<YouTubeConnectionDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    use crate::runtime_config::mask::YOUTUBE_API_KEY_MASK;
    let stored = settings.service().get_youtube_raw()?;
    let key = resolve_secret(&body.api_key, YOUTUBE_API_KEY_MASK, stored.api_key.expose());
    let verdict = settings.probes().youtube(YOUTUBE_BASE_URL, &key).await;
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

// --- events ------------------------------------------------------------------

/// Read the events sources (keys masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/events",
    responses((status = 200, description = "Events sources", body = EventsSettingsDto))
)]
pub async fn get_events(
    State(settings): State<SettingsSetup>,
) -> Result<Json<EventsSettingsDto>, SettingsError> {
    settings.service().get_events().map(Json)
}

/// Save the events sources (masked keys keep the stored ones) and kick
/// the sweep.
#[utoipa::path(
    put,
    path = "/api/v3/settings/events",
    request_body = EventsSettingsDto,
    responses((status = 200, description = "Saved sources", body = EventsSettingsDto))
)]
pub async fn put_events(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<EventsSettingsDto>,
) -> Result<Json<EventsSettingsDto>, SettingsError> {
    settings.service().save_events(&body).await.map(Json)
}

/// Test the submitted Ticketmaster key (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/events/test-ticketmaster",
    request_body = EventsSettingsDto,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn test_ticketmaster(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<EventsSettingsDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    use crate::runtime_config::mask::TICKETMASTER_KEY_MASK;
    let stored = settings.service().get_events_raw()?;
    let key = resolve_secret(
        body.ticketmaster_api_key.trim(),
        TICKETMASTER_KEY_MASK,
        stored.ticketmaster_api_key.expose(),
    );
    let verdict = settings
        .probes()
        .ticketmaster(TICKETMASTER_BASE_URL, &key)
        .await;
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

/// Test the submitted Skiddle key (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/events/test-skiddle",
    request_body = EventsSettingsDto,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn test_skiddle(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<EventsSettingsDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    use crate::runtime_config::mask::SKIDDLE_KEY_MASK;
    let stored = settings.service().get_events_raw()?;
    let key = resolve_secret(
        body.skiddle_api_key.trim(),
        SKIDDLE_KEY_MASK,
        stored.skiddle_api_key.expose(),
    );
    let verdict = settings.probes().skiddle(SKIDDLE_BASE_URL, &key).await;
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

// --- wrapped -----------------------------------------------------------------

/// Read the wrapped settings (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/wrapped",
    responses((status = 200, description = "Wrapped settings", body = WrappedSettingsDto))
)]
pub async fn get_wrapped(
    State(settings): State<SettingsSetup>,
) -> Result<Json<WrappedSettingsDto>, SettingsError> {
    settings.service().get_wrapped().map(Json)
}

/// Save the wrapped settings (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/wrapped",
    request_body = WrappedSettingsDto,
    responses((status = 200, description = "Saved settings", body = WrappedSettingsDto))
)]
pub async fn put_wrapped(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<WrappedSettingsDto>,
) -> Result<Json<WrappedSettingsDto>, SettingsError> {
    settings.service().save_wrapped(&body).await.map(Json)
}

// --- scrobble + primary source + lastfm --------------------------------------

/// Read the scrobble targets.
#[utoipa::path(
    get,
    path = "/api/v3/settings/scrobble",
    responses((status = 200, description = "Scrobble targets", body = ScrobbleSettingsDto))
)]
pub async fn get_scrobble(
    State(settings): State<SettingsSetup>,
) -> Result<Json<ScrobbleSettingsDto>, SettingsError> {
    settings.service().get_scrobble().map(Json)
}

/// Save the scrobble targets.
#[utoipa::path(
    put,
    path = "/api/v3/settings/scrobble",
    request_body = ScrobbleSettingsDto,
    responses((status = 200, description = "Saved targets", body = ScrobbleSettingsDto))
)]
pub async fn put_scrobble(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ScrobbleSettingsDto>,
) -> Result<Json<ScrobbleSettingsDto>, SettingsError> {
    settings.service().save_scrobble(&body).await.map(Json)
}

/// Read the primary music source.
#[utoipa::path(
    get,
    path = "/api/v3/settings/primary-source",
    responses((status = 200, description = "Primary music source", body = PrimaryMusicSourceDto))
)]
pub async fn get_primary_source(
    State(settings): State<SettingsSetup>,
) -> Result<Json<PrimaryMusicSourceDto>, SettingsError> {
    settings.service().get_primary_source().map(Json)
}

/// Save the primary music source.
#[utoipa::path(
    put,
    path = "/api/v3/settings/primary-source",
    request_body = PrimaryMusicSourceDto,
    responses((status = 200, description = "Saved source", body = PrimaryMusicSourceDto))
)]
pub async fn put_primary_source(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<PrimaryMusicSourceDto>,
) -> Result<Json<PrimaryMusicSourceDto>, SettingsError> {
    settings
        .service()
        .save_primary_source(&body)
        .await
        .map(Json)
}

/// Read the Last.fm master switch.
#[utoipa::path(
    get,
    path = "/api/v3/settings/lastfm",
    responses((status = 200, description = "Last.fm master switch", body = LastFmSettingsDto))
)]
pub async fn get_lastfm(
    State(settings): State<SettingsSetup>,
) -> Result<Json<LastFmSettingsDto>, SettingsError> {
    settings.service().get_lastfm().map(Json)
}

/// Save the Last.fm master switch.
#[utoipa::path(
    put,
    path = "/api/v3/settings/lastfm",
    request_body = LastFmSettingsDto,
    responses((status = 200, description = "Saved switch", body = LastFmSettingsDto))
)]
pub async fn put_lastfm(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<LastFmSettingsDto>,
) -> Result<Json<LastFmSettingsDto>, SettingsError> {
    settings.service().save_lastfm(&body).await.map(Json)
}

// --- free music + get-it -----------------------------------------------------

/// Read the free-music settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/free-music",
    responses((status = 200, description = "Free-music settings", body = FreeMusicDto))
)]
pub async fn get_free_music(
    State(settings): State<SettingsSetup>,
) -> Result<Json<FreeMusicDto>, SettingsError> {
    settings.service().get_free_music().map(Json)
}

/// Save the free-music settings.
#[utoipa::path(
    put,
    path = "/api/v3/settings/free-music",
    request_body = FreeMusicDto,
    responses((status = 200, description = "Saved settings", body = FreeMusicDto))
)]
pub async fn put_free_music(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<FreeMusicDto>,
) -> Result<Json<FreeMusicDto>, SettingsError> {
    settings.service().save_free_music(&body).await.map(Json)
}

/// Read the store-region settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/get-it",
    responses((status = 200, description = "Store-region settings", body = GetItDto))
)]
pub async fn get_get_it(
    State(settings): State<SettingsSetup>,
) -> Result<Json<GetItDto>, SettingsError> {
    settings.service().get_get_it().map(Json)
}

/// Save the store-region settings.
#[utoipa::path(
    put,
    path = "/api/v3/settings/get-it",
    request_body = GetItDto,
    responses((status = 200, description = "Saved settings", body = GetItDto))
)]
pub async fn put_get_it(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<GetItDto>,
) -> Result<Json<GetItDto>, SettingsError> {
    settings.service().save_get_it(&body).await.map(Json)
}

// --- security + oidc + connect-apps ------------------------------------------

/// Read the security posture settings.
#[utoipa::path(
    get,
    path = "/api/v3/settings/security",
    responses((status = 200, description = "Security settings", body = SecuritySettingsDto))
)]
pub async fn get_security(
    State(settings): State<SettingsSetup>,
) -> Result<Json<SecuritySettingsDto>, SettingsError> {
    settings.service().get_security().map(Json)
}

/// Save the security posture settings.
#[utoipa::path(
    put,
    path = "/api/v3/settings/security",
    request_body = SecuritySettingsDto,
    responses((status = 200, description = "Saved settings", body = SecuritySettingsDto))
)]
pub async fn put_security(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SecuritySettingsDto>,
) -> Result<Json<SecuritySettingsDto>, SettingsError> {
    settings.service().save_security(&body).await.map(Json)
}

/// Check the submitted HIBP hash-list path (must exist and start with
/// a `40-char-SHA1:count` line).
#[utoipa::path(
    post,
    path = "/api/v3/settings/security/verify-hibp",
    request_body = SecuritySettingsDto,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_hibp(
    ValidJson(body): ValidJson<SecuritySettingsDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    let verdict = super::verify::check_hibp_file(&body.hibp_local_path);
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

/// Read the OIDC connection (secret masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/oidc",
    responses((status = 200, description = "OIDC connection", body = OidcConnectionDto))
)]
pub async fn get_oidc(
    State(settings): State<SettingsSetup>,
) -> Result<Json<OidcConnectionDto>, SettingsError> {
    settings.service().get_oidc().map(Json)
}

/// Save the OIDC connection (a masked secret keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/oidc",
    request_body = OidcConnectionDto,
    responses((status = 200, description = "Saved connection", body = OidcConnectionDto))
)]
pub async fn put_oidc(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<OidcConnectionDto>,
) -> Result<Json<OidcConnectionDto>, SettingsError> {
    settings.service().save_oidc(&body).await.map(Json)
}

/// Test the submitted OIDC issuer (fetches its discovery document).
#[utoipa::path(
    post,
    path = "/api/v3/settings/oidc/verify",
    request_body = OidcConnectionDto,
    responses((status = 200, description = "Verify verdict", body = VerifyConnectionResponse))
)]
pub async fn verify_oidc(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<OidcConnectionDto>,
) -> Result<Json<VerifyConnectionResponse>, SettingsError> {
    let verdict = settings.probes().oidc(&body.issuer).await;
    Ok(Json(VerifyConnectionResponse {
        valid: verdict.valid,
        message: verdict.message,
    }))
}

/// Read the inbound Connect Apps config.
#[utoipa::path(
    get,
    path = "/api/v3/settings/connect-apps",
    responses((status = 200, description = "Connect Apps config", body = ConnectAppsDto))
)]
pub async fn get_connect_apps(
    State(settings): State<SettingsSetup>,
) -> Result<Json<ConnectAppsDto>, SettingsError> {
    settings.service().get_connect_apps().map(Json)
}

/// Save the inbound Connect Apps config.
#[utoipa::path(
    put,
    path = "/api/v3/settings/connect-apps",
    request_body = ConnectAppsDto,
    responses((status = 200, description = "Saved config", body = ConnectAppsDto))
)]
pub async fn put_connect_apps(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ConnectAppsDto>,
) -> Result<Json<ConnectAppsDto>, SettingsError> {
    settings.service().save_connect_apps(&body).await.map(Json)
}

// --- musicbrainz + brainzmash -------------------------------------------------

/// Read the settled MusicBrainz connection plus the transient pending echo.
#[utoipa::path(
    get,
    path = "/api/v3/settings/musicbrainz",
    responses((status = 200, description = "MusicBrainz settings", body = MusicBrainzSettingsDto))
)]
pub async fn get_musicbrainz(
    State(settings): State<SettingsSetup>,
) -> Result<Json<MusicBrainzSettingsDto>, SettingsError> {
    settings.lifecycle().get().map(Json)
}

/// Persist one normalized source change (BrainzMash moves through the
/// consent-bound flow, never a direct update).
#[utoipa::path(
    put,
    path = "/api/v3/settings/musicbrainz",
    request_body = MusicBrainzSettingsUpdate,
    responses((status = 200, description = "Saved settings", body = MusicBrainzSettingsDto))
)]
pub async fn put_musicbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<MusicBrainzSettingsUpdate>,
) -> Result<Json<MusicBrainzSettingsDto>, SettingsError> {
    settings.lifecycle().save_update(&body).await.map(Json)
}

/// Stage a BrainzMash proposal without probing upstream.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/brainzmash/stage",
    responses((status = 200, description = "Staged settings", body = MusicBrainzSettingsDto))
)]
pub async fn stage_brainzmash(
    State(settings): State<SettingsSetup>,
) -> Result<Json<MusicBrainzSettingsDto>, SettingsError> {
    settings.lifecycle().stage().await.map(|(dto, _)| Json(dto))
}

/// Record consent for the exact staged proposal. A stale proposal or
/// an outdated disclosure is a 409.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/brainzmash/consent",
    request_body = MusicBrainzBindingRequest,
    responses(
        (status = 200, description = "Consented settings", body = MusicBrainzSettingsDto),
        (status = 409, description = "Proposal is stale or outdated")
    )
)]
pub async fn consent_brainzmash(
    State(settings): State<SettingsSetup>,
    Extension(admin): Extension<AdminUser>,
    ValidJson(body): ValidJson<MusicBrainzBindingRequest>,
) -> Result<Json<MusicBrainzSettingsDto>, SettingsError> {
    settings
        .lifecycle()
        .consent(&body, &admin.user_id)
        .map(Json)
}

/// Verify a BrainzMash binding or probe a plain tier. A binding names
/// the exact consented proposal and probes the pinned endpoint; an
/// update probes its own URL (never BrainzMash). A failed probe is a
/// 502; alternative probes are a 409 while BrainzMash is active.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/verify",
    request_body = MusicBrainzVerifyRequest,
    responses(
        (status = 200, description = "Verified settings", body = MusicBrainzSettingsDto),
        (status = 409, description = "Proposal is stale, or BrainzMash is active"),
        (status = 502, description = "Probe failed")
    )
)]
pub async fn verify_musicbrainz(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<MusicBrainzVerifyRequest>,
) -> Result<Json<MusicBrainzSettingsDto>, SettingsError> {
    use crate::runtime_config::sections::{
        BRAINZMASH_ENDPOINT, MbSourceMode, MusicBrainzSettings, OFFICIAL_MB_API_BASE,
    };
    match &body {
        MusicBrainzVerifyRequest::Binding(binding) => {
            let current = settings.lifecycle().get()?;
            let selected = if current.pending_brainzmash.is_some() {
                current.selected_source_mode
            } else {
                current.source_mode
            };
            if selected != super::models::MbSourceModeDto::Brainzmash {
                return Err(SettingsError::InvalidInput {
                    message: "BrainzMash binding is only valid for a selected BrainzMash proposal."
                        .to_owned(),
                });
            }
            settings.lifecycle().check_verify_binding(binding)?;
            let verdict = settings.probes().musicbrainz(BRAINZMASH_ENDPOINT).await;
            if !verdict.valid {
                return Err(SettingsError::Upstream {
                    message: verdict.message,
                });
            }
            if !settings.lifecycle().proposal_is_current(binding) {
                return Err(SettingsError::Conflict {
                    message: "BrainzMash proposal is stale.".to_owned(),
                });
            }
            settings.lifecycle().record_verification(binding).map(Json)
        }
        MusicBrainzVerifyRequest::Update(update) => {
            let stored: MusicBrainzSettings = settings
                .service()
                .store
                .get()
                .map_err(|error| SettingsError::from_config(error, &*settings.service().ids))?;
            if super::musicbrainz::is_brainzmash_active_binding_valid(&stored) {
                return Err(SettingsError::Conflict {
                    message: "Alternative MusicBrainz tests are disabled while BrainzMash is active; save to switch sources."
                        .to_owned(),
                });
            }
            let mode: MbSourceMode = update.source_mode.into();
            if mode == MbSourceMode::Brainzmash {
                return Err(SettingsError::InvalidInput {
                    message: "BrainzMash verification requires the consent binding.".to_owned(),
                });
            }
            let api_url = update
                .api_url
                .clone()
                .filter(|url| !url.trim().is_empty())
                .unwrap_or_else(|| OFFICIAL_MB_API_BASE.to_owned());
            let api_url = require_service_url(&api_url, "MusicBrainz API URL")?;
            let verdict = settings.probes().musicbrainz(&api_url).await;
            if !verdict.valid {
                return Err(SettingsError::Upstream {
                    message: verdict.message,
                });
            }
            settings.lifecycle().get().map(Json)
        }
    }
}

/// Promote the exact verified proposal to the active binding.
#[utoipa::path(
    post,
    path = "/api/v3/settings/musicbrainz/activate",
    request_body = MusicBrainzBindingRequest,
    responses(
        (status = 200, description = "Activated settings", body = MusicBrainzSettingsDto),
        (status = 409, description = "Proposal is stale, unconsented, or unverified")
    )
)]
pub async fn activate_brainzmash(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<MusicBrainzBindingRequest>,
) -> Result<Json<MusicBrainzSettingsDto>, SettingsError> {
    settings.lifecycle().activate(&body).await.map(Json)
}

// --- download client (slskd) -------------------------------------------------

/// Read the slskd connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-client/config",
    responses((status = 200, description = "slskd connection", body = SlskdConnectionDto))
)]
pub async fn get_slskd(
    State(settings): State<SettingsSetup>,
) -> Result<Json<SlskdConnectionDto>, SettingsError> {
    settings.service().get_slskd().map(Json)
}

/// Save the slskd connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-client/config",
    request_body = SlskdConnectionDto,
    responses((status = 200, description = "Saved connection", body = SlskdConnectionDto))
)]
pub async fn put_slskd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SlskdConnectionDto>,
) -> Result<Json<SlskdConnectionDto>, SettingsError> {
    settings.service().save_slskd(&body).await.map(Json)
}

/// Test the submitted slskd values (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/download-client/test",
    request_body = SlskdConnectionDto,
    responses((status = 200, description = "Verify verdict", body = TestConnectionResponse))
)]
pub async fn test_slskd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SlskdConnectionDto>,
) -> Result<Json<TestConnectionResponse>, SettingsError> {
    use crate::runtime_config::mask::SLSKD_API_KEY_MASK;
    let url = require_service_url(&body.url, "Download client URL")?;
    let stored = settings.service().get_slskd_raw()?;
    // Strip paste whitespace before the mask comparison (the mask is
    // strip-identity) so a pasted key tests as typed.
    let key = resolve_secret(
        body.api_key.trim(),
        SLSKD_API_KEY_MASK,
        stored.api_key.expose(),
    );
    let verdict = settings.probes().slskd(&url, &key).await;
    Ok(Json(TestConnectionResponse {
        valid: verdict.valid,
        version: verdict.version,
        message: verdict.message,
    }))
}

// --- download clients (sabnzbd) ----------------------------------------------

/// Read the SABnzbd connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/sabnzbd",
    responses((status = 200, description = "SABnzbd connection", body = SabnzbdConnectionDto))
)]
pub async fn get_sabnzbd(
    State(settings): State<SettingsSetup>,
) -> Result<Json<SabnzbdConnectionDto>, SettingsError> {
    settings.service().get_sabnzbd().map(Json)
}

/// Save the SABnzbd connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/sabnzbd",
    request_body = SabnzbdConnectionDto,
    responses((status = 200, description = "Saved connection", body = SabnzbdConnectionDto))
)]
pub async fn put_sabnzbd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SabnzbdConnectionDto>,
) -> Result<Json<SabnzbdConnectionDto>, SettingsError> {
    settings.service().save_sabnzbd(&body).await.map(Json)
}

/// Test the submitted SABnzbd values (a masked key tests the stored
/// one). The submitted downloads mount is diagnosed, not the stored one.
#[utoipa::path(
    post,
    path = "/api/v3/settings/download-clients/sabnzbd/test",
    request_body = SabnzbdConnectionDto,
    responses((status = 200, description = "Verify verdict", body = SabnzbdTestResponse))
)]
pub async fn test_sabnzbd(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SabnzbdConnectionDto>,
) -> Result<Json<SabnzbdTestResponse>, SettingsError> {
    use crate::runtime_config::mask::SABNZBD_API_KEY_MASK;
    let url = require_service_url(&body.url, "SABnzbd URL")?;
    let stored = settings.service().get_sabnzbd_raw()?;
    let key = resolve_secret(&body.api_key, SABNZBD_API_KEY_MASK, stored.api_key.expose());
    let verdict = settings
        .probes()
        .sabnzbd(&url, &key, &body.downloads_mount)
        .await;
    let diagnosis = verdict
        .diagnosis
        .unwrap_or(super::verify::SabnzbdMountDiagnosis {
            mount_has_files: true,
            resolvable_downloads: 0,
            sampled_downloads: 0,
            mount_message: None,
        });
    Ok(Json(SabnzbdTestResponse {
        valid: verdict.valid,
        version: verdict.version,
        message: verdict.message,
        categories: verdict.categories,
        complete_dir: verdict.complete_dir,
        mount_has_files: Some(diagnosis.mount_has_files),
        resolvable_downloads: Some(diagnosis.resolvable_downloads),
        sampled_downloads: Some(diagnosis.sampled_downloads),
        mount_message: diagnosis.mount_message,
    }))
}

// --- source priority + wanted ------------------------------------------------

/// Read the acquisition source try-order.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/source-priority",
    responses((status = 200, description = "Source try-order", body = SourcePriorityDto))
)]
pub async fn get_source_priority(
    State(settings): State<SettingsSetup>,
) -> Result<Json<SourcePriorityDto>, SettingsError> {
    settings.service().get_source_priority().map(Json)
}

/// Save the acquisition source try-order.
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/source-priority",
    request_body = SourcePriorityDto,
    responses((status = 200, description = "Saved try-order", body = SourcePriorityDto))
)]
pub async fn put_source_priority(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<SourcePriorityDto>,
) -> Result<Json<SourcePriorityDto>, SettingsError> {
    settings
        .service()
        .save_source_priority(&body)
        .await
        .map(Json)
}

/// Read the wanted watcher toggles.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/wanted",
    responses((status = 200, description = "Wanted watcher toggles", body = WantedWatcherDto))
)]
pub async fn get_wanted(
    State(settings): State<SettingsSetup>,
) -> Result<Json<WantedWatcherDto>, SettingsError> {
    settings.service().get_wanted().map(Json)
}

/// Save the wanted watcher toggles.
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/wanted",
    request_body = WantedWatcherDto,
    responses((status = 200, description = "Saved toggles", body = WantedWatcherDto))
)]
pub async fn put_wanted(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<WantedWatcherDto>,
) -> Result<Json<WantedWatcherDto>, SettingsError> {
    settings.service().save_wanted(&body).await.map(Json)
}

// --- download policy ---------------------------------------------------------

/// Read the acquisition policy.
#[utoipa::path(
    get,
    path = "/api/v3/settings/download-clients/policy",
    responses((status = 200, description = "Acquisition policy", body = DownloadPolicyDto))
)]
pub async fn get_policy(
    State(settings): State<SettingsSetup>,
) -> Result<Json<DownloadPolicyDto>, SettingsError> {
    settings.service().get_policy().map(Json)
}

/// Save the acquisition policy.
#[utoipa::path(
    put,
    path = "/api/v3/settings/download-clients/policy",
    request_body = DownloadPolicyDto,
    responses((status = 200, description = "Saved policy", body = DownloadPolicyDto))
)]
pub async fn put_policy(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<DownloadPolicyDto>,
) -> Result<Json<DownloadPolicyDto>, SettingsError> {
    settings.service().save_policy(&body).await.map(Json)
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
) -> Result<Json<PolicySummaryResponse>, SettingsError> {
    settings.service().policy_summary().map(Json)
}

/// Impact preview of an unsaved policy body against persisted rows.
/// Needs the database; unwired states answer 503.
#[utoipa::path(
    post,
    path = "/api/v3/settings/download-clients/policy/impact",
    request_body = DownloadPolicyDto,
    responses(
        (status = 200, description = "Impact preview", body = PolicyImpactResponse),
        (status = 503, description = "Bucket counts are unwired")
    )
)]
pub async fn post_policy_impact(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<DownloadPolicyDto>,
) -> Result<Json<PolicyImpactResponse>, SettingsError> {
    let buckets = settings
        .buckets()
        .ok_or_else(|| SettingsError::Unavailable {
            message: "Policy impact needs the database.".to_owned(),
        })?;
    settings
        .service()
        .policy_impact(buckets.as_ref(), &body)
        .await
        .map(Json)
}

// --- indexers ----------------------------------------------------------------

/// List the configured Newznab indexers (keys masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/indexers",
    responses((status = 200, description = "Configured indexers", body = Vec<NewznabIndexerDto>))
)]
pub async fn list_indexers(
    State(settings): State<SettingsSetup>,
) -> Result<Json<Vec<NewznabIndexerDto>>, SettingsError> {
    settings.service().list_indexers().map(Json)
}

/// Save one indexer (create when the id is blank, else update).
#[utoipa::path(
    post,
    path = "/api/v3/settings/indexers",
    request_body = NewznabIndexerDto,
    responses((status = 200, description = "Saved indexer id", body = IndexerSavedResponse))
)]
pub async fn create_indexer(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<NewznabIndexerDto>,
) -> Result<Json<IndexerSavedResponse>, SettingsError> {
    settings.service().save_indexer(&body).await.map(Json)
}

/// Save one indexer; the path id wins over the body id.
#[utoipa::path(
    put,
    path = "/api/v3/settings/indexers/{id}",
    params(("id" = String, Path, description = "Indexer id")),
    request_body = NewznabIndexerDto,
    responses((status = 200, description = "Saved indexer id", body = IndexerSavedResponse))
)]
pub async fn update_indexer(
    State(settings): State<SettingsSetup>,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<NewznabIndexerDto>,
) -> Result<Json<IndexerSavedResponse>, SettingsError> {
    let mut dto = body;
    dto.id = id;
    settings.service().save_indexer(&dto).await.map(Json)
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
) -> Result<Json<OperationResult>, SettingsError> {
    settings.service().delete_indexer(&id).await?;
    Ok(Json(OperationResult { success: true }))
}

/// Persist a dragged-card priority order. Unknown or duplicate ids are
/// a 400, never a partial reorder.
#[utoipa::path(
    post,
    path = "/api/v3/settings/indexers/reorder",
    request_body = IndexerReorderRequest,
    responses((status = 200, description = "Reordered", body = OperationResult))
)]
pub async fn reorder_indexers(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<IndexerReorderRequest>,
) -> Result<Json<OperationResult>, SettingsError> {
    settings
        .service()
        .reorder_indexers(&body.ordered_ids)
        .await?;
    Ok(Json(OperationResult { success: true }))
}

/// Test one indexer's caps with the submitted url/key (a masked key
/// tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/indexers/test",
    request_body = NewznabIndexerDto,
    responses((status = 200, description = "Caps verdict", body = IndexerTestResponse))
)]
pub async fn test_indexer(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<NewznabIndexerDto>,
) -> Result<Json<IndexerTestResponse>, SettingsError> {
    use crate::runtime_config::mask::INDEXER_API_KEY_MASK;
    let url = require_service_url(&body.url, "Indexer URL")?;
    let key = if body.api_key == INDEXER_API_KEY_MASK {
        settings
            .service()
            .get_indexer_raw(&body.id)?
            .map(|indexer| indexer.api_key.expose().to_owned())
            .unwrap_or_default()
    } else {
        body.api_key.clone()
    };
    let verdict = settings.probes().newznab(&url, &key).await;
    Ok(Json(IndexerTestResponse {
        valid: verdict.valid,
        version: verdict.version,
        message: verdict.message,
        supports_audio_search: verdict.supports_audio_search,
        category_count: verdict.category_count,
        suggested_url: verdict.suggested_url,
    }))
}

/// Read the active Usenet search backend.
#[utoipa::path(
    get,
    path = "/api/v3/settings/indexers/search-backend",
    responses((status = 200, description = "Search backend", body = UsenetSearchBackendDto))
)]
pub async fn get_search_backend(
    State(settings): State<SettingsSetup>,
) -> Result<Json<UsenetSearchBackendDto>, SettingsError> {
    settings.service().get_usenet_backend().map(Json)
}

/// Save the active Usenet search backend.
#[utoipa::path(
    put,
    path = "/api/v3/settings/indexers/search-backend",
    request_body = UsenetSearchBackendDto,
    responses((status = 200, description = "Saved backend", body = UsenetSearchBackendDto))
)]
pub async fn put_search_backend(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<UsenetSearchBackendDto>,
) -> Result<Json<UsenetSearchBackendDto>, SettingsError> {
    settings
        .service()
        .save_usenet_backend(&body)
        .await
        .map(Json)
}

// --- prowlarr ----------------------------------------------------------------

/// Read the Prowlarr connection (key masked unless unset).
#[utoipa::path(
    get,
    path = "/api/v3/settings/prowlarr/config",
    responses((status = 200, description = "Prowlarr connection", body = ProwlarrConnectionDto))
)]
pub async fn get_prowlarr(
    State(settings): State<SettingsSetup>,
) -> Result<Json<ProwlarrConnectionDto>, SettingsError> {
    settings.service().get_prowlarr().map(Json)
}

/// Save the Prowlarr connection (a masked key keeps the stored one).
#[utoipa::path(
    put,
    path = "/api/v3/settings/prowlarr/config",
    request_body = ProwlarrConnectionDto,
    responses((status = 200, description = "Saved connection", body = ProwlarrConnectionDto))
)]
pub async fn put_prowlarr(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ProwlarrConnectionDto>,
) -> Result<Json<ProwlarrConnectionDto>, SettingsError> {
    settings.service().save_prowlarr(&body).await.map(Json)
}

/// Test the submitted Prowlarr values (a masked key tests the stored one).
#[utoipa::path(
    post,
    path = "/api/v3/settings/prowlarr/test",
    request_body = ProwlarrConnectionDto,
    responses((status = 200, description = "Verify verdict", body = ProwlarrTestResponse))
)]
pub async fn test_prowlarr(
    State(settings): State<SettingsSetup>,
    ValidJson(body): ValidJson<ProwlarrConnectionDto>,
) -> Result<Json<ProwlarrTestResponse>, SettingsError> {
    use crate::runtime_config::mask::PROWLARR_API_KEY_MASK;
    let url = require_service_url(&body.url, "Prowlarr URL")?;
    let stored = settings.service().get_prowlarr_raw()?;
    let key = resolve_secret(
        &body.api_key,
        PROWLARR_API_KEY_MASK,
        stored.api_key.expose(),
    );
    let verdict = settings.probes().prowlarr(&url, &key).await;
    Ok(Json(ProwlarrTestResponse {
        valid: verdict.valid,
        version: verdict.version,
        message: verdict.message,
        indexer_count: verdict.indexer_count,
    }))
}

// --- dropped sections (410) --------------------------------------------------

/// The legacy catalog-sync section is gone (one-shot import only now).
#[utoipa::path(
    get,
    path = "/api/v3/settings/library/sync",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_library_sync() -> Result<Json<serde_json::Value>, SettingsError> {
    super::validator::ensure_kept_section("library_sync_settings")?;
    Ok(Json(serde_json::Value::Null))
}

/// PUT twin of the dropped catalog-sync route: gone, not moved.
#[utoipa::path(
    put,
    path = "/api/v3/settings/library/sync",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_library_sync_put() -> Result<Json<serde_json::Value>, SettingsError> {
    dropped_library_sync().await
}

/// The vestigial home section is gone.
#[utoipa::path(
    get,
    path = "/api/v3/settings/home",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_home() -> Result<Json<serde_json::Value>, SettingsError> {
    super::validator::ensure_kept_section("home_settings")?;
    Ok(Json(serde_json::Value::Null))
}

/// PUT twin of the dropped home route: gone, not moved.
#[utoipa::path(
    put,
    path = "/api/v3/settings/home",
    responses((status = 410, description = "Section dropped in v3"))
)]
pub async fn dropped_home_put() -> Result<Json<serde_json::Value>, SettingsError> {
    dropped_home().await
}

// --- section prefs (per-user) ------------------------------------------------

/// Read the caller's section visibility prefs (all three pages).
#[utoipa::path(
    get,
    path = "/api/v3/me/section-prefs",
    responses((status = 200, description = "Section prefs", body = SectionPrefsResponse))
)]
pub async fn get_section_prefs(
    State(settings): State<SettingsSetup>,
    Extension(session): Extension<CurrentSession>,
) -> Result<Json<SectionPrefsResponse>, SettingsError> {
    let prefs = settings.prefs().ok_or_else(|| SettingsError::Unavailable {
        message: "Section prefs need the database.".to_owned(),
    })?;
    let lastfm_master = settings.service().get_lastfm()?.enabled;
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
) -> Result<Json<SectionPrefsResponse>, SettingsError> {
    let prefs = settings.prefs().ok_or_else(|| SettingsError::Unavailable {
        message: "Section prefs need the database.".to_owned(),
    })?;
    let lastfm_master = settings.service().get_lastfm()?.enabled;
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
