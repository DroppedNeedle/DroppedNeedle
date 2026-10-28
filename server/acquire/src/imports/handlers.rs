//! Native `/api/v3` import + health handlers and the router constructor.
//!
//! Handlers are thin: extract the caller, call one service, render the
//! shape. Status mapping lives in [`ImportsError`](super::error::ImportsError)'s
//! one mapping onto `ApiError`; query strings parse through [`ValidQuery`] and
//! bodies through [`ValidJson`] so malformed input stays inside the shared
//! envelope. [`routes`] mounts every handler in the group of its caller
//! extractor, so those routes 401 anonymously, and the Spotify OAuth
//! callback in the public group (it is identified by its state token).
//! The callback also answers on its v2 path for Spotify apps registered against
//! v2; either way it exchanges the code with the redirect URI stored next to
//! the state at authorize time.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use droppedneedle_auth::session::middleware::effective_origin;
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Deserialize;
use utoipa::IntoParams;

use super::error::ImportsError;
use super::health::HealthProbes;
use super::jobs::{ImportState, SpotifyImports};
use super::lidarr::{
    LIDARR_NOT_CONNECTED, LidarrClient, LidarrError, LidarrImportService, ServiceError,
    normalize_lidarr_url,
};
use super::models::{
    AcquireHealth, LidarrArtistListResponse, LidarrConnectionSettings, LidarrImportRequest,
    LidarrImportResponse, LidarrTestResponse, SabnzbdStatusResponse, SlskdStatusResponse,
    SpotifyAuthUrlResponse, SpotifyImportRequest, SpotifyImportResponse, SpotifyJobStatus,
    SpotifyPlaylistListResponse, SpotifyRedirectUri, SpotifySettings,
};
use super::spotify::SpotifyImportService;
use super::spotify_albums::AlbumResolver;
use super::spotify_client::{SpotifyClient, SpotifyError, now_unix_secs};
use super::spotify_oauth::{
    PendingAuth, SPOTIFY_CALLBACK_PATH, SPOTIFY_LEGACY_CALLBACK_PATH, SpotifyConnection,
    redirect_uri,
};
use super::spotify_store::{CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates};
use crate::follows::Follows;
use crate::requests::error::RequestsError;
use crate::settings::{ConfigLidarrSettings, ConfigSpotifySettings};

/// Every dependency the imports routes need, injected by constructor.
#[derive(Clone)]
pub struct ImportsDeps {
    /// Read-only Lidarr client (the two sanctioned GETs).
    pub lidarr: LidarrClient,
    /// Lidarr connection settings.
    pub lidarr_settings: Arc<ConfigLidarrSettings>,
    /// The follows the import writes.
    pub follows: Follows,
    /// Spotify API client over injected bases.
    pub spotify: SpotifyClient,
    /// Spotify app settings.
    pub spotify_settings: Arc<ConfigSpotifySettings>,
    /// Single-use OAuth states.
    pub spotify_states: Arc<SqliteSpotifyStates>,
    /// Per-user Spotify links.
    pub spotify_links: Arc<SqliteSpotifyLinks>,
    /// Imported playlists (`spotify:{id}` refs) and their rows.
    pub spotify_playlists: Arc<CollectionsPlaylistBridge>,
    /// Finds each imported album's MusicBrainz release group.
    pub resolver: Arc<AlbumResolver>,
    /// Queues Spotify imports on the jobs table and reads their state.
    pub spotify_imports: SpotifyImports,
    /// Health probes behind the smoke and gates.
    pub probes: Arc<HealthProbes>,
    /// Whether a download source is on, for the smoke.
    pub readiness: crate::readiness::SourceReadiness,
    /// Fresh ids, the OAuth state fallback when the OS RNG fails.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment base path (between origin and callback, exactly once).
    pub base_path: String,
    /// Reverse proxies whose forwarded headers name this server's address.
    pub trusted_proxies: TrustedProxies,
}

impl ImportsDeps {
    /// Lidarr import service over the wired stores.
    fn lidarr_service(&self) -> LidarrImportService {
        LidarrImportService::new(
            self.lidarr.clone(),
            self.lidarr_settings.clone(),
            self.follows.clone(),
        )
    }

