//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the service,
//! render. Status mapping lives in [`PlaybackError`](super::error::PlaybackError).
//! Bodies parse through [`ValidJson`] and query strings through [`ValidQuery`],
//! which keep malformed input inside the shared error envelope instead of
//! Axum's default plain-text 400.

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, State},
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

use super::{
    error::PlaybackError,
    models::{
        NowPlayingDeleteQuery, NowPlayingHeartbeat, NowPlayingSnapshot, PlaybackProgressRequest,
        PlaybackProgressResponse, PlaybackStartRequest, PlaybackStartResponse, PlaybackStopRequest,
        PlaybackStopResponse, ScrobbleNowPlayingRequest, ScrobbleResponse, ScrobbleSubmitRequest,
    },
    services::{self, PlaybackDeps},
};
use droppedneedle::auth::session::middleware::CurrentSession;

/// The authenticated caller. The deny-by-default session middleware stashes
/// the session; this extractor reads it without a store round-trip (all
/// slice routes are plain authenticated-user routes, no role gating).
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// Owning user id.
    pub user_id: String,
    /// Authenticating session id.
    pub session_id: String,
}

impl AuthenticatedUser {
    /// Reject unauthenticated callers with the 401 envelope.
    pub fn require(parts: &Parts) -> Result<Self, PlaybackError> {
        parts
            .extensions
            .get::<CurrentSession>()
            .map(|session| Self {
                user_id: session.user_id.clone(),
                session_id: session.session_id.clone(),
            })
            .ok_or_else(|| PlaybackError::Unauthorized {
                message: "Authentication required".to_owned(),
            })
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedUser {
    type Rejection = PlaybackError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Self::require(parts)
    }
}

/// JSON body extractor that renders failures in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = PlaybackError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| PlaybackError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

/// Query extractor that renders failures in the shared envelope.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = PlaybackError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request(req, state)
            .await
            .map(|axum::extract::Query(value)| Self(value))
            .map_err(|cause| PlaybackError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

fn map(error: services::ServiceError, deps: &PlaybackDeps) -> PlaybackError {
    error.into_playback_error(deps.ids.as_ref())
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

/// Open a playback session for one catalog track.
#[utoipa::path(
    post,
    path = "/api/v3/playback/start",
    request_body = PlaybackStartRequest,
    responses(
        (status = 200, description = "Session opened", body = PlaybackStartResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track"),
    )
)]
pub async fn start_playback(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<PlaybackStartRequest>,
) -> Result<Json<PlaybackStartResponse>, PlaybackError> {
    services::start_playback(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Record a heartbeat for a live session.
#[utoipa::path(
    post,
    path = "/api/v3/playback/progress",
    request_body = PlaybackProgressRequest,
    responses(
        (status = 200, description = "Heartbeat recorded", body = PlaybackProgressResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track"),
    )
)]
pub async fn report_progress(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<PlaybackProgressRequest>,
) -> Result<Json<PlaybackProgressResponse>, PlaybackError> {
    services::report_progress(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Close a session, counting the play past threshold.
#[utoipa::path(
    post,
    path = "/api/v3/playback/stop",
    request_body = PlaybackStopRequest,
    responses(
        (status = 200, description = "Session closed", body = PlaybackStopResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track"),
    )
)]
pub async fn stop_playback(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<PlaybackStopRequest>,
) -> Result<Json<PlaybackStopResponse>, PlaybackError> {
    services::stop_playback(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

// ---------------------------------------------------------------------------
// Native scrobble
// ---------------------------------------------------------------------------

/// Submit one native scrobble by name.
#[utoipa::path(
    post,
    path = "/api/v3/scrobble/submit",
    request_body = ScrobbleSubmitRequest,
    responses(
        (status = 200, description = "Scrobble counted", body = ScrobbleResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn submit_scrobble(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<ScrobbleSubmitRequest>,
) -> Result<Json<ScrobbleResponse>, PlaybackError> {
    services::submit_scrobble(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Forward one native now-playing report to the linked accounts.
#[utoipa::path(
    post,
    path = "/api/v3/scrobble/now-playing",
    request_body = ScrobbleNowPlayingRequest,
    responses(
        (status = 200, description = "Forward outcome", body = ScrobbleResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn forward_now_playing(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<ScrobbleNowPlayingRequest>,
) -> Result<Json<ScrobbleResponse>, PlaybackError> {
    services::forward_sink_now_playing(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// The live now-playing snapshot across users.
#[utoipa::path(
    get,
    path = "/api/v3/now-playing",
    responses(
        (status = 200, description = "Live snapshot", body = NowPlayingSnapshot),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn now_playing(
    State(deps): State<PlaybackDeps>,
    _user: AuthenticatedUser,
) -> Json<NowPlayingSnapshot> {
    Json(services::snapshot(&deps))
}

/// Record a native heartbeat (upsert presence).
#[utoipa::path(
    post,
    path = "/api/v3/now-playing",
    request_body = NowPlayingHeartbeat,
    responses(
        (status = 204, description = "Heartbeat recorded"),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn heartbeat(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<NowPlayingHeartbeat>,
) -> StatusCode {
    services::heartbeat(&deps, &user.user_id, &body);
    StatusCode::NO_CONTENT
}

/// Clear one native device (stop presence).
#[utoipa::path(
    delete,
    path = "/api/v3/now-playing",
    params(("device" = Option<String>, Query, description = "Device slug, `web` default")),
    responses(
        (status = 204, description = "Presence cleared"),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn clear_now_playing(
    State(deps): State<PlaybackDeps>,
    user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<NowPlayingDeleteQuery>,
) -> StatusCode {
    let device = query.device.as_deref().unwrap_or("web");
    services::clear_presence(&deps, &user.user_id, device);
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every route in this slice. Mount under `/api/v3` behind the session
/// gate; see `mod.rs` for the wiring note.
pub fn playback_router(deps: PlaybackDeps) -> axum::Router {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/playback/start", post(start_playback))
        .route("/playback/progress", post(report_progress))
        .route("/playback/stop", post(stop_playback))
        .route("/scrobble/submit", post(submit_scrobble))
        .route("/scrobble/now-playing", post(forward_now_playing))
        .route(
            "/now-playing",
            get(now_playing).post(heartbeat).delete(clear_now_playing),
        )
        .with_state(deps)
}
