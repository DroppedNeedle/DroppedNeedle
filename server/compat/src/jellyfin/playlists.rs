//! Playlist routes over the shared playlists.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use droppedneedle_kernel::error::ApiError;

use super::models::{BaseItemDtoQueryResult, CreatePlaylistDto};
use super::params::{self, CiParams};
use super::playstate::*;
use super::query::page;
use super::router::*;
use super::seams::ReadFailed;

// ===== Playlists =====

pub(super) async fn decode_playlist(
    state: &JellyfinState,
    jf_id: &str,
) -> Result<String, Response> {
    match state.ids.from_jf(jf_id).await {
        Ok(Some((kind, internal))) if kind == "playlist" => Ok(internal),
        Ok(_) => Err(error(StatusCode::NOT_FOUND)),
        Err(failed) => Err(ApiError::from(failed).into_response()),
    }
}

/// The track file ids of `jf_ids`, in order; unknown and non-track ids
/// drop out. Every id is read before the playlist write, which is one
/// transaction, so a busy answer means nothing was written and asking
/// again never repeats an entry.
async fn track_file_ids(
    state: &JellyfinState,
    jf_ids: &[String],
) -> Result<Vec<String>, ReadFailed> {
    Ok(state
        .ids
        .from_jf_many(jf_ids)
        .await?
        .into_iter()
        .flatten()
        .filter(|(kind, _)| kind == "track")
        .map(|(_, internal)| internal)
        .collect())
}

/// Create from a JSON body or `?name=`/`?ids=` (v2 `_create_playlist`).
/// Unknown track ids are skipped silently.
pub(super) async fn create_playlist(
    State(state): State<JellyfinState>,
    authed: Authed,
    request: Request<Body>,
) -> Response {
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
    let file_ids = match track_file_ids(&state, &track_ids).await {
        Ok(file_ids) => file_ids,
        Err(failed) => return ApiError::from(failed).into_response(),
    };
    let internal = match state
        .library
        .create_playlist(&authed.principal.id, &name, &file_ids)
        .await
    {
        Ok(internal) => internal,
        Err(refusal) => return refused(refusal),
    };
    json(
        StatusCode::OK,
        &serde_json::json!({ "Id": state.ids.to_jf("playlist", &internal).await }),
    )
}

pub(super) async fn get_playlist(
    State(state): State<JellyfinState>,
    Path(playlist_id): Path<String>,
    ctx: Authed,
) -> Response {
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let detail = match state.library.playlist(&ctx.principal.id, &internal).await {
        Ok(Some(detail)) => detail,
        Ok(None) => return error(StatusCode::NOT_FOUND),
        Err(failed) => return ApiError::from(failed).into_response(),
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

pub(super) async fn playlist_items(
    State(state): State<JellyfinState>,
    Path(playlist_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Response {
    let raw_query = request.uri().query().map(str::to_owned);
    let q = CiParams::parse(raw_query.as_deref());
    playlist_items_inner(&state, &ctx.principal.id, &playlist_id, &q).await
}

/// Real playlist-items body, split out so the auth gate stays readable.
pub(super) async fn playlist_items_inner(
    state: &JellyfinState,
    user_id: &str,
    playlist_id: &str,
    q: &CiParams,
) -> Response {
    let internal = match decode_playlist(state, playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let detail = match state.library.playlist(user_id, &internal).await {
        Ok(Some(detail)) => detail,
        Ok(None) => return error(StatusCode::NOT_FOUND),
        Err(failed) => return ApiError::from(failed).into_response(),
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
    let tracks = match state.library.tracks_by_ids(user_id, &file_ids).await {
        Ok(tracks) => tracks,
        Err(failed) => return ApiError::from(failed).into_response(),
    };
    let tracks = tracks
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

pub(super) async fn playlist_add(
    State(state): State<JellyfinState>,
    Path(playlist_id): Path<String>,
    authed: Authed,
    request: Request<Body>,
) -> Response {
    let raw_query = request.uri().query().map(str::to_owned);
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    let file_ids = match track_file_ids(&state, &params::ids_param(&q, &["ids", "Ids"])).await {
        Ok(file_ids) => file_ids,
        Err(failed) => return ApiError::from(failed).into_response(),
    };
    if let Err(refusal) = state
        .library
        .add_playlist_entries(&authed.principal.id, &internal, &file_ids)
        .await
    {
        return refused(refusal);
    }
    no_content()
}

pub(super) async fn playlist_remove(
    State(state): State<JellyfinState>,
    Path(playlist_id): Path<String>,
    authed: Authed,
    request: Request<Body>,
) -> Response {
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

pub(super) async fn playlist_move(
    State(state): State<JellyfinState>,
    Path((playlist_id, entry_id, new_index)): Path<(String, String, String)>,
    authed: Authed,
) -> Response {
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