    /// Spotify import service over the wired stores.
    fn spotify_service(&self) -> SpotifyImportService {
        SpotifyImportService::new(
            self.spotify.clone(),
            self.spotify_settings.clone(),
            self.spotify_links.clone(),
            self.spotify_playlists.clone(),
            self.resolver.clone(),
        )
    }
}

/// Every imports route plus the v2 OAuth callback in one router with no
/// layers, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn imports_router(deps: ImportsDeps) -> axum::Router {
    routes()
        .with_state(deps.clone())
        .into_router()
        .merge(imports_legacy_callback_router(deps))
}

/// The imports, health and Spotify routes, in the group of each handler's
/// caller extractor. The Spotify OAuth callback is public: it is
/// state-token identified.
pub fn routes() -> Routes<ImportsDeps> {
    Routes::new()
        .admin(routes!(get_lidarr_config, put_lidarr_config))
        .admin(routes!(test_lidarr))
        .admin(routes!(list_lidarr_artists))
        .admin(routes!(import_lidarr))
        .admin(routes!(get_spotify_settings, put_spotify_settings))
        .admin(routes!(get_spotify_redirect_uri))
        .user(routes!(get_spotify_auth_url))
        .public(routes!(spotify_callback))
        .user(routes!(list_spotify_playlists))
        .user(routes!(import_spotify_playlist))
        .user(routes!(get_spotify_job))
        .user(routes!(disconnect_spotify))
        .user(routes!(get_health))
        .user(routes!(get_slskd_status))
        .admin(routes!(get_sabnzbd_status))
}

/// The callback at its v2 path, at full path (not nested under `/api/v3`).
/// Public and state-identified like the v3 one.
pub fn imports_legacy_callback_router(deps: ImportsDeps) -> axum::Router {
    axum::Router::new()
        .route(SPOTIFY_LEGACY_CALLBACK_PATH, get(spotify_legacy_callback))
        .with_state(deps)
}

/// Map a Lidarr service failure to the wire. Upstream detail reaches the
/// log only.
fn lidarr_failed(error: ServiceError) -> ImportsError {
    match error {
        ServiceError::NotConnected => ImportsError::NotConfigured {
            message: LIDARR_NOT_CONNECTED.to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Auth) => ImportsError::AuthFailed {
            cause: "Lidarr rejected the stored API key".to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Unavailable(detail)) => ImportsError::Unavailable {
            cause: format!("lidarr import failed: {detail}"),
        },
        ServiceError::Follows(RequestsError::Busy { operation }) => ImportsError::Busy {
            cause: format!("lidarr import follows: {operation}"),
        },
        ServiceError::Follows(cause) => ImportsError::internal(&format_args!(
            "lidarr import follow write failed: {cause:?}"
        )),
    }
}

/// Masked Lidarr connection settings.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/lidarr-import/config",
    responses((status = 200, description = "Masked Lidarr settings", body = LidarrConnectionSettings)))]
pub async fn get_lidarr_config(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Json<LidarrConnectionSettings> {
    Json(deps.lidarr_settings.get())
}

/// Save the Lidarr connection. A masked key preserves the stored one.
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/lidarr-import/config",
    request_body = LidarrConnectionSettings,
    responses((status = 200, description = "Saved Lidarr settings", body = LidarrConnectionSettings)))]
pub async fn put_lidarr_config(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<LidarrConnectionSettings>,
) -> Json<LidarrConnectionSettings> {
    deps.lidarr_settings.save(&body).await;
    Json(deps.lidarr_settings.get())
}

/// Test submitted Lidarr credentials against `system/status`: the
/// submitted url/key are probed so Test works before the first
/// save, a masked key resolves to the stored one, and reachable/bad-key
/// verdicts travel in the body, never as a leaked 5xx, never echoing the
/// URL or host.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/lidarr-import/test",
    request_body = LidarrConnectionSettings,
    responses((status = 200, description = "Lidarr probe verdict", body = LidarrTestResponse)))]
