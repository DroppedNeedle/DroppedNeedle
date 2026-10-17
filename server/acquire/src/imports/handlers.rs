//! Native `/api/v3` import + health handlers and the router constructor.
//!
//! Handlers are thin: extract the caller, call one service, render the
//! shape. Status mapping lives in [`ImportsError`](super::error::ImportsError)'s
//! one mapping onto `ApiError`; query strings parse through [`ValidQuery`] and
//! bodies through [`ValidJson`] so malformed input stays inside the shared
//! envelope. The app mounts [`imports_gated_router`] inside the session gate
//! and the Spotify OAuth callback outside it; every gated handler also takes
//! a user extractor, so those routes 401 anonymously (the callback is
//! identified by its state token, exactly like v2). The callback also
//! answers on its v2 path for Spotify apps registered against v2; either
//! way it exchanges the code with the redirect URI stored next to the state
//! at authorize time.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use droppedneedle_auth::session::middleware::effective_origin;
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::http_client::read_capped;
use droppedneedle_kernel::ids::IdGenerator;
use serde::Deserialize;
use utoipa::IntoParams;

use super::error::ImportsError;
use super::health::HealthProbes;
use super::jobs::{
    JobRegistry, JobState, QueuedSpotifyImport, SpotifyImportExecutor, import_job_key,
};
use super::lidarr::{
    ApprovalSink, FollowStore, LIDARR_NOT_CONNECTED, LidarrClient, LidarrError,
    LidarrImportService, LidarrSettingsStore, ServiceError, normalize_lidarr_url,
};
use super::models::{
    AcquireHealth, LIDARR_API_KEY_MASK, LidarrArtistListResponse, LidarrConnectionSettings,
    LidarrImportRequest, LidarrImportResponse, LidarrTestResponse, SabnzbdStatusResponse,
    SlskdStatusResponse, SpotifyAuthUrlResponse, SpotifyImportRequest, SpotifyImportResponse,
    SpotifyJobStatus, SpotifyPlaylistListResponse, SpotifyRedirectUri, SpotifySettings,
};
use super::spotify::{
    AlbumMbidResolver, MAX_API_BODY_BYTES, PendingAuth, PlaylistIndex, PlaylistTrackSink,
    SPOTIFY_CALLBACK_PATH, SPOTIFY_LEGACY_CALLBACK_PATH, SpotifyClient, SpotifyConnection,
    SpotifyConnectionStore, SpotifyError, SpotifyImportService, SpotifySettingsStore,
    SpotifyStateStore, now_unix_secs, redirect_uri,
};

/// Every dependency the imports routes need, injected by constructor.
#[derive(Clone)]
pub struct ImportsDeps {
    /// Shared outbound HTTP client.
    pub http: reqwest::Client,
    /// Read-only Lidarr client (the two sanctioned GETs).
    pub lidarr: LidarrClient,
    /// Lidarr connection rows.
    pub lidarr_settings: Arc<dyn LidarrSettingsStore>,
    /// Follow rows the import writes.
    pub follows: Arc<dyn FollowStore>,
    /// Approval batches for non-admin mirrors.
    pub approvals: Arc<dyn ApprovalSink>,
    /// Spotify API client over injected bases.
    pub spotify: SpotifyClient,
    /// Spotify app settings rows.
    pub spotify_settings: Arc<dyn SpotifySettingsStore>,
    /// Single-use OAuth states.
    pub spotify_states: Arc<dyn SpotifyStateStore>,
    /// Per-user Spotify links.
    pub spotify_links: Arc<dyn SpotifyConnectionStore>,
    /// Internal playlist index (`spotify:{id}` refs).
    pub playlists: Arc<dyn PlaylistIndex>,
    /// Imported track rows.
    pub tracks: Arc<dyn PlaylistTrackSink>,
    /// Album-to-MBID resolver.
    pub resolver: Arc<dyn AlbumMbidResolver>,
    /// `spotify:import` job registry.
    pub jobs: Arc<JobRegistry>,
    /// Downloads-seam executor running the populates.
    pub executor: Arc<dyn SpotifyImportExecutor>,
    /// Health probes behind the smoke and gates.
    pub probes: Arc<HealthProbes>,
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
            self.approvals.clone(),
        )
    }

    /// Spotify import service over the wired stores.
    fn spotify_service(&self) -> SpotifyImportService {
        SpotifyImportService::new(
            self.spotify.clone(),
            self.spotify_settings.clone(),
            self.spotify_links.clone(),
            self.playlists.clone(),
            self.tracks.clone(),
            self.resolver.clone(),
        )
    }
}

/// The gated routes plus both OAuth callbacks in one router, for tests. The
/// app mounts [`imports_gated_router`] inside the session gate and the
/// callbacks outside it.
#[cfg(any(test, feature = "test-support"))]
pub fn imports_router(deps: ImportsDeps) -> axum::Router {
    imports_gated_router(deps.clone())
        .merge(imports_callback_router(deps.clone()))
        .merge(imports_legacy_callback_router(deps))
}

