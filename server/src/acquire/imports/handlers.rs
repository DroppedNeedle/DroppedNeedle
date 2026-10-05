//! Native `/api/v3` import + health handlers and the router constructor.
//!
//! Handlers are thin: extract the caller, call one service, render the
//! shape. Status mapping lives in [`ImportsError`](super::error::ImportsError);
//! query strings parse through [`ValidQuery`] and bodies through [`ValidJson`]
//! so malformed input stays inside the shared envelope. The integrator
//! mounts [`imports_router`] inside the session gate; every handler except
//! the Spotify OAuth callback also takes a slice-local user extractor, so
//! every other route 401s anonymously (the callback is state-token
//! identified, exactly like v2).

use std::sync::Arc;

use crate::auth::session::extract::Transport;
use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::UsersDeps;
use crate::auth::users::roles::{AuthContext, Role};
use crate::auth::users::stores::StoreError as UserStoreError;
use crate::ids::IdGenerator;
use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use utoipa::IntoParams;

use super::error::{ImportsError, ValidJson, ValidQuery};
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
    AlbumMbidResolver, PlaylistIndex, PlaylistTrackSink, SpotifyClient, SpotifyConnection,
    SpotifyConnectionStore, SpotifyError, SpotifyImportService, SpotifySettingsStore,
    SpotifyStateStore, now_unix_secs, redirect_uri,
};

/// Every dependency this slice needs, injected by constructor.
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
    /// Auth bundle, used only to resolve the caller.
    pub auth: UsersDeps,
    /// Fresh ids for 5xx error ids.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment base path (between origin and callback, exactly once).
    pub base_path: String,
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

/// The gated routes plus the OAuth callback in one router, for tests. The
/// app mounts [`imports_gated_router`] inside the session gate and
/// [`imports_callback_router`] outside it.
#[cfg(any(test, feature = "test-support"))]
pub fn imports_router(deps: ImportsDeps) -> axum::Router {
    imports_gated_router(deps.clone()).merge(imports_callback_router(deps))
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

/// Any authenticated user. Missing session or a session whose account is
/// gone reads as 401, mirroring the sibling role extractors.
pub struct ImportsUser(pub AuthContext);

impl FromRequestParts<ImportsDeps> for ImportsUser {
    type Rejection = ImportsError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &ImportsDeps,
    ) -> Result<Self, Self::Rejection> {
        let ctx = auth_context(parts, state).await?;
        Ok(Self(ctx))
    }
}

/// Admin-only caller (v2 owner decision 2026-09-07: the Lidarr import and
/// the Spotify/SABnzbd settings surfaces are admin-only). Lesser roles
/// read as 403.
pub struct ImportsAdmin(pub AuthContext);

impl FromRequestParts<ImportsDeps> for ImportsAdmin {
    type Rejection = ImportsError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &ImportsDeps,
    ) -> Result<Self, Self::Rejection> {
        let ctx = auth_context(parts, state).await?;
        if ctx.role != Role::Admin {
            return Err(ImportsError::Forbidden {
                message: "Admin access required".to_owned(),
            });
        }
        Ok(Self(ctx))
    }
}

/// Resolve the caller from the session the gate authenticated.
async fn auth_context(
    parts: &mut axum::http::request::Parts,
    state: &ImportsDeps,
) -> Result<AuthContext, ImportsError> {
    let session = parts
        .extensions
        .get::<CurrentSession>()
        .ok_or(ImportsError::Unauthorized {
            message: "Authentication required".to_owned(),
        })?;
    let user = state
        .auth
        .users
        .get_by_id(&session.user_id)
        .await
        .map_err(|error| match error {
            UserStoreError::Conflict => ImportsError::InvalidInput {
                message: "Conflicting state".to_owned(),
            },
            UserStoreError::Internal(cause) => ImportsError::internal(&cause, state.ids.as_ref()),
        })?
        .ok_or(ImportsError::Unauthorized {
            message: "Authentication required".to_owned(),
        })?;
    Ok(AuthContext {
        user_id: user.id,
        username: user.username_display.or(user.username),
        role: user.role,
        session_id: session.session_id.clone(),
        session_kind: session.kind,
        via_cookie: session.transport == Transport::Cookie,
    })
}

