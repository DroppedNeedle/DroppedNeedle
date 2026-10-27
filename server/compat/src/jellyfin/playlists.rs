//! Playlist routes over the shared playlists.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;

use super::models::{BaseItemDtoQueryResult, CreatePlaylistDto};
use super::params::{self, CiParams};
use super::playstate::*;
use super::query::page;
use super::router::*;
use super::seams::{IdMap, LibraryRead, PlaybackSessions, StreamEngine};

// ===== Playlists =====

pub(super) async fn decode_playlist<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    jf_id: &str,
) -> Result<String, Response>
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    match state.ids.from_jf(jf_id).await {
        Some((kind, internal)) if kind == "playlist" => Ok(internal),
        _ => Err(error(StatusCode::NOT_FOUND)),
    }
}

/// Create from a JSON body or `?name=`/`?ids=` (v2 `_create_playlist`).
/// Unknown track ids are skipped silently.
pub(super) async fn create_playlist<L, E, P, I>(
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
    let (query, body) = read_body_lenient::<CreatePlaylistDto>(request).await;
    let q = CiParams::parse(query.as_deref());
    let body = body.unwrap_or_default();
    let name = if !body.name.is_empty() {
        body.name
    } else {
        q.get("name").unwrap_or("Playlist").to_owned()
    };
    let track_ids = if !body.ids.is_empty() {
        body.ids
    } else {
        params::ids_param(&q, &["ids"])
    };
    let internal = match state
        .library
        .create_playlist(&authed.principal.id, &name)
        .await
    {
        Ok(internal) => internal,
        Err(refusal) => return refused(refusal),
    };
    for jf_id in &track_ids {
        if let Some(file_id) = report_file_id(&state, jf_id).await
            && let Err(refusal) = state
                .library
                .add_playlist_entry(&authed.principal.id, &internal, &file_id)
                .await
        {
            return refused(refusal);
        }
    }
    json(
        StatusCode::OK,
        &serde_json::json!({ "Id": state.ids.to_jf("playlist", &internal).await }),
    )
}

pub(super) async fn get_playlist<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(playlist_id): Path<String>,
    ctx: Authed,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let detail = match state.library.playlist(&ctx.principal.id, &internal).await {
        Some(detail) => detail,
        None => return error(StatusCode::NOT_FOUND),
    };
    let mut item_ids = Vec::new();
    for entry in &detail.entries {
        if let Some(file_id) = entry.file_id.as_deref() {
            item_ids.push(state.ids.to_jf("track", file_id).await);
        }
    }
    json(
        StatusCode::OK,
        &serde_json::json!({
            "Id": playlist_id,
            "Name": detail.name,
            "Type": "Playlist",
            "ServerId": state.server_id,
            "OpenAccess": detail.open_access,
            "Shares": [],
            "ItemIds": item_ids,
        }),
    )
}

pub(super) async fn playlist_items<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(playlist_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let raw_query = request.uri().query().map(str::to_owned);
    let q = CiParams::parse(raw_query.as_deref());
    playlist_items_inner(&state, &ctx.principal.id, &playlist_id, &q).await
}

/// Real playlist-items body, split out so the auth gate stays readable.
pub(super) async fn playlist_items_inner<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    user_id: &str,
    playlist_id: &str,
    q: &CiParams,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_playlist(state, playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let detail = match state.library.playlist(user_id, &internal).await {
        Some(detail) => detail,
        None => return error(StatusCode::NOT_FOUND),
    };
    // v2 reads both `startIndex` and `StartIndex`, but params are
    // case-insensitive so both spellings are one key.
    let start = params::qint(q, "startIndex", 0).max(0) as usize;
    let limit = params::qint(q, "limit", 0).max(0) as usize;
    // One batch read for every linked file; entries whose file is gone
    // drop out before paging, so the total counts what is served.
    let file_ids = detail
        .entries
        .iter()
        .filter_map(|entry| entry.file_id.clone())
        .collect::<Vec<_>>();
    let tracks = state
        .library
        .tracks_by_ids(user_id, &file_ids)
        .await
        .into_iter()
        .map(|track| (track.file_id.clone(), track))
        .collect::<std::collections::HashMap<_, _>>();
    let served = detail
        .entries
        .iter()
        .filter_map(|entry| {
            let track = tracks.get(entry.file_id.as_deref()?)?;
            Some((entry.id.clone(), track))
        })
        .collect::<Vec<_>>();
    let (served, total) = page(&served, start, limit);
    let b = builder(state);
    let mut items = Vec::with_capacity(served.len());
    for (entry_id, track) in served {
        let mut dto = b.audio(track).await;
        dto.playlist_item_id = Some(entry_id);
        items.push(dto);
    }
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items,
            total_record_count: total,
            start_index: start,
        },
    )
}

pub(super) async fn playlist_add<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(playlist_id): Path<String>,
    authed: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let raw_query = request.uri().query().map(str::to_owned);
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    for jf_id in params::ids_param(&q, &["ids", "Ids"]) {
        if let Some(file_id) = report_file_id(&state, &jf_id).await
            && let Err(refusal) = state
                .library
                .add_playlist_entry(&authed.principal.id, &internal, &file_id)
                .await
        {
            return refused(refusal);
        }
    }
    no_content()
}

pub(super) async fn playlist_remove<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path(playlist_id): Path<String>,
    authed: Authed,
    request: Request<Body>,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let raw_query = request.uri().query().map(str::to_owned);
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    let entry_ids = params::ids_param(&q, &["entryIds", "EntryIds"]);
    if !entry_ids.is_empty()
        && let Err(refusal) = state
            .library
            .remove_playlist_entries(&authed.principal.id, &internal, &entry_ids)
            .await
    {
        return refused(refusal);
    }
    no_content()
}

pub(super) async fn playlist_move<L, E, P, I>(
    State(state): State<JellyfinState<L, E, P, I>>,
    Path((playlist_id, entry_id, new_index)): Path<(String, String, String)>,
    authed: Authed,
) -> Response
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let Ok(index) = new_index.parse::<usize>() else {
        return error(StatusCode::BAD_REQUEST);
    };
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    if let Err(refusal) = state
        .library
        .move_playlist_entry(&authed.principal.id, &internal, &entry_id, index)
        .await
    {
        return refused(refusal);
    }
    no_content()
}