pub async fn test_lidarr(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<LidarrConnectionSettings>,
) -> Json<LidarrTestResponse> {
    let mut api_key = body.api_key;
    if api_key == droppedneedle_kernel::runtime_config::SECRET_MASK {
        api_key = deps.lidarr_settings.get_raw().api_key.expose().to_owned();
    }
    let url = normalize_lidarr_url(&body.url);
    match deps.lidarr.system_status(&url, &api_key).await {
        Err(LidarrError::Auth) => Json(LidarrTestResponse {
            valid: false,
            version: None,
            message: "Lidarr rejected the API key. Check Settings -> General -> Security -> API Key in Lidarr.".to_owned(),
        }),
        Err(LidarrError::Unavailable(detail)) => {
            tracing::warn!(%detail, "lidarr test probe failed");
            Json(LidarrTestResponse {
                valid: false,
                version: None,
                message: "Couldn't reach Lidarr. Check the URL and that Lidarr is running.".to_owned(),
            })
        }
        Ok(status) => Json(LidarrTestResponse {
            valid: true,
            version: if status.version.is_empty() { None } else { Some(status.version.clone()) },
            message: if status.version.is_empty() {
                "Connected".to_owned()
            } else {
                format!("Connected - Lidarr v{}", status.version)
            },
        }),
    }
}

/// Monitored Lidarr artists annotated for the caller.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/lidarr-import/artists",
    responses((status = 200, description = "Import candidates", body = LidarrArtistListResponse)))]
pub async fn list_lidarr_artists(
    State(deps): State<ImportsDeps>,
    admin: Admin,
) -> Result<Json<LidarrArtistListResponse>, ApiError> {
    let artists = deps
        .lidarr_service()
        .list_candidates(admin.user_id())
        .await
        .map_err(lidarr_failed)?;
    Ok(Json(artists))
}

/// Import selected Lidarr artists into the caller's follows.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/lidarr-import/import",
    request_body = LidarrImportRequest,
    responses((status = 200, description = "Import summary", body = LidarrImportResponse)))]
pub async fn import_lidarr(
    State(deps): State<ImportsDeps>,
    admin: Admin,
    ValidJson(body): ValidJson<LidarrImportRequest>,
) -> Result<Json<LidarrImportResponse>, ApiError> {
    // Only an admin reaches this route, so the flag is always true; the
    // service keeps it for a future non-admin import route whose
    // auto-download mirror needs an approval batch instead.
    let summary = deps
        .lidarr_service()
        .import_artists(admin.user_id(), true, &body.selected_mbids)
        .await
        .map_err(lidarr_failed)?;
    Ok(Json(summary))
}

/// Masked Spotify app settings.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/settings",
    responses((status = 200, description = "Masked Spotify settings", body = SpotifySettings)))]
pub async fn get_spotify_settings(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Json<SpotifySettings> {
    Json(deps.spotify_settings.get())
}

/// Save the Spotify app settings. A masked secret preserves the stored
/// one; a non-absolute redirect origin is a 400.
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/spotify/settings",
    request_body = SpotifySettings,
    responses((status = 200, description = "Saved Spotify settings", body = SpotifySettings)))]
pub async fn put_spotify_settings(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<SpotifySettings>,
) -> Result<Json<SpotifySettings>, ApiError> {
    deps.spotify_settings.save(&body).await?;
    Ok(Json(deps.spotify_settings.get()))
}

/// The computed OAuth redirect URI, for the admin to register.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/redirect-uri",
    responses(
        (status = 200, description = "OAuth redirect URI", body = SpotifyRedirectUri),
        (status = 400, description = "No Redirect Origin is set and the request names no address to build one from"),
    ))]
pub async fn get_spotify_redirect_uri(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    request: Request,
) -> Result<Json<SpotifyRedirectUri>, ApiError> {
    Ok(Json(SpotifyRedirectUri {
        redirect_uri: callback_uri(&deps, &request)?,
    }))
}

/// What a caller reads when the callback address cannot be built.
const NO_CALLBACK_ADDRESS: &str = "DroppedNeedle could not work out its own address for the \
     Spotify link. An admin needs to fill in Redirect Origin under Settings > Spotify.";