/// Map a Lidarr service failure to the wire. Upstream detail reaches the
/// log only; callers get a fixed user-safe summary.
fn lidarr_failed(error: ServiceError) -> ImportsError {
    match error {
        ServiceError::NotConnected => ImportsError::NotConfigured {
            message: LIDARR_NOT_CONNECTED.to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Auth) => ImportsError::AuthFailed {
            message: "Lidarr rejected the stored API key; check Settings".to_owned(),
        },
        ServiceError::Lidarr(LidarrError::Unavailable(detail)) => {
            tracing::warn!(%detail, "lidarr import failed");
            ImportsError::Unavailable {
                message: "Lidarr answered with an error".to_owned(),
            }
        }
    }
}

/// Masked Lidarr connection settings.
#[utoipa::path(get, path = "/api/v3/acquire/lidarr-import/config",
    responses((status = 200, description = "Masked Lidarr settings", body = LidarrConnectionSettings)))]
pub async fn get_lidarr_config(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
) -> Json<LidarrConnectionSettings> {
    Json(deps.lidarr_settings.get())
}

/// Save the Lidarr connection. A masked key preserves the stored one.
#[utoipa::path(put, path = "/api/v3/acquire/lidarr-import/config",
    responses((status = 200, description = "Saved Lidarr settings", body = LidarrConnectionSettings)))]
pub async fn put_lidarr_config(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
    ValidJson(body): ValidJson<LidarrConnectionSettings>,
) -> Json<LidarrConnectionSettings> {
    deps.lidarr_settings.save(&body);
    Json(deps.lidarr_settings.get())
}

/// Test submitted Lidarr credentials against `system/status` (v2 Test
/// route): the SUBMITTED url/key are probed so Test works before the first
/// save, a masked key resolves to the stored one, and reachable/bad-key
/// verdicts travel in the body, never as a leaked 5xx, never echoing the
/// URL or host.
#[utoipa::path(post, path = "/api/v3/acquire/lidarr-import/test",
    responses((status = 200, description = "Lidarr probe verdict", body = LidarrTestResponse)))]
pub async fn test_lidarr(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
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
#[utoipa::path(get, path = "/api/v3/acquire/lidarr-import/artists",
    responses((status = 200, description = "Import candidates", body = LidarrArtistListResponse)))]
pub async fn list_lidarr_artists(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(ctx): ImportsAdmin,
) -> Result<Json<LidarrArtistListResponse>, ImportsError> {
    deps.lidarr_service()
        .list_candidates(&ctx.user_id)
        .await
        .map(Json)
        .map_err(lidarr_failed)
}

/// Import selected Lidarr artists into the caller's follows.
#[utoipa::path(post, path = "/api/v3/acquire/lidarr-import/import",
    responses((status = 200, description = "Import summary", body = LidarrImportResponse)))]
pub async fn import_lidarr(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(ctx): ImportsAdmin,
    ValidJson(body): ValidJson<LidarrImportRequest>,
) -> Result<Json<LidarrImportResponse>, ImportsError> {
    // ImportsAdmin guarantees an admin caller, so the flag is always true
    // here; the service keeps it for a future non-admin import route whose
    // auto-download mirror needs an approval batch instead.
    deps.lidarr_service()
        .import_artists(&ctx.user_id, ctx.role == Role::Admin, &body.selected_mbids)
        .await
        .map(Json)
        .map_err(lidarr_failed)
}

/// Masked Spotify app settings.
#[utoipa::path(get, path = "/api/v3/acquire/spotify/settings",
    responses((status = 200, description = "Masked Spotify settings", body = SpotifySettings)))]
pub async fn get_spotify_settings(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
) -> Json<SpotifySettings> {
    Json(deps.spotify_settings.get())
}

/// Save the Spotify app settings. A masked secret preserves the stored
/// one; a non-absolute redirect origin is a 400 (v2 text kept).
#[utoipa::path(put, path = "/api/v3/acquire/spotify/settings",
    responses((status = 200, description = "Saved Spotify settings", body = SpotifySettings)))]
