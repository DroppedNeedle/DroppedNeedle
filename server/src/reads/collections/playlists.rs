//! Playlists: CRUD, track order, visibility, and covers.
//!
//! Ownership rules: anyone authenticated lists; owners see full rows for
//! their playlists and everyone sees full rows for public ones. Other users'
//! private playlists redact to existence plus count plus owner. Mutations are
//! owner-only: private plus non-owner is 404 (the row stays hidden), public
//! plus non-owner is 403. Admins read like anyone else; they hold no extra
//! playlist rights.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use super::{
    auth::Principal,
    error::{CollectionsError, ValidJson},
    models::{
        AddTracksBody, AddTracksResponse, CheckTracksBody, CheckTracksResponse, CoverUploadBody,
        CoverUploadResponse, CreatePlaylistBody, PlaylistDetail, PlaylistListItem,
        PlaylistListResponse, PlaylistSummary, PlaylistTrack, RedactedPlaylist, RemoveTracksBody,
        RemoveTracksResponse, ReorderBody, ReorderResponse, ResolveSourcesResponse, StatusResponse,
        TrackInput, UpdatePlaylistBody, UpdateTrackBody, VisibilityBody,
    },
    state::{
        CollectionsState, PlaylistStore, StoredCover, StoredPlaylist, now_epoch, read_store,
        write_store,
    },
};

/// Longest accepted playlist name.
const MAX_NAME_LEN: usize = 200;
/// How many track covers a summary carries.
const SUMMARY_COVER_COUNT: usize = 4;
/// Decoded cover cap: 5 MiB.
const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;
/// Cover mime allowlist.
const COVER_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// Trimmed non-empty name or 400.
fn clean_name(raw: &str) -> Result<String, CollectionsError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(CollectionsError::InvalidInput {
            message: "Playlist name must not be blank".to_owned(),
        });
    }
    if name.len() > MAX_NAME_LEN {
        return Err(CollectionsError::InvalidInput {
            message: "Playlist name is too long".to_owned(),
        });
    }
    Ok(name.to_owned())
}

/// Stored row to summary for one caller.
fn to_summary(row: &StoredPlaylist, caller: &Principal) -> PlaylistSummary {
    let cover_urls = row
        .tracks
        .iter()
        .filter_map(|track| track.cover_url.clone())
        .take(SUMMARY_COVER_COUNT)
        .collect::<Vec<_>>();
    let total_duration = row
        .tracks
        .iter()
        .filter_map(|track| track.duration)
        .reduce(|a, b| a + b);
    PlaylistSummary {
        id: row.id.clone(),
        name: row.name.clone(),
        track_count: row.tracks.len(),
        total_duration,
        cover_urls,
        custom_cover_url: row.cover.as_ref().map(|_| cover_url(&row.id)),
        source_ref: row.source_ref.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        is_public: row.is_public,
        is_owner: row.owner_id == caller.user_id,
        owner_name: Some(row.owner_name.clone()),
        is_redacted: false,
    }
}

/// Stored row to detail for one caller.
fn to_detail(row: &StoredPlaylist, caller: &Principal) -> PlaylistDetail {
    let summary = to_summary(row, caller);
    PlaylistDetail {
        id: summary.id,
        name: summary.name,
        cover_urls: summary.cover_urls,
        custom_cover_url: summary.custom_cover_url,
        source_ref: summary.source_ref,
        tracks: row.tracks.clone(),
        track_count: summary.track_count,
        total_duration: summary.total_duration,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        is_public: summary.is_public,
        is_owner: summary.is_owner,
        owner_name: summary.owner_name,
        is_redacted: false,
    }
}

/// Stored row to redacted stub.
fn to_redacted(row: &StoredPlaylist) -> RedactedPlaylist {
    RedactedPlaylist {
        id: row.id.clone(),
        track_count: row.tracks.len(),
        owner_name: Some(row.owner_name.clone()),
        is_redacted: true,
    }
}