/// The redirect URI this server sends Spotify: the v2 callback path for an
/// app the v2 import carried (its dashboard lists that path), else the v3
/// one. Without a configured origin it is built on the address the session
/// origin check works out for this request, so forwarded headers count
/// only from a trusted proxy. A request that names no address is refused
/// instead of guessed at.
fn callback_uri(deps: &ImportsDeps, request: &Request) -> Result<String, ImportsError> {
    let path = if deps.spotify_settings.legacy_callback() {
        SPOTIFY_LEGACY_CALLBACK_PATH
    } else {
        SPOTIFY_CALLBACK_PATH
    };
    let configured = deps.spotify_settings.get_raw().spotify_redirect_origin;
    let base = if configured.trim().is_empty() {
        effective_origin(request, &deps.trusted_proxies)
            .map(|origin| origin.serialized())
            .ok_or_else(|| ImportsError::InvalidInput {
                message: NO_CALLBACK_ADDRESS.to_owned(),
            })?
    } else {
        String::new()
    };
    Ok(redirect_uri(path, &configured, &base, &deps.base_path))
}

/// Authorize URL for the caller's Spotify link flow. Requires the admin
/// app to be enabled and complete.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/auth/url",
    responses(
        (status = 200, description = "Authorize URL", body = SpotifyAuthUrlResponse),
        (status = 400, description = "Spotify is not set up, or no Redirect Origin is set and the request names no address to build one from"),
    ))]
pub async fn get_spotify_auth_url(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    request: Request,
) -> Result<Json<SpotifyAuthUrlResponse>, ApiError> {
    let uri = callback_uri(&deps, &request)?;
    let state = fresh_state(&deps);
    let response = deps
        .spotify_service()
        .auth_url(&uri, &state)
        .map_err(|message| ImportsError::InvalidInput { message })?;
    let pending = PendingAuth {
        user_id: principal.user_id().to_owned(),
        redirect_uri: uri,
    };
    deps.spotify_states
        .store_state(&state, &pending)
        .await
        .map_err(|cause| ImportsError::internal(&cause))?;
    Ok(Json(response))
}

/// OAuth callback query (`code`, `state`, `error`, all optional).
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SpotifyCallbackQuery {
    /// Authorization code from Spotify.
    pub code: Option<String>,
    /// Round-tripped state token.
    pub state: Option<String>,
    /// Provider-side error, when the user declined.
    pub error: Option<String>,
}

/// Spotify OAuth callback. State-identified, so it sits outside the
/// session gate: every outcome redirects to the profile page with
/// `spotify=connected` or `spotify=error` with an optional `reason`.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/auth/callback",
    params(SpotifyCallbackQuery),
    responses((status = 307, description = "Redirect to the profile page")))]
pub async fn spotify_callback(
    State(deps): State<ImportsDeps>,
    ValidQuery(query): ValidQuery<SpotifyCallbackQuery>,
) -> Response {
    finish_spotify_link(&deps, query).await
}

/// The same callback on the v2 path, where Spotify returns users of an app
/// registered against v2.
pub async fn spotify_legacy_callback(
    State(deps): State<ImportsDeps>,
    ValidQuery(query): ValidQuery<SpotifyCallbackQuery>,
) -> Response {
    finish_spotify_link(&deps, query).await
}