pub async fn put_spotify_settings(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
    ValidJson(body): ValidJson<SpotifySettings>,
) -> Result<Json<SpotifySettings>, ImportsError> {
    deps.spotify_settings
        .save(&body)
        .map_err(|message| ImportsError::InvalidInput { message })?;
    Ok(Json(deps.spotify_settings.get()))
}

/// The computed OAuth redirect URI (v2 admin display endpoint).
#[utoipa::path(get, path = "/api/v3/acquire/spotify/redirect-uri",
    responses((status = 200, description = "OAuth redirect URI", body = SpotifyRedirectUri)))]
pub async fn get_spotify_redirect_uri(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
    headers: HeaderMap,
) -> Json<SpotifyRedirectUri> {
    let origin = deps.spotify_settings.get_raw().spotify_redirect_origin;
    Json(SpotifyRedirectUri {
        redirect_uri: redirect_uri(&origin, &request_base(&headers), &deps.base_path),
    })
}

/// Authorize URL for the caller's Spotify link flow. Requires the admin
/// app to be enabled and complete (v2 400 text kept).
#[utoipa::path(get, path = "/api/v3/acquire/spotify/auth/url",
    responses((status = 200, description = "Authorize URL", body = SpotifyAuthUrlResponse)))]
pub async fn get_spotify_auth_url(
    State(deps): State<ImportsDeps>,
    ImportsUser(ctx): ImportsUser,
    headers: HeaderMap,
) -> Result<Json<SpotifyAuthUrlResponse>, ImportsError> {
    let origin = deps.spotify_settings.get_raw().spotify_redirect_origin;
    let uri = redirect_uri(&origin, &request_base(&headers), &deps.base_path);
    let state = fresh_state(&deps);
    let response = deps
        .spotify_service()
        .auth_url(&uri, &state)
        .map_err(|message| ImportsError::InvalidInput { message })?;
    deps.spotify_states.store_state(&state, &ctx.user_id);
    Ok(Json(response))
}

/// OAuth callback query (v2: `code`, `state`, `error`, all optional).
#[derive(Debug, Clone, Deserialize, IntoParams)]
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
#[utoipa::path(get, path = "/api/v3/acquire/spotify/auth/callback",
    responses((status = 307, description = "Redirect to the profile page")))]
pub async fn spotify_callback(
    State(deps): State<ImportsDeps>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<SpotifyCallbackQuery>,
) -> Response {
    let profile = |suffix: &str| format!("{}{suffix}", deps.base_path);
    if query.error.is_some() || query.code.is_none() || query.state.is_none() {
        return Redirect::temporary(&profile("/profile?spotify=error")).into_response();
    }
    let Some(user_id) = deps
        .spotify_states
        .consume_state(query.state.as_deref().unwrap_or_default())
    else {
        return Redirect::temporary(&profile("/profile?spotify=error&reason=state"))
            .into_response();
    };
    let raw = deps.spotify_settings.get_raw();
    let uri = redirect_uri(
        &raw.spotify_redirect_origin,
        &request_base(&headers),
        &deps.base_path,
    );
    let grant = match deps
        .spotify
        .exchange_code(
            &raw.client_id,
            &raw.client_secret,
            query.code.as_deref().unwrap_or_default(),
            &uri,
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
        Ok(response) => match response.bytes().await {
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
    deps.spotify_links.upsert(
        &user_id,
        &SpotifyConnection {
            access_token: grant.access_token,
            refresh_token: grant.refresh_token.unwrap_or_default(),
            expires_at_unix: now_unix_secs() + grant.expires_in_secs,
            username,
            spotify_user_id,
        },
    );
    Redirect::temporary(&profile("/profile?spotify=connected")).into_response()
}

/// The caller's owned Spotify playlists. Unlinked reads as the v2 400;
/// upstream failures as the v2 502.
#[utoipa::path(get, path = "/api/v3/acquire/spotify/playlists",
    responses((status = 200, description = "Owned playlists", body = SpotifyPlaylistListResponse)))]