/// Canonical cover URL for a playlist.
fn cover_url(playlist_id: &str) -> String {
    format!("/api/v3/playlists/{playlist_id}/cover")
}

/// Build a stored track row from caller input.
fn to_track(input: &TrackInput, id: String, position: usize, now: u64) -> PlaylistTrack {
    PlaylistTrack {
        id,
        position,
        track_name: input.track_name.clone(),
        artist_name: input.artist_name.clone(),
        album_name: input.album_name.clone(),
        album_id: input.album_id.clone(),
        artist_id: input.artist_id.clone(),
        track_source_id: input.track_source_id.clone(),
        cover_url: input.cover_url.clone(),
        source_type: input.source_type.clone(),
        available_sources: input.available_sources.clone(),
        format: input.format.clone(),
        track_number: input.track_number,
        disc_number: input.disc_number,
        duration: input.duration,
        created_at: now,
        plex_rating_key: input.plex_rating_key.clone(),
        library_file_id: None,
    }
}

/// Validate one track input. Names are required; the rest is optional.
fn check_track_input(input: &TrackInput) -> Result<(), CollectionsError> {
    for (label, value) in [
        ("track_name", &input.track_name),
        ("artist_name", &input.artist_name),
        ("album_name", &input.album_name),
    ] {
        if value.trim().is_empty() {
            return Err(CollectionsError::InvalidInput {
                message: format!("Track {label} must not be blank"),
            });
        }
    }
    Ok(())
}

/// Renumber positions after an insert or remove.
fn renumber(tracks: &mut [PlaylistTrack]) {
    for (position, track) in tracks.iter_mut().enumerate() {
        track.position = position;
    }
}

/// List the caller's visible playlists. Private rows owned by others redact.
pub fn list_playlists(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<PlaylistListResponse, CollectionsError> {
    state.check_injection()?;
    let rows = read_store(&state.playlists.playlists, "playlist")?;
    let mut playlists = rows
        .values()
        .map(|row| {
            if row.owner_id == caller.user_id || row.is_public {
                PlaylistListItem::Full(to_summary(row, caller))
            } else {
                PlaylistListItem::Redacted(to_redacted(row))
            }
        })
        .collect::<Vec<_>>();
    playlists.sort_by(|a, b| {
        let name_a = match a {
            PlaylistListItem::Full(summary) => summary.name.clone(),
            PlaylistListItem::Redacted(redacted) => redacted.id.clone(),
        };
        let name_b = match b {
            PlaylistListItem::Full(summary) => summary.name.clone(),
            PlaylistListItem::Redacted(redacted) => redacted.id.clone(),
        };
        name_a.cmp(&name_b)
    });
    Ok(PlaylistListResponse { playlists })
}

/// Create a playlist owned by the caller.
pub fn create_playlist(
    state: &CollectionsState,
    caller: &Principal,
    body: &CreatePlaylistBody,
) -> Result<PlaylistDetail, CollectionsError> {
    state.check_injection()?;
    let name = clean_name(&body.name)?;
    let now = now_epoch();
    let row = StoredPlaylist {
        id: format!("pl-{}", uuid::Uuid::new_v4()),
        name,
        owner_id: caller.user_id.clone(),
        owner_name: caller.display_name(),
        is_public: false,
        tracks: Vec::new(),
        cover: None,
        source_ref: body.source_ref.clone(),
        created_at: now,
        updated_at: now,
    };
    let detail = to_detail(&row, caller);
    write_store(&state.playlists.playlists, "playlist")?.insert(row.id.clone(), row);
    Ok(detail)
}

/// Read one playlist row. Private plus non-owner is 404: detail callers get
/// the full row or nothing, and redaction only appears in list answers.
fn read_playlist_row(
    store: &PlaylistStore,
    playlist_id: &str,
    caller: &Principal,
) -> Result<StoredPlaylist, CollectionsError> {
    let rows = read_store(&store.playlists, "playlist")?;
    let row = rows.get(playlist_id).ok_or(CollectionsError::NotFound)?;
    if row.owner_id == caller.user_id || row.is_public {
        Ok(row.clone())
    } else {
        Err(CollectionsError::NotFound)
    }
}

/// Read one playlist detail.
pub fn get_playlist(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
) -> Result<PlaylistDetail, CollectionsError> {
    state.check_injection()?;
    read_playlist_row(&state.playlists, playlist_id, caller).map(|row| to_detail(&row, caller))
}

/// Owner-only mutation guard: private plus non-owner is 404, public plus
/// non-owner is 403.
fn require_owner(row: &StoredPlaylist, caller: &Principal) -> Result<(), CollectionsError> {
    if row.owner_id == caller.user_id {
        Ok(())
    } else if row.is_public {
        Err(CollectionsError::Forbidden {
            message: "Only the owner can change this playlist".to_owned(),
        })
    } else {
        Err(CollectionsError::NotFound)
    }
}

/// Rename a playlist. Owner only.
pub fn update_playlist(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    body: &UpdatePlaylistBody,
) -> Result<PlaylistDetail, CollectionsError> {
    state.check_injection()?;
    let name = body.name.as_deref().map(clean_name).transpose()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    if let Some(name) = name {
        row.name = name;
        row.updated_at = now_epoch();
    }
    Ok(to_detail(row, caller))
}

/// Delete a playlist. Owner only.
pub fn delete_playlist(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
) -> Result<StatusResponse, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows.get(playlist_id).ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    rows.remove(playlist_id);
    Ok(StatusResponse {
        status: "ok".to_owned(),
        message: "Playlist deleted".to_owned(),
    })
}

