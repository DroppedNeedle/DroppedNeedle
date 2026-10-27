//! Thin Axum handlers for the library reads.
//!
//! Every handler answers one route: extract, call the service, render.
//! Status mapping lives in `From<LibraryFailure> for ApiError` (`super::error`).

use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

use super::LibraryDeps;
use super::models::{
    AlbumDetailView, AlbumPage, AlbumQuery, ArtistPage, ArtistQuery, ArtistView, CursorQuery,
    DecadesResponse, GenreList, LibraryAlbumStatus, LibraryMbids, LibraryMembershipRequest,
    LibraryMembershipResponse, LyricsView, PageQuery, RecentQuery, ResolveTracksRequest,
    ResolveTracksResponse, StatsView, SuggestionsQuery, SuggestionsResponse, TrackListPage,
    TrackPage, TrackQuery, TrackTags, TrackView,
};
use super::services;

/// One page of catalog albums.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/albums",
    params(AlbumQuery),
    responses(
        (status = 200, description = "Album page", body = AlbumPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_albums(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    ValidQuery(query): ValidQuery<AlbumQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::list_albums(&deps, caller.user_id(), &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One catalog album.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/albums/{id}",
    params(("id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "Album", body = AlbumDetailView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn get_album(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> Result<Json<AlbumDetailView>, ApiError> {
    services::get_album(&deps, caller.user_id(), &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of an album's streamable tracks.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/albums/{id}/tracks",
    params(("id" = String, Path, description = "Local album id"), PageQuery),
    responses(
        (status = 200, description = "Track page", body = TrackPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn list_album_tracks(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::album_tracks(&deps, caller.user_id(), &id, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Other local albums sharing the album's release group.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/albums/{id}/copies",
    params(("id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "Sibling albums", body = AlbumPage),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn list_album_copies(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::album_copies(&deps, caller.user_id(), &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of catalog artists.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists",
    params(ArtistQuery),
    responses(
        (status = 200, description = "Artist page", body = ArtistPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_artists(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    ValidQuery(query): ValidQuery<ArtistQuery>,
) -> Result<Json<ArtistPage>, ApiError> {
    services::list_artists(&deps, caller.user_id(), &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One catalog artist.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists/{id}",
    params(("id" = String, Path, description = "Local artist id or linked MusicBrainz artist id")),
    responses(
        (status = 200, description = "Artist", body = ArtistView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist id"),
    )
)]
pub async fn get_artist(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> Result<Json<ArtistView>, ApiError> {
    services::get_artist(&deps, caller.user_id(), &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of albums led by the artist.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists/{id}/albums",
    params(("id" = String, Path, description = "Local artist id or linked MusicBrainz artist id"), CursorQuery),
    responses(
        (status = 200, description = "Led albums", body = AlbumPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist id"),
    )
)]
pub async fn list_artist_albums(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<CursorQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::artist_albums(&deps, caller.user_id(), &id, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of albums where the artist appears without leading.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists/{id}/appearances",
    params(("id" = String, Path, description = "Local artist id or linked MusicBrainz artist id"), CursorQuery),
    responses(
        (status = 200, description = "Appearance albums", body = AlbumPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist id"),
    )
)]
pub async fn list_artist_appearances(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<CursorQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::artist_appearances(&deps, caller.user_id(), &id, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of streamable tracks.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks",
    params(TrackQuery),
    responses(
        (status = 200, description = "Track page", body = TrackListPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_tracks(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    ValidQuery(query): ValidQuery<TrackQuery>,
) -> Result<Json<TrackListPage>, ApiError> {
    services::list_tracks(&deps, caller.user_id(), &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One streamable track.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks/{id}",
    params(("id" = String, Path, description = "Local track id")),
    responses(
        (status = 200, description = "Track", body = TrackView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track id"),
    )
)]
pub async fn get_track(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> Result<Json<TrackView>, ApiError> {
    services::get_track(&deps, caller.user_id(), &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Stored lyrics for one track.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks/{id}/lyrics",
    params(("id" = String, Path, description = "Local track id")),
    responses(
        (status = 200, description = "Lyrics", body = LyricsView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track id"),
    )
)]
pub async fn get_lyrics(
    State(deps): State<LibraryDeps>,
    _caller: User,
    Path(id): Path<String>,
) -> Result<Json<LyricsView>, ApiError> {
    services::lyrics(&deps, &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Library totals plus the caller's favorite counts.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/stats",
    responses(
        (status = 200, description = "Library totals", body = StatsView),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_stats(
    State(deps): State<LibraryDeps>,
    User(caller): User,
) -> Result<Json<StatsView>, ApiError> {
    services::stats(&deps, caller.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Newest albums first, capped.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/recently-added",
    params(RecentQuery),
    responses(
        (status = 200, description = "Recent albums", body = AlbumPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_recently_added(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    ValidQuery(query): ValidQuery<RecentQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::recently_added(&deps, caller.user_id(), &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Full genre listing.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/genres",
    responses(
        (status = 200, description = "Genres", body = GenreList),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_genres(
    State(deps): State<LibraryDeps>,
    _caller: User,
) -> Result<Json<GenreList>, ApiError> {
    services::genres(&deps)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of a genre's streamable tracks.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/genres/{name}/tracks",
    params(("name" = String, Path, description = "Genre name"), PageQuery),
    responses(
        (status = 200, description = "Genre tracks", body = TrackPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_genre_tracks(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(name): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::genre_tracks(&deps, caller.user_id(), &name, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Decade shelves, oldest first.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/decades",
    responses(
        (status = 200, description = "Decades", body = DecadesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_decades(
    State(deps): State<LibraryDeps>,
    _caller: User,
) -> Result<Json<DecadesResponse>, ApiError> {
    services::decades(&deps)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of an album's streamable tracks by release-group mbid or
/// local album id.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/albums/match/{mbid}",
    params(
        ("mbid" = String, Path, description = "Release-group mbid or local album id"),
        PageQuery
    ),
    responses(
        (status = 200, description = "Track page", body = TrackPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown mbid or album id"),
    )
)]
pub async fn match_album(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::album_match(&deps, caller.user_id(), &mbid, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Reason-tagged track suggestions.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/suggestions",
    params(SuggestionsQuery),
    responses(
        (status = 200, description = "Suggestions", body = SuggestionsResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_suggestions(
    State(deps): State<LibraryDeps>,
    _caller: User,
    ValidQuery(query): ValidQuery<SuggestionsQuery>,
) -> Result<Json<SuggestionsResponse>, ApiError> {
    services::suggestions(&deps, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The tags in one track's file on disk (admins only).
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks/{id}/tags",
    params(("id" = String, Path, description = "Local track id")),
    responses(
        (status = 200, description = "The file's tags", body = TrackTags),
        (status = 400, description = "The file is gone from disk or cannot be read"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required, or the file left the library roots"),
        (status = 404, description = "Unknown track"),
    )
)]
pub async fn track_tags(
    State(deps): State<LibraryDeps>,
    admin: Admin,
    Path(id): Path<String>,
) -> Result<Json<TrackTags>, ApiError> {
    services::track_tags(&deps, &admin, &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Every MusicBrainz release group the library holds, and every album
/// with an open request.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/mbids",
    responses(
        (status = 200, description = "Held and requested album ids", body = LibraryMbids),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn album_mbids(
    State(deps): State<LibraryDeps>,
    _caller: User,
) -> Result<Json<LibraryMbids>, ApiError> {
    services::album_mbids(&deps)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Which MusicBrainz album ids the library holds or has requested.
#[utoipa::path(
    post,
    tag = "library",
    path = "/api/v3/library/membership",
    request_body = LibraryMembershipRequest,
    responses(
        (status = 200, description = "Owned and requested ids", body = LibraryMembershipResponse),
        (status = 400, description = "Bad body or more than 500 ids"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn membership(
    State(deps): State<LibraryDeps>,
    _caller: User,
    ValidJson(body): ValidJson<LibraryMembershipRequest>,
) -> Result<Json<LibraryMembershipResponse>, ApiError> {
    services::membership(&deps, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// What the library holds for one album, by local id or MusicBrainz id,
/// with each track's quality tier against the upgrade cutoff.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/albums/{id}/status",
    params(("id" = String, Path, description = "Local album id, or a release-group or release mbid")),
    responses(
        (status = 200, description = "Album status", body = LibraryAlbumStatus),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn album_status(
    State(deps): State<LibraryDeps>,
    User(caller): User,
    Path(id): Path<String>,
) -> Result<Json<LibraryAlbumStatus>, ApiError> {
    services::album_status(&deps, caller.user_id(), &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Resolve track positions to playable local files.
#[utoipa::path(
    post,
    tag = "library",
    path = "/api/v3/library/resolve-tracks",
    request_body = ResolveTracksRequest,
    responses(
        (status = 200, description = "Resolved positions", body = ResolveTracksResponse),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn resolve_tracks(
    State(deps): State<LibraryDeps>,
    _caller: User,
    ValidJson(body): ValidJson<ResolveTracksRequest>,
) -> Result<Json<ResolveTracksResponse>, ApiError> {
    services::resolve_tracks(&deps, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}