/// Consume the state, exchange the code with the redirect URI stored at
/// authorize time (Spotify rejects any other), read the Spotify profile
/// and store the link.
async fn finish_spotify_link(deps: &ImportsDeps, query: SpotifyCallbackQuery) -> Response {
    let profile = |suffix: &str| format!("{}{suffix}", deps.base_path);
    if query.error.is_some() || query.code.is_none() || query.state.is_none() {
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    }
    let pending = match deps
        .spotify_states
        .consume_state(query.state.as_deref().unwrap_or_default())
        .await
    {
        Ok(Some(pending)) => pending,
        Ok(None) => {
            return Redirect::temporary(&profile("/profile?spotify=error&reason=state"))
                .into_response();
        }
        Err(cause) => {
            tracing::error!(%cause, "spotify state lookup failed");
            return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
        }
    };
    let raw = deps.spotify_settings.get_raw();
    let grant = match deps
        .spotify
        .exchange_code(
            &raw.client_id,
            raw.client_secret.expose(),
            query.code.as_deref().unwrap_or_default(),
            &pending.redirect_uri,
        )
        .await
    {
        Ok(grant) => grant,
        Err(cause) => {
            tracing::warn!(?cause, "spotify token exchange failed");
            return Redirect::temporary(&profile("/profile?spotify=error&reason=token"))
                .into_response();
        }
    };
    let me = match deps.spotify.profile(&grant.access_token).await {
        Ok(me) => me,
        Err(cause) => {
            tracing::warn!(?cause, "spotify callback /me failed");
            return Redirect::temporary(&profile("/profile?spotify=error&reason=network"))
                .into_response();
        }
    };
    let (username, spotify_user_id) = match me {
        Some(me) => (me.display_name.unwrap_or_else(|| me.id.clone()), me.id),
        None => ("Spotify".to_owned(), String::new()),
    };
    let link = SpotifyConnection {
        access_token: grant.access_token,
        refresh_token: grant.refresh_token.unwrap_or_default(),
        expires_at_unix: now_unix_secs(deps.spotify_links.clock().as_ref()) + grant.expires_in_secs,
        username,
        spotify_user_id,
    };
    if let Err(cause) = deps.spotify_links.upsert(&pending.user_id, &link).await {
        tracing::error!(%cause, "spotify link write failed");
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    }
    Redirect::temporary(&profile("/profile?spotify=connected")).into_response()
}

/// Unlink the caller's Spotify account.
/// Idempotent: no link reads as done too.
#[utoipa::path(delete, tag = "imports", path = "/api/v3/me/connections/spotify",
    responses(
        (status = 204, description = "Unlinked"),
        (status = 401, description = "Not authenticated"),
    ))]
pub async fn disconnect_spotify(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> Result<StatusCode, ApiError> {
    let removed = deps
        .spotify_links
        .remove(principal.user_id())
        .await
        .map_err(|cause| ImportsError::internal(&cause))?;
    if removed {
        tracing::info!(user_id = %principal.user_id(), "spotify link removed");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The caller's owned Spotify playlists. Unlinked reads as a 400,
/// upstream failures as a 502.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/playlists",
    responses((status = 200, description = "Owned playlists", body = SpotifyPlaylistListResponse)))]
pub async fn list_spotify_playlists(
    State(deps): State<ImportsDeps>,
    User(principal): User,
) -> Result<Json<SpotifyPlaylistListResponse>, ApiError> {
    let playlists = deps
        .spotify_service()
        .list_playlists(principal.user_id())
        .await
        .map_err(spotify_failed)?;
    Ok(Json(playlists))
}

/// Map a Spotify service failure to the wire (400 not-linked, 502
/// upstream); store failures are server faults.
fn spotify_failed(error: SpotifyError) -> ImportsError {
    match error {
        SpotifyError::NotLinked => ImportsError::NotConfigured {
            message: "Spotify account not linked".to_owned(),
        },
        SpotifyError::Unavailable(detail) => ImportsError::Unavailable {
            cause: format!("spotify playlist list failed: {detail}"),
        },
        SpotifyError::Store(cause) => ImportsError::internal(&cause),
        SpotifyError::Busy => ImportsError::Busy {
            cause: "spotify store busy".to_owned(),
        },
    }
}

/// Map a jobs table failure to the wire, keeping busy a 503.
fn job_failed(error: JobError) -> ImportsError {
    if error.is_busy() {
        ImportsError::Busy {
            cause: error.to_string(),
        }
    } else {
        ImportsError::internal(&error)
    }
}

/// Longest Spotify playlist id an import accepts. Spotify's are 22
/// characters; the bound keeps the job key within what the jobs table
/// holds.
const MAX_SPOTIFY_ID: usize = 64;

/// Start a Spotify playlist import: ensure the internal record, queue the
/// import job unless one for this playlist is already queued or running,
/// and answer the id at once.
#[utoipa::path(post, tag = "imports", path = "/api/v3/acquire/spotify/playlists/{id}/import",
    params(("id" = String, Path, description = "Spotify playlist id")),
    request_body = SpotifyImportRequest,
    responses((status = 200, description = "Import acknowledgement", body = SpotifyImportResponse)))]