/// Flip playlist visibility. Owner only.
pub fn set_visibility(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    body: &VisibilityBody,
) -> Result<PlaylistSummary, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    row.is_public = body.is_public;
    row.updated_at = now_epoch();
    Ok(to_summary(row, caller))
}

/// Add tracks at a position, or append. Owner only.
pub fn add_tracks(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    body: &AddTracksBody,
) -> Result<AddTracksResponse, CollectionsError> {
    state.check_injection()?;
    for input in &body.tracks {
        check_track_input(input)?;
    }
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    let now = now_epoch();
    let at = body
        .position
        .unwrap_or(row.tracks.len())
        .min(row.tracks.len());
    let mut added = Vec::with_capacity(body.tracks.len());
    for (offset, input) in body.tracks.iter().enumerate() {
        added.push(to_track(
            input,
            format!("trk-{}", uuid::Uuid::new_v4()),
            at + offset,
            now,
        ));
    }
    row.tracks.splice(at..at, added.clone());
    renumber(&mut row.tracks);
    row.updated_at = now;
    let added = row.tracks[at..at + added.len()].to_vec();
    Ok(AddTracksResponse { tracks: added })
}

/// Remove tracks by id, skipping unknown ids. Owner only.
pub fn remove_tracks(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    body: &RemoveTracksBody,
) -> Result<RemoveTracksResponse, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    let before = row.tracks.len();
    row.tracks
        .retain(|track| !body.track_ids.iter().any(|id| id == &track.id));
    renumber(&mut row.tracks);
    row.updated_at = now_epoch();
    let removed = before - row.tracks.len();
    Ok(RemoveTracksResponse {
        status: "ok".to_owned(),
        message: format!("Removed {removed} track(s)"),
        removed,
    })
}

/// Remove one track. Unknown tracks are 404. Owner only.
pub fn remove_track(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    track_id: &str,
) -> Result<StatusResponse, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    let before = row.tracks.len();
    row.tracks.retain(|track| track.id != track_id);
    if row.tracks.len() == before {
        return Err(CollectionsError::NotFound);
    }
    renumber(&mut row.tracks);
    row.updated_at = now_epoch();
    Ok(StatusResponse {
        status: "ok".to_owned(),
        message: "Track removed".to_owned(),
    })
}

