//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the service,
//! render. Status mapping lives in the `From<PlaybackError>` impl for
//! [`ApiError`]. Bodies and query strings parse through the kernel's
//! [`ValidJson`] and [`ValidQuery`].

use axum::{
    Json,
    extract::{FromRef, State},
    http::StatusCode,
};

use super::{
    PlaybackDeps, events,
    handoff::models::{HandoffHeartbeat, HandoffRequest, HandoffUpdate},
    models::{
        NowPlayingDeleteQuery, NowPlayingHeartbeat, PlaybackEventRequest, PlaybackProgressRequest,
        PlaybackProgressResponse, PlaybackStartRequest, PlaybackStartResponse, PlaybackStopRequest,
        PlaybackStopResponse, ScrobbleNowPlayingRequest, ScrobbleResponse, ScrobbleSubmitRequest,
    },
    presence, report,
    rules::ServiceError,
    sessions,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::events::NowPlayingSnapshot;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};

fn map(error: ServiceError) -> ApiError {
    error.into_playback_error().into()
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

/// Open a playback session for one track.
///
/// A Jellyfin, Navidrome or Plex track is reported on to that server under
/// the caller's own linked account, and the answer does not wait for it.
#[utoipa::path(
    post,
    tag = "playback",
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
    User(user): User,
    ValidJson(body): ValidJson<PlaybackStartRequest>,
) -> Result<Json<PlaybackStartResponse>, ApiError> {
    sessions::start_playback(&deps, user.user_id(), &body)
        .map(Json)
        .map_err(map)
}

/// Record a heartbeat for a live session.
#[utoipa::path(
    post,
    tag = "playback",
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
    User(user): User,
    ValidJson(body): ValidJson<PlaybackProgressRequest>,
) -> Result<Json<PlaybackProgressResponse>, ApiError> {
    sessions::report_progress(&deps, user.user_id(), &body)
        .map(Json)
        .map_err(map)
}

/// Close a session, counting a library play past threshold.
#[utoipa::path(
    post,
    tag = "playback",
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
    User(user): User,
    ValidJson(body): ValidJson<PlaybackStopRequest>,
) -> Result<Json<PlaybackStopResponse>, ApiError> {
    sessions::stop_playback(&deps, user.user_id(), &body)
        .map(Json)
        .map_err(map)
}

/// Record how one web-player playback ended. It counts no play: the
/// scrobble submit does that.
#[utoipa::path(
    post,
    tag = "playback",
    path = "/api/v3/playback/events",
    request_body = PlaybackEventRequest,
    responses(
        (status = 204, description = "Event recorded"),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn record_playback_event(
    State(deps): State<PlaybackDeps>,
    User(user): User,
    ValidJson(body): ValidJson<PlaybackEventRequest>,
) -> Result<StatusCode, ApiError> {
    events::submit(&deps, user.user_id(), &body)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(map)
}

// ---------------------------------------------------------------------------
// Native scrobble
// ---------------------------------------------------------------------------

/// Submit one native scrobble by name.
#[utoipa::path(
    post,
    tag = "playback",
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
    User(user): User,
    ValidJson(body): ValidJson<ScrobbleSubmitRequest>,
) -> Result<Json<ScrobbleResponse>, ApiError> {
    report::submit_scrobble(&deps, user.user_id(), &body)
        .map(Json)
        .map_err(map)
}

/// Forward one native now-playing report to the linked accounts.
#[utoipa::path(
    post,
    tag = "playback",
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
    User(user): User,
    ValidJson(body): ValidJson<ScrobbleNowPlayingRequest>,
) -> Result<Json<ScrobbleResponse>, ApiError> {
    report::forward_sink_now_playing(&deps, user.user_id(), &body)
        .map(Json)
        .map_err(map)
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// The live now-playing snapshot across users.
#[utoipa::path(
    get,
    tag = "playback",
    path = "/api/v3/now-playing",
    responses(
        (status = 200, description = "Live snapshot", body = NowPlayingSnapshot),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn now_playing(
    State(deps): State<PlaybackDeps>,
    _user: User,
) -> Json<NowPlayingSnapshot> {
    Json(presence::snapshot(&deps))
}

/// Record a native heartbeat (upsert presence).
#[utoipa::path(
    post,
    tag = "playback",
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
    User(user): User,
    ValidJson(body): ValidJson<NowPlayingHeartbeat>,
) -> StatusCode {
    presence::heartbeat(&deps, user.user_id(), &body);
    StatusCode::NO_CONTENT
}

/// Clear one native device (stop presence).
#[utoipa::path(
    delete,
    tag = "playback",
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
    User(user): User,
    ValidQuery(query): ValidQuery<NowPlayingDeleteQuery>,
) -> StatusCode {
    let device = query.device.as_deref().unwrap_or("web");
    presence::clear_presence(&deps, user.user_id(), device);
    StatusCode::NO_CONTENT
}

/// Sign this player up for handoff, read peers and acknowledge any outstanding transfer.
#[utoipa::path(
    post,
    tag = "playback",
    path = "/api/v3/playback/handoff/sync",
    request_body = HandoffHeartbeat,
    responses(
        (status = 200, description = "Your active players and this player's next step", body = HandoffUpdate),
        (status = 400, description = "Invalid player identity or name"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Player unavailable"),
        (status = 409, description = "Player capacity reached"),
    )
)]
pub async fn sync_handoff(
    State(deps): State<PlaybackDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<HandoffHeartbeat>,
) -> Result<Json<HandoffUpdate>, ApiError> {
    Ok(Json(deps.handoff.sync(
        &caller,
        body,
        deps.clock.now_ms(),
    )?))
}

/// Start a local queue transfer between two of the caller's active players.
#[utoipa::path(
    post,
    tag = "playback",
    path = "/api/v3/playback/handoff",
    request_body = HandoffRequest,
    responses(
        (status = 204, description = "Transfer queued"),
        (status = 400, description = "Invalid transfer"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Player unavailable"),
        (status = 409, description = "Transfer busy or source unavailable"),
    )
)]
pub async fn start_handoff(
    State(deps): State<PlaybackDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<HandoffRequest>,
) -> Result<StatusCode, ApiError> {
    deps.handoff.start(&caller, body, deps.clock.now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every playback route, for any signed-in user.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    PlaybackDeps: FromRef<S>,
{
    Routes::new()
        .user(routes!(sync_handoff))
        .user(routes!(start_handoff))
        .user(routes!(start_playback))
        .user(routes!(report_progress))
        .user(routes!(stop_playback))
        .user(routes!(record_playback_event))
        .user(routes!(submit_scrobble))
        .user(routes!(forward_now_playing))
        .user(routes!(now_playing, heartbeat, clear_now_playing))
}