pub async fn import_spotify_playlist(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<SpotifyImportRequest>,
) -> Result<Json<SpotifyImportResponse>, ApiError> {
    if id.trim().is_empty() || id.chars().count() > MAX_SPOTIFY_ID {
        return Err(ImportsError::InvalidInput {
            message: "Spotify playlist id is not valid".to_owned(),
        }
        .into());
    }
    let playlist_id = deps
        .spotify_service()
        .ensure_playlist_record(principal.user_id(), &id, &body.name)
        .await
        .map_err(spotify_failed)?;
    deps.spotify_imports
        .start(principal.user_id(), &id, &playlist_id)
        .await
        .map_err(job_failed)?;
    Ok(Json(SpotifyImportResponse { playlist_id }))
}

/// Latest known state of one Spotify playlist import.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/jobs/{id}",
    params(("id" = String, Path, description = "Spotify playlist id the import runs for")),
    responses((status = 200, description = "Job state", body = SpotifyJobStatus)))]
pub async fn get_spotify_job(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(id): Path<String>,
) -> Result<Json<SpotifyJobStatus>, ApiError> {
    let user_id = principal.user_id();
    let Some(state) = deps
        .spotify_imports
        .state(user_id, &id)
        .await
        .map_err(|error| job_failed(error.into()))?
    else {
        return Err(ImportsError::NotFound.into());
    };
    // The import fills the record its start made; a record deleted since
    // leaves nothing to report.
    let Some(playlist_id) = deps
        .spotify_playlists
        .get_by_source_ref(&format!("spotify:{id}"), user_id)
        .await
        .map_err(|cause| ImportsError::internal(&cause))?
    else {
        return Err(ImportsError::NotFound.into());
    };
    let status = match state {
        ImportState::Running => SpotifyJobStatus {
            state: "running".to_owned(),
            playlist_id,
            track_count: None,
            message: None,
        },
        ImportState::Done => {
            let tracks = deps
                .spotify_playlists
                .track_count(&playlist_id)
                .await
                .map_err(|cause| ImportsError::internal(&cause))?;
            SpotifyJobStatus {
                state: "done".to_owned(),
                playlist_id,
                track_count: Some(i64::try_from(tracks).unwrap_or(i64::MAX)),
                message: None,
            }
        }
        ImportState::Error { message } => SpotifyJobStatus {
            state: "error".to_owned(),
            playlist_id,
            track_count: None,
            message: Some(message),
        },
    };
    Ok(Json(status))
}

/// Acquisition health smoke: whether a download source is on, plus the
/// independent per-source release gates.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/health",
    responses((status = 200, description = "Acquisition health", body = AcquireHealth)))]
pub async fn get_health(State(deps): State<ImportsDeps>, _user: User) -> Json<AcquireHealth> {
    Json(super::health::smoke(&deps.probes, &deps.readiness.now()))
}

/// Live slskd client status (any signed-in user may read it).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/slskd/status",
    responses((status = 200, description = "slskd status", body = SlskdStatusResponse)))]
pub async fn get_slskd_status(
    State(deps): State<ImportsDeps>,
    _user: User,
) -> Json<SlskdStatusResponse> {
    Json(super::health::slskd_status(deps.probes.slskd.as_ref()))
}

/// Live SABnzbd status against the saved config (admin only).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/sabnzbd/status",
    responses((status = 200, description = "SABnzbd status", body = SabnzbdStatusResponse)))]
pub async fn get_sabnzbd_status(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Json<SabnzbdStatusResponse> {
    Json(super::health::sabnzbd_status(deps.probes.sabnzbd.as_ref()))
}

/// Fresh OAuth state token: 32 random bytes, base64url, unpadded.
/// Falls back to a request id when the OS
/// RNG is unavailable, so auth never hard-fails on entropy.
fn fresh_state(deps: &ImportsDeps) -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        return deps.ids.new_id();
    }
    URL_SAFE_NO_PAD.encode(bytes)
}