/// Move one track to a new position, clamping past-the-end. Owner only.
pub fn reorder_track(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    body: &ReorderBody,
) -> Result<ReorderResponse, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    let from = row
        .tracks
        .iter()
        .position(|track| track.id == body.track_id)
        .ok_or(CollectionsError::NotFound)?;
    let track = row.tracks.remove(from);
    let at = body.new_position.min(row.tracks.len());
    row.tracks.insert(at, track);
    renumber(&mut row.tracks);
    row.updated_at = now_epoch();
    Ok(ReorderResponse {
        status: "ok".to_owned(),
        message: "Track reordered".to_owned(),
        actual_position: at,
    })
}

/// Update one track's source fields. Owner only.
pub fn update_track(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    track_id: &str,
    body: &UpdateTrackBody,
) -> Result<PlaylistTrack, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    let track = row
        .tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .ok_or(CollectionsError::NotFound)?;
    if let Some(source_type) = body.source_type.clone() {
        track.source_type = source_type;
    }
    if let Some(sources) = body.available_sources.clone() {
        track.available_sources = Some(sources);
    }
    row.updated_at = now_epoch();
    Ok(track.clone())
}

/// Which of the caller's visible playlists hold each queried track. Matches
/// on exact name, artist, and album.
pub fn check_track_membership(
    state: &CollectionsState,
    caller: &Principal,
    body: &CheckTracksBody,
) -> Result<CheckTracksResponse, CollectionsError> {
    state.check_injection()?;
    let rows = read_store(&state.playlists.playlists, "playlist")?;
    let visible = rows
        .values()
        .filter(|row| row.owner_id == caller.user_id || row.is_public)
        .collect::<Vec<_>>();
    let mut membership = HashMap::new();
    for (index, query) in body.tracks.iter().enumerate() {
        let mut holding = Vec::new();
        for row in &visible {
            let found = row.tracks.iter().any(|track| {
                track.track_name == query.track_name
                    && track.artist_name == query.artist_name
                    && track.album_name == query.album_name
            });
            if found {
                holding.push(row.id.clone());
            }
        }
        membership.insert(index.to_string(), holding);
    }
    Ok(CheckTracksResponse { membership })
}

/// Resolve each track's known sources. Visible playlists only.
pub fn resolve_sources(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
) -> Result<ResolveSourcesResponse, CollectionsError> {
    state.check_injection()?;
    let row = read_playlist_row(&state.playlists, playlist_id, caller)?;
    let sources = row
        .tracks
        .iter()
        .map(|track| {
            (
                track.id.clone(),
                track.available_sources.clone().unwrap_or_default(),
            )
        })
        .collect();
    Ok(ResolveSourcesResponse { sources })
}

/// Store a cover from base64 bytes. Owner only.
pub fn upload_cover(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
    body: &CoverUploadBody,
) -> Result<CoverUploadResponse, CollectionsError> {
    state.check_injection()?;
    if !COVER_TYPES.contains(&body.content_type.as_str()) {
        return Err(CollectionsError::InvalidInput {
            message: "Cover must be png, jpeg, or webp".to_owned(),
        });
    }
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.image_base64.trim())
        .map_err(|_| CollectionsError::InvalidInput {
            message: "Cover is not valid base64".to_owned(),
        })?;
    if bytes.len() > MAX_COVER_BYTES {
        return Err(CollectionsError::InvalidInput {
            message: "Cover exceeds the 5 MiB limit".to_owned(),
        });
    }
    if bytes.is_empty() {
        return Err(CollectionsError::InvalidInput {
            message: "Cover must not be empty".to_owned(),
        });
    }
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    row.cover = Some(StoredCover {
        content_type: body.content_type.clone(),
        bytes,
    });
    row.updated_at = now_epoch();
    Ok(CoverUploadResponse {
        cover_url: cover_url(&row.id),
    })
}

