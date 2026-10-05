//! Favorites, played markers and playback reporting.

use crate::auth::compat_auth::jellyfin::JellyfinPasswordStore;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;

use super::builders::{self};
use super::models::{PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo, UserItemDataDto};
use super::router::*;
use super::seams::{
    IdMap, LibraryRead, PlaybackSessions, StreamEngine, TICKS_PER_SECOND, should_scrobble,
};

// ===== Favorites + played (both dialects, 200 UserItemDataDto) =====

pub(super) fn marker(item_id: &str, is_favorite: bool, played: bool) -> UserItemDataDto {
    UserItemDataDto {
        item_id: item_id.to_owned(),
        key: item_id.to_owned(),
        playback_position_ticks: 0,
        play_count: 0,
        is_favorite,
        played,
        last_played_date: None,
        rating: None,
        played_percentage: None,
    }
}

pub(super) async fn set_favorite<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    item_id: &str,
    add: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let Some((kind, internal)) = state.ids.from_jf(item_id).await else {
        return error(StatusCode::NOT_FOUND);
    };
    if kind != "artist" && kind != "album" && kind != "track" {
        return error(StatusCode::BAD_REQUEST);
    }
    state
        .library
        .set_favorite(user_id, &kind, &internal, add)
        .await;
    json(StatusCode::OK, &marker(item_id, add, false))
}

pub(super) async fn set_played<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    item_id: &str,
    played: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    // Marker only: play counting goes via Sessions/Playing/Stopped, so this
    // writes nothing (avoids unwanted scrobble forwards, v2 `_set_played`).
    if state.ids.from_jf(item_id).await.is_none() {
        return error(StatusCode::NOT_FOUND);
    }
    json(StatusCode::OK, &marker(item_id, false, played))
}

pub(super) async fn favorite_add_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |ctx| {
        set_favorite(&state, &ctx.principal.id, &item_id, true).await
    })
}

pub(super) async fn favorite_add_modern<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |ctx| {
        set_favorite(&state, &ctx.principal.id, &item_id, true).await
    })
}

pub(super) async fn favorite_remove_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |ctx| {
        set_favorite(&state, &ctx.principal.id, &item_id, false).await
    })
}

pub(super) async fn favorite_remove_modern<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |ctx| {
        set_favorite(&state, &ctx.principal.id, &item_id, false).await
    })
}

pub(super) async fn played_add_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, true)
        .await)
}

pub(super) async fn played_add_modern<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, true)
        .await)
}

pub(super) async fn played_remove_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, false)
        .await)
}

pub(super) async fn played_remove_modern<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, false)
        .await)
}

// ===== Playback reporting / scrobbling (all 204, lenient bodies) =====

/// Item id → file id for session reports (`None` for unknown/non-track,
/// v2 `_track_from_item_id`).
pub(super) async fn report_file_id<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    item_id: &str,
) -> Option<String>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (kind, internal) = state.ids.from_jf(item_id).await?;
    (kind == "track").then_some(internal)
}

pub(super) async fn read_body_lenient<T: serde::de::DeserializeOwned>(
    request: Request<Body>,
) -> (HeaderMap, Option<String>, Option<T>) {
    let (parts, body) = request.into_parts();
    let query = parts.uri.query().map(str::to_owned);
    let parsed = axum::body::to_bytes(body, 64 * 1024)
        .await
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok());
    (parts.headers, query, parsed)
}

/// Start → presence + now-playing (v2 `_playing_start`).
pub(super) async fn playing<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let (headers, query, body) = read_body_lenient::<PlaybackStartInfo>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let Some(body) = body else {
        return no_content();
    };
    let Some(item_id) = body.item_id.as_deref() else {
        return no_content();
    };
    let key = body.play_session_id.as_deref().or(Some(item_id));
    if let Some(key) = key {
        state.sessions.mark_started(&authed.principal.id, key).await;
    }
    if let Some(file_id) = report_file_id(&state, item_id).await {
        state
            .sessions
            .now_playing(&authed.principal.id, &file_id, authed.client.as_deref())
            .await;
    }
    no_content()
}

/// Progress → live scrubber + heartbeat, never a scrobble (v2
/// `_playing_progress`).
pub(super) async fn playing_progress<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let (headers, query, body) = read_body_lenient::<PlaybackProgressInfo>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let Some(body) = body else {
        return no_content();
    };
    let Some(item_id) = body.item_id.as_deref() else {
        return no_content();
    };
    if let Some(file_id) = report_file_id(&state, item_id).await {
        let position_ms = body.position_ticks.map(|t| t / (TICKS_PER_SECOND / 1000));
        state
            .sessions
            .progress(&authed.principal.id, &file_id, position_ms, body.is_paused)
            .await;
    }
    no_content()
}

/// Stop → always drop presence, then scrobble only if past the threshold (v2
/// `_playing_stopped`). `Failed` stops skip the scrobble.
pub(super) async fn playing_stopped<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let (headers, query, body) = read_body_lenient::<PlaybackStopInfo>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    // Drop presence even when the play never reaches the threshold below,
    // or it lingers until TTL.
    state
        .sessions
        .clear_presence(&authed.principal.id, authed.client.as_deref())
        .await;
    let Some(body) = body else {
        return no_content();
    };
    if body.failed {
        return no_content();
    }
    let Some(item_id) = body.item_id.as_deref() else {
        return no_content();
    };
    let Some(file_id) = report_file_id(&state, item_id).await else {
        return no_content();
    };
    let track = state.library.track(&authed.principal.id, &file_id).await;
    let runtime = body.run_time_ticks.or_else(|| {
        track
            .as_ref()
            .and_then(|t| builders::ticks(t.duration_seconds))
    });
    if !should_scrobble(body.position_ticks, runtime) {
        return no_content();
    }
    let key = body
        .play_session_id
        .as_deref()
        .or(Some(item_id))
        .unwrap_or("");
    state.sessions.pop_started(&authed.principal.id, key).await;
    state
        .sessions
        .scrobble(&authed.principal.id, &file_id, authed.client.as_deref())
        .await;
    no_content()
}

pub(super) async fn sessions_ping<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| no_content())
}

/// No session registry stores reported capabilities; accept and 204 to
/// avoid a 404 (v2 `_handle` lambda).
pub(super) async fn sessions_capabilities<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| no_content())
}