pub async fn list_spotify_playlists(
    State(deps): State<ImportsDeps>,
    ImportsUser(ctx): ImportsUser,
) -> Result<Json<SpotifyPlaylistListResponse>, ImportsError> {
    deps.spotify_service()
        .list_playlists(&ctx.user_id)
        .await
        .map(Json)
        .map_err(|error| match error {
            SpotifyError::NotLinked => ImportsError::NotConfigured {
                message: "Spotify account not linked".to_owned(),
            },
            SpotifyError::Unavailable(detail) => {
                tracing::warn!(%detail, "spotify playlist list failed");
                ImportsError::Unavailable {
                    message: "Failed to fetch playlists from Spotify".to_owned(),
                }
            }
        })
}

/// Start a Spotify playlist import: ensure the internal record, queue the
/// `spotify:import` durable job unless it already runs, and answer the id
/// immediately (v2 answer-fast shape).
#[utoipa::path(post, path = "/api/v3/acquire/spotify/playlists/{id}/import",
    responses((status = 200, description = "Import acknowledgement", body = SpotifyImportResponse)))]
pub async fn import_spotify_playlist(
    State(deps): State<ImportsDeps>,
    ImportsUser(ctx): ImportsUser,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<SpotifyImportRequest>,
) -> Result<Json<SpotifyImportResponse>, ImportsError> {
    if id.trim().is_empty() {
        return Err(ImportsError::InvalidInput {
            message: "Spotify playlist id must not be empty".to_owned(),
        });
    }
    let playlist_id = deps
        .spotify_service()
        .ensure_playlist_record(&ctx.user_id, &id, &body.name);
    let key = import_job_key(&ctx.user_id, &id);
    if !deps.jobs.is_running(&key) {
        deps.executor.execute(QueuedSpotifyImport {
            key,
            user_id: ctx.user_id,
            spotify_playlist_id: id,
            playlist_id: playlist_id.clone(),
        });
    }
    Ok(Json(SpotifyImportResponse { playlist_id }))
}

/// Latest known state of one `spotify:import` job.
#[utoipa::path(get, path = "/api/v3/acquire/spotify/jobs/{id}",
    responses((status = 200, description = "Job state", body = SpotifyJobStatus)))]
pub async fn get_spotify_job(
    State(deps): State<ImportsDeps>,
    ImportsUser(ctx): ImportsUser,
    Path(id): Path<String>,
) -> Result<Json<SpotifyJobStatus>, ImportsError> {
    let key = import_job_key(&ctx.user_id, &id);
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
        None => Err(ImportsError::NotFound),
    }
}

/// Acquisition health smoke: Free OR slskd OR Usenet readiness plus the
/// independent per-source release gates.
#[utoipa::path(get, path = "/api/v3/acquire/health",
    responses((status = 200, description = "Acquisition health", body = AcquireHealth)))]
pub async fn get_health(
    State(deps): State<ImportsDeps>,
    ImportsUser(_): ImportsUser,
) -> Json<AcquireHealth> {
    Json(super::health::smoke(&deps.probes))
}

/// Live slskd client status (v2: any authenticated user may read it).
#[utoipa::path(get, path = "/api/v3/acquire/slskd/status",
    responses((status = 200, description = "slskd status", body = SlskdStatusResponse)))]
pub async fn get_slskd_status(
    State(deps): State<ImportsDeps>,
    ImportsUser(_): ImportsUser,
) -> Json<SlskdStatusResponse> {
    Json(super::health::slskd_status(deps.probes.slskd.as_ref()))
}

/// Live SABnzbd status against the saved config (v2: admin-only).
#[utoipa::path(get, path = "/api/v3/acquire/sabnzbd/status",
    responses((status = 200, description = "SABnzbd status", body = SabnzbdStatusResponse)))]
pub async fn get_sabnzbd_status(
    State(deps): State<ImportsDeps>,
    ImportsAdmin(_): ImportsAdmin,
) -> Json<SabnzbdStatusResponse> {
    Json(super::health::sabnzbd_status(deps.probes.sabnzbd.as_ref()))
}

/// Request-derived base URL for the redirect-URI fallback (v2
/// `request.base_url`): plain `http` over the Host header. Deployments
/// behind untrusted proxies set the configured origin instead (GH-298).
fn request_base(headers: &HeaderMap) -> String {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
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