/// Session-gated routes. Paths are relative: the app nests this under
/// `/api/v3` inside the deny-by-default session gate.
pub fn imports_gated_router(deps: ImportsDeps) -> axum::Router {
    axum::Router::new()
        .route(
            "/acquire/lidarr-import/config",
            get(get_lidarr_config).put(put_lidarr_config),
        )
        .route("/acquire/lidarr-import/test", post(test_lidarr))
        .route("/acquire/lidarr-import/artists", get(list_lidarr_artists))
        .route("/acquire/lidarr-import/import", post(import_lidarr))
        .route(
            "/acquire/spotify/settings",
            get(get_spotify_settings).put(put_spotify_settings),
        )
        .route(
            "/acquire/spotify/redirect-uri",
            get(get_spotify_redirect_uri),
        )
        .route("/acquire/spotify/auth/url", get(get_spotify_auth_url))
        .route("/acquire/spotify/playlists", get(list_spotify_playlists))
        .route(
            "/acquire/spotify/playlists/{id}/import",
            post(import_spotify_playlist),
        )
        .route("/acquire/spotify/jobs/{id}", get(get_spotify_job))
        .route("/me/connections/spotify", delete(disconnect_spotify))
        .route("/acquire/health", get(get_health))
        .route("/acquire/slskd/status", get(get_slskd_status))
        .route("/acquire/sabnzbd/status", get(get_sabnzbd_status))
        .with_state(deps)
}

/// Spotify OAuth callback, mounted outside the session gate: it is
/// state-token identified, exactly like v2's ungated route.
pub fn imports_callback_router(deps: ImportsDeps) -> axum::Router {
    axum::Router::new()
        .route("/acquire/spotify/auth/callback", get(spotify_callback))
        .with_state(deps)
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
        ServiceError::Approvals(cause) => {
            ImportsError::internal(&format_args!("approval batch failed: {cause}"))
        }
        ServiceError::NotConnected => ImportsError::NotConfigured {
            message: LIDARR_NOT_CONNECTED.to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Auth) => ImportsError::AuthFailed {
            cause: "Lidarr rejected the stored API key".to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Unavailable(detail)) => ImportsError::Unavailable {
            cause: format!("lidarr import failed: {detail}"),
        },
        ServiceError::Follows(cause) => {
            ImportsError::internal(&format_args!("lidarr import follow write failed: {cause}"))
        }
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
    deps.lidarr_settings.save(&body);
    Json(deps.lidarr_settings.get())
}

/// Test submitted Lidarr credentials against `system/status` (v2 Test
/// route): the submitted url/key are probed so Test works before the first
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
    if api_key == LIDARR_API_KEY_MASK {
        api_key = deps.lidarr_settings.get_raw().api_key;
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
/// one; a non-absolute redirect origin is a 400 (v2 text kept).
#[utoipa::path(put, tag = "imports", path = "/api/v3/acquire/spotify/settings",
    request_body = SpotifySettings,
    responses((status = 200, description = "Saved Spotify settings", body = SpotifySettings)))]
pub async fn put_spotify_settings(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<SpotifySettings>,
) -> Result<Json<SpotifySettings>, ApiError> {
    deps.spotify_settings
        .save(&body)
        .map_err(|message| ImportsError::InvalidInput { message })?;
    Ok(Json(deps.spotify_settings.get()))
}

/// The computed OAuth redirect URI (v2 admin display endpoint).
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
/// app to be enabled and complete (v2 400 text kept).
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

/// OAuth callback query (v2: `code`, `state`, `error`, all optional).
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

/// Spotify OAuth callback (v2 `spotify_auth_callback`). State-identified,
/// so it sits outside the session gate like v2's ungated route: every
/// outcome redirects to the profile page with the v2 query contract
/// (`spotify=connected`, or `spotify=error` with an optional `reason`).
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
            &raw.client_secret,
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
    let me = match deps
        .http
        .get(format!("{}/me", deps.spotify.api_base()))
        .header("Authorization", format!("Bearer {}", grant.access_token))
        .send()
        .await
    {
        Ok(response) => match read_capped(response, MAX_API_BODY_BYTES).await {
            Ok(body) => serde_json::from_slice::<serde_json::Value>(&body)
                .unwrap_or(serde_json::Value::Null),
            Err(_) => serde_json::Value::Null,
        },
        Err(cause) => {
            tracing::warn!(%cause, "spotify callback /me failed");
            return Redirect::temporary(&profile("/profile?spotify=error&reason=network"))
                .into_response();
        }
    };
    let username = me
        .get("display_name")
        .and_then(|name| name.as_str())
        .or_else(|| me.get("id").and_then(|id| id.as_str()))
        .unwrap_or("Spotify")
        .to_owned();
    let spotify_user_id = me
        .get("id")
        .and_then(|id| id.as_str())
        .unwrap_or("")
        .to_owned();
    let link = SpotifyConnection {
        access_token: grant.access_token,
        refresh_token: grant.refresh_token.unwrap_or_default(),
        expires_at_unix: now_unix_secs() + grant.expires_in_secs,
        username,
        spotify_user_id,
    };
    if let Err(cause) = deps.spotify_links.upsert(&pending.user_id, &link).await {
        tracing::error!(%cause, "spotify link write failed");
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    }
    Redirect::temporary(&profile("/profile?spotify=connected")).into_response()
}

