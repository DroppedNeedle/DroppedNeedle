//! Private library playlists evaluated against the signed-in listener's current ratings.
mod store;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::{
    extract::{ValidJson, ValidQuery},
    mount::{Routes, routes},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{error::CollectionsError, http::HttpResult};
use crate::local::{
    LibraryDeps,
    models::{TrackListPage, TrackQuery},
    services::list_tracks,
};

/// A saved rule. Track membership is evaluated when a client requests a page.
#[derive(Serialize, ToSchema)]
pub struct SmartPlaylist {
    /// Stable playlist id.
    pub id: String,
    /// Listener-chosen display name.
    pub name: String,
    /// Include tracks this listener rated at least this many stars.
    #[schema(minimum = 1, maximum = 5)]
    pub min_rating: u8,
}

/// Create or replace a private rule.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SmartPlaylistInput {
    /// Display name, up to 200 UTF-8 bytes after trimming.
    pub name: String,
    /// Minimum rating, inclusive. Unrated tracks are excluded.
    #[schema(minimum = 1, maximum = 5)]
    pub min_rating: u8,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct MatchesQuery {
    /// Tracks per page, 1–500. Defaults to 50.
    limit: Option<i64>,
    /// The previous page's opaque next cursor.
    after: Option<String>,
}

fn owned<'a>(deps: &'a LibraryDeps, user_id: &str) -> store::Owned<'a> {
    store::Owned {
        db: deps.catalog.db(),
        user_id: user_id.to_owned(),
    }
}

/// Mount the definition and match routes behind the session gate.
pub fn routes() -> Routes<LibraryDeps> {
    Routes::new()
        .user(routes!(list_smart_playlists, create_smart_playlist))
        .user(routes!(
            get_smart_playlist,
            update_smart_playlist,
            delete_smart_playlist
        ))
        .user(routes!(smart_playlist_tracks))
}

/// List the caller's private rules, ordered by name.
#[utoipa::path(
    get,
    path = "/api/v3/smart-playlists",
    tag = "collections",
    responses(
        (status = 200, description = "Owned smart playlists", body = [SmartPlaylist]),
        (status = 401, description = "Not authenticated"),
    )
)]
async fn list_smart_playlists(
    State(deps): State<LibraryDeps>,
    User(caller): User,
) -> HttpResult<Json<Vec<SmartPlaylist>>> {
    Ok(Json(
        owned(&deps, caller.user_id())
            .list()
            .await
            .map_err(CollectionsError::from)?,
    ))
}

/// Save a new private rule.
#[utoipa::path(
    post,
    path = "/api/v3/smart-playlists",
    tag = "collections",
    request_body = SmartPlaylistInput,
    responses(
        (status = 201, description = "Created playlist", body = SmartPlaylist),
        (status = 400, description = "Invalid definition"),
        (status = 401, description = "Not authenticated"),
    )
)]
async fn create_smart_playlist(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<SmartPlaylistInput>,
) -> HttpResult<(StatusCode, Json<SmartPlaylist>)> {
    Ok((
        StatusCode::CREATED,
        Json(owned(&deps, caller.user_id()).save(None, body).await?),
    ))
}

/// Get one saved rule.
#[utoipa::path(
    get,
    path = "/api/v3/smart-playlists/{playlist_id}",
    tag = "collections",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 200, description = "Owned smart playlist", body = SmartPlaylist),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such owned playlist"),
    )
)]
async fn get_smart_playlist(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> HttpResult<Json<SmartPlaylist>> {
    Ok(Json(owned(&deps, caller.user_id()).get(id).await?))
}

/// Replace a saved rule without changing its id.
#[utoipa::path(
    put,
    path = "/api/v3/smart-playlists/{playlist_id}",
    tag = "collections",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    request_body = SmartPlaylistInput,
    responses(
        (status = 200, description = "Updated smart playlist", body = SmartPlaylist),
        (status = 400, description = "Invalid definition"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such owned playlist"),
    )
)]
async fn update_smart_playlist(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<SmartPlaylistInput>,
) -> HttpResult<Json<SmartPlaylist>> {
    Ok(Json(
        owned(&deps, caller.user_id()).save(Some(id), body).await?,
    ))
}

/// Delete a saved rule.
#[utoipa::path(
    delete,
    path = "/api/v3/smart-playlists/{playlist_id}",
    tag = "collections",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such owned playlist"),
    )
)]
async fn delete_smart_playlist(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> HttpResult<StatusCode> {
    owned(&deps, caller.user_id()).delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Page through the current matching library tracks in title order.
#[utoipa::path(
    get,
    path = "/api/v3/smart-playlists/{playlist_id}/tracks",
    tag = "collections",
    params(("playlist_id" = String, Path, description = "Playlist id"), MatchesQuery),
    responses(
        (status = 200, description = "Matching tracks", body = TrackListPage),
        (status = 400, description = "Invalid page"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No such owned playlist"),
    )
)]
async fn smart_playlist_tracks(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
    ValidQuery(page): ValidQuery<MatchesQuery>,
) -> HttpResult<Json<TrackListPage>> {
    let playlist = owned(&deps, caller.user_id()).get(id).await?;
    let query = TrackQuery {
        limit: page.limit,
        after: page.after,
        rating: Some(playlist.min_rating),
        ..Default::default()
    };
    Ok(Json(list_tracks(&deps, caller.user_id(), &query).await?))
}
