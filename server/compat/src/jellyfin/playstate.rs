//! Favorites, played marks and playback reporting.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;

use super::builders::{self};
use super::models::{PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo, UserItemDataDto};
use super::router::*;
use super::seams::{
    IdMap, ItemView, LibraryRead, PlaybackSessions, ReadFailed, StreamEngine, TICKS_PER_SECOND,
    WriteRefusal, should_scrobble,
};

// ===== Favorites + played (both dialects, 200 UserItemDataDto) =====

/// The caller's state of one item as the item's own read gives it, `None`
/// when that read finds nothing (a missing track, say).
async fn item_state<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    user_id: &str,
    kind: &str,
    internal: &str,
) -> Result<Option<UserItemDataDto>, ReadFailed>
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let b = builder(state);
    Ok(match state.library.item(user_id, kind, internal).await? {
        Some(ItemView::Track(track)) => b.audio(&track).await.user_data,
        Some(ItemView::Album(album)) => b.album(&album).await.user_data,
        Some(ItemView::Artist(artist)) => b.artist(&artist).await.user_data,
        None => None,
    })
}

/// Change one item's state for the caller with `write`, then answer the
/// item's state as its own read gives it. An unknown id, or one whose read
/// finds nothing, is `missing` (Jellyfin's 404) and nothing is written; a
/// playlist, genre or library is 400, and a failed read 500. The answer
/// names the item's current id, also when the app sent an old album or
/// artist id.
async fn change_item<L, E, P, I, W>(
    state: &JellyfinState<L, E, P, I>,
    user_id: &str,
    item_id: &str,
    missing: Response,
    write: impl FnOnce(String, String) -> W,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
    W: Future<Output = Result<(), WriteRefusal>>,
{
    let Some((kind, internal)) = state.ids.from_jf(item_id).await else {
        return missing;
    };
    if kind != "artist" && kind != "album" && kind != "track" {
        return error(StatusCode::BAD_REQUEST);
    }
    match item_state(state, user_id, &kind, &internal).await {
        Ok(Some(_)) => {}
        Ok(None) => return missing,
        Err(ReadFailed) => return error(StatusCode::INTERNAL_SERVER_ERROR),
    }
    if let Err(refusal) = write(kind.clone(), internal.clone()).await {
        return refused(refusal);
    }
    match item_state(state, user_id, &kind, &internal).await {
        Ok(Some(answer)) => json(StatusCode::OK, &answer),
        Ok(None) => missing,
        Err(ReadFailed) => error(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(super) async fn set_favorite<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    user_id: &str,
    item_id: &str,
    add: bool,
    missing: Response,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    change_item(
        state,
        user_id,
        item_id,
        missing,
        |kind, internal| async move {
            state
                .library
                .set_favorite(user_id, &kind, &internal, add)
                .await
        },
    )
    .await
}

/// Mark an item played or unplayed for the caller, never anyone else: the
/// user in the legacy path and a `userId` parameter are ignored. Nothing
/// is played, scrobbled or shown as playing.
pub(super) async fn set_played<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    user_id: &str,
    item_id: &str,
    played: bool,
    missing: Response,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    change_item(
        state,
        user_id,
        item_id,
        missing,
        |kind, internal| async move {
            state
                .library
                .set_played(user_id, &kind, &internal, played)
                .await
        },
    )
    .await
}

pub(super) async fn favorite_add_legacy<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_favorite(&state, &ctx.principal.id, &item_id, true, missing).await
}

pub(super) async fn favorite_add_modern<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(item_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_favorite(&state, &ctx.principal.id, &item_id, true, missing).await
}

pub(super) async fn favorite_remove_legacy<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_favorite(&state, &ctx.principal.id, &item_id, false, missing).await
}

pub(super) async fn favorite_remove_modern<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(item_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_favorite(&state, &ctx.principal.id, &item_id, false, missing).await
}

pub(super) async fn played_add_legacy<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_played(&state, &ctx.principal.id, &item_id, true, missing).await
}

pub(super) async fn played_add_modern<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(item_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_played(&state, &ctx.principal.id, &item_id, true, missing).await
}

pub(super) async fn played_remove_legacy<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_played(&state, &ctx.principal.id, &item_id, false, missing).await
}

pub(super) async fn played_remove_modern<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(item_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let missing = not_found_item(&request);
    set_played(&state, &ctx.principal.id, &item_id, false, missing).await
}

// ===== Playback reporting / scrobbling (all 204, lenient bodies) =====

/// Item id → file id for session reports (`None` for unknown/non-track,
/// v2 `_track_from_item_id`).
pub(super) async fn report_file_id<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    item_id: &str,
) -> Option<String>
where
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
) -> (Option<String>, Option<T>) {
    let (parts, body) = request.into_parts();
    let query = parts.uri.query().map(str::to_owned);
    let parsed = axum::body::to_bytes(body, 64 * 1024)
        .await
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok());
    (query, parsed)
}

/// Start → presence + now-playing (v2 `_playing_start`).
pub(super) async fn playing<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    authed: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (_, body) = read_body_lenient::<PlaybackStartInfo>(request).await;
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
pub(super) async fn playing_progress<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    authed: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (_, body) = read_body_lenient::<PlaybackProgressInfo>(request).await;
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
pub(super) async fn playing_stopped<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    authed: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (_, body) = read_body_lenient::<PlaybackStopInfo>(request).await;
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

pub(super) async fn sessions_ping(_caller: Authed) -> Response {
    no_content()
}

/// No session registry stores reported capabilities; accept and 204 to
/// avoid a 404 (v2 `_handle` lambda).
pub(super) async fn sessions_capabilities(_caller: Authed) -> Response {
    no_content()
}