/// Unlink the caller's Spotify account (v2 `DELETE /me/connections/spotify`).
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

/// The caller's owned Spotify playlists. Unlinked reads as the v2 400;
/// upstream failures as the v2 502.
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

/// Map a Spotify service failure to the wire (v2 400 not-linked, 502
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
    }
}

/// Start a Spotify playlist import: ensure the internal record, queue the
/// `spotify:import` durable job unless it already runs, and answer the id
/// immediately (v2 answer-fast shape).
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
    if id.trim().is_empty() {
        return Err(ImportsError::InvalidInput {
            message: "Spotify playlist id must not be empty".to_owned(),
        }
        .into());
    }
    let playlist_id = deps
        .spotify_service()
        .ensure_playlist_record(principal.user_id(), &id, &body.name)
        .await
        .map_err(spotify_failed)?;
    let key = import_job_key(principal.user_id(), &id);
    if !deps.jobs.is_running(&key) {
        deps.executor.execute(QueuedSpotifyImport {
            key,
            user_id: principal.user_id().to_owned(),
            spotify_playlist_id: id,
            playlist_id: playlist_id.clone(),
        });
    }
    Ok(Json(SpotifyImportResponse { playlist_id }))
}

/// Latest known state of one `spotify:import` job.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/spotify/jobs/{id}",
    params(("id" = String, Path, description = "Spotify playlist id the import runs for")),
    responses((status = 200, description = "Job state", body = SpotifyJobStatus)))]
pub async fn get_spotify_job(
    State(deps): State<ImportsDeps>,
    User(principal): User,
    Path(id): Path<String>,
) -> Result<Json<SpotifyJobStatus>, ApiError> {
    let key = import_job_key(principal.user_id(), &id);
    match deps.jobs.state_for(&key) {
        Some(JobState::Running { playlist_id }) => Ok(Json(SpotifyJobStatus {
            state: "running".to_owned(),
            playlist_id,
            track_count: None,
            message: None,
        })),
        Some(JobState::Done {
            playlist_id,
            track_count,
        }) => Ok(Json(SpotifyJobStatus {
            state: "done".to_owned(),
            playlist_id,
            track_count: Some(track_count as i64),
            message: None,
        })),
        Some(JobState::Error {
            playlist_id,
            message,
        }) => Ok(Json(SpotifyJobStatus {
            state: "error".to_owned(),
            playlist_id,
            track_count: None,
            message: Some(message),
        })),
        None => Err(ImportsError::NotFound.into()),
    }
}

/// Acquisition health smoke: slskd or Usenet readiness plus the
/// independent per-source release gates.
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/health",
    responses((status = 200, description = "Acquisition health", body = AcquireHealth)))]
pub async fn get_health(State(deps): State<ImportsDeps>, _user: User) -> Json<AcquireHealth> {
    Json(super::health::smoke(&deps.probes))
}

/// Live slskd client status (v2: any authenticated user may read it).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/slskd/status",
    responses((status = 200, description = "slskd status", body = SlskdStatusResponse)))]
pub async fn get_slskd_status(
    State(deps): State<ImportsDeps>,
    _user: User,
) -> Json<SlskdStatusResponse> {
    Json(super::health::slskd_status(deps.probes.slskd.as_ref()))
}

/// Live SABnzbd status against the saved config (v2: admin-only).
#[utoipa::path(get, tag = "imports", path = "/api/v3/acquire/sabnzbd/status",
    responses((status = 200, description = "SABnzbd status", body = SabnzbdStatusResponse)))]
pub async fn get_sabnzbd_status(
    State(deps): State<ImportsDeps>,
    _admin: Admin,
) -> Json<SabnzbdStatusResponse> {
    Json(super::health::sabnzbd_status(deps.probes.sabnzbd.as_ref()))
}

/// Fresh OAuth state token: 32 random bytes, base64url, unpadded (v2
/// `secrets.token_urlsafe(32)`). Falls back to a request id when the OS
/// RNG is unavailable, so auth never hard-fails on entropy.
fn fresh_state(deps: &ImportsDeps) -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        return deps.ids.new_id();
    }
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(43);
    for chunk in bytes.chunks(3) {
        let mut block: u32 = 0;
        for (index, byte) in chunk.iter().enumerate() {
            block |= (*byte as u32) << (16 - 8 * index);
        }
        for index in 0..4 - (3 - chunk.len()) {
            out.push(ALPHABET[((block >> (18 - 6 * index)) & 0x3F) as usize] as char);
        }
    }
    out
}