/// Read cover bytes. Visible playlists only; missing covers are 404.
pub fn get_cover(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
) -> Result<Response, CollectionsError> {
    state.check_injection()?;
    let row = read_playlist_row(&state.playlists, playlist_id, caller)?;
    let cover = row.cover.as_ref().ok_or(CollectionsError::NotFound)?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, cover.content_type.clone())],
        cover.bytes.clone(),
    )
        .into_response())
}

/// Delete a cover. Owner only; missing covers are 404.
pub fn remove_cover(
    state: &CollectionsState,
    caller: &Principal,
    playlist_id: &str,
) -> Result<StatusResponse, CollectionsError> {
    state.check_injection()?;
    let mut rows = write_store(&state.playlists.playlists, "playlist")?;
    let row = rows
        .get_mut(playlist_id)
        .ok_or(CollectionsError::NotFound)?;
    require_owner(row, caller)?;
    if row.cover.is_none() {
        return Err(CollectionsError::NotFound);
    }
    row.cover = None;
    row.updated_at = now_epoch();
    Ok(StatusResponse {
        status: "ok".to_owned(),
        message: "Cover removed".to_owned(),
    })
}

// Handlers. Thin: extract, call the service, render.

/// List visible playlists.
#[utoipa::path(
    get,
    path = "/api/v3/playlists",
    responses(
        (status = 200, description = "Visible playlists", body = PlaylistListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_playlists_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<PlaylistListResponse>, CollectionsError> {
    list_playlists(&state, &caller).map(Json)
}

/// Create a playlist.
#[utoipa::path(
    post,
    path = "/api/v3/playlists",
    request_body = CreatePlaylistBody,
    responses(
        (status = 201, description = "Created playlist", body = PlaylistDetail),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn create_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    ValidJson(body): ValidJson<CreatePlaylistBody>,
) -> Result<(StatusCode, Json<PlaylistDetail>), CollectionsError> {
    create_playlist(&state, &caller, &body).map(|detail| (StatusCode::CREATED, Json(detail)))
}

/// Read one playlist.
#[utoipa::path(
    get,
    path = "/api/v3/playlists/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Playlist detail", body = PlaylistDetail),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn get_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> Result<Json<PlaylistDetail>, CollectionsError> {
    get_playlist(&state, &caller, &playlist_id).map(Json)
}

/// Rename a playlist.
#[utoipa::path(
    put,
    path = "/api/v3/playlists/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = UpdatePlaylistBody,
    responses(
        (status = 200, description = "Renamed playlist", body = PlaylistDetail),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn update_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<UpdatePlaylistBody>,
) -> Result<Json<PlaylistDetail>, CollectionsError> {
    update_playlist(&state, &caller, &playlist_id, &body).map(Json)
}

/// Delete a playlist.
#[utoipa::path(
    delete,
    path = "/api/v3/playlists/{playlist_id}",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Deletion receipt", body = StatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn delete_playlist_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> Result<Json<StatusResponse>, CollectionsError> {
    delete_playlist(&state, &caller, &playlist_id).map(Json)
}

/// Flip playlist visibility.
#[utoipa::path(
    patch,
    path = "/api/v3/playlists/{playlist_id}/visibility",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = VisibilityBody,
    responses(
        (status = 200, description = "Updated summary", body = PlaylistSummary),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn set_visibility_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<VisibilityBody>,
) -> Result<Json<PlaylistSummary>, CollectionsError> {
    set_visibility(&state, &caller, &playlist_id, &body).map(Json)
}

/// Add tracks to a playlist.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/tracks",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = AddTracksBody,
    responses(
        (status = 200, description = "Added tracks", body = AddTracksResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn add_tracks_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<AddTracksBody>,
) -> Result<Json<AddTracksResponse>, CollectionsError> {
    add_tracks(&state, &caller, &playlist_id, &body).map(Json)
}

/// Bulk-remove tracks from a playlist.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/tracks/remove",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = RemoveTracksBody,
    responses(
        (status = 200, description = "Removal receipt", body = RemoveTracksResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn remove_tracks_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<RemoveTracksBody>,
) -> Result<Json<RemoveTracksResponse>, CollectionsError> {
    remove_tracks(&state, &caller, &playlist_id, &body).map(Json)
}

/// Remove one track from a playlist.
#[utoipa::path(
    delete,
    path = "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
    params(
        ("playlist_id" = String, Path, description = "Playlist id"),
        ("track_id" = String, Path, description = "Track row id")
    ),
    responses(
        (status = 200, description = "Removal receipt", body = StatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist or track id"),
    )
)]
pub async fn remove_track_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path((playlist_id, track_id)): Path<(String, String)>,
) -> Result<Json<StatusResponse>, CollectionsError> {
    remove_track(&state, &caller, &playlist_id, &track_id).map(Json)
}

/// Reorder one track inside a playlist.
#[utoipa::path(
    patch,
    path = "/api/v3/playlists/{playlist_id}/tracks/reorder",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = ReorderBody,
    responses(
        (status = 200, description = "Reorder receipt", body = ReorderResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist or track id"),
    )
)]
pub async fn reorder_track_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<ReorderBody>,
) -> Result<Json<ReorderResponse>, CollectionsError> {
    reorder_track(&state, &caller, &playlist_id, &body).map(Json)
}

/// Update one track's source fields.
#[utoipa::path(
    patch,
    path = "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
    params(
        ("playlist_id" = String, Path, description = "Playlist id"),
        ("track_id" = String, Path, description = "Track row id")
    ),
    request_body = UpdateTrackBody,
    responses(
        (status = 200, description = "Updated track", body = PlaylistTrack),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist or track id"),
    )
)]
pub async fn update_track_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path((playlist_id, track_id)): Path<(String, String)>,
    ValidJson(body): ValidJson<UpdateTrackBody>,
) -> Result<Json<PlaylistTrack>, CollectionsError> {
    update_track(&state, &caller, &playlist_id, &track_id, &body).map(Json)
}

/// Check which visible playlists hold each queried track.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/check-tracks",
    request_body = CheckTracksBody,
    responses(
        (status = 200, description = "Membership map", body = CheckTracksResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn check_tracks_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    ValidJson(body): ValidJson<CheckTracksBody>,
) -> Result<Json<CheckTracksResponse>, CollectionsError> {
    check_track_membership(&state, &caller, &body).map(Json)
}

/// Resolve each track's known sources.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/resolve-sources",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Source map", body = ResolveSourcesResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn resolve_sources_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> Result<Json<ResolveSourcesResponse>, CollectionsError> {
    resolve_sources(&state, &caller, &playlist_id).map(Json)
}

/// Upload a playlist cover.
#[utoipa::path(
    post,
    path = "/api/v3/playlists/{playlist_id}/cover",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = CoverUploadBody,
    responses(
        (status = 200, description = "Cover URL", body = CoverUploadResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn upload_cover_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
    ValidJson(body): ValidJson<CoverUploadBody>,
) -> Result<Json<CoverUploadResponse>, CollectionsError> {
    upload_cover(&state, &caller, &playlist_id, &body).map(Json)
}

/// Serve playlist cover bytes.
#[utoipa::path(
    get,
    path = "/api/v3/playlists/{playlist_id}/cover",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Cover bytes"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn get_cover_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> Result<Response, CollectionsError> {
    get_cover(&state, &caller, &playlist_id)
}

/// Delete a playlist cover.
#[utoipa::path(
    delete,
    path = "/api/v3/playlists/{playlist_id}/cover",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Removal receipt", body = StatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Owner only"),
        (status = 404, description = "Unknown playlist id"),
    )
)]
pub async fn remove_cover_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(playlist_id): Path<String>,
) -> Result<Json<StatusResponse>, CollectionsError> {
    remove_cover(&state, &caller, &playlist_id).map(Json)
}
