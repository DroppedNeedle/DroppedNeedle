//! Thin Axum handlers and the library reads auth extractor.
//!
//! Every handler answers one route: extract, call the service, render.
//! Status mapping lives in `From<LibraryFailure> for ApiError` (`super::error`).

use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::request::Parts,
};
use droppedneedle_auth::session::extract::Transport;
use droppedneedle_auth::session::middleware::CurrentSession;
use droppedneedle_auth::users::roles::AuthContext;
use droppedneedle_auth::users::stores::StoreError as UserStoreError;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

use super::LibraryDeps;
use super::models::{
    AlbumCardPage, AlbumDetailView, AlbumPage, AlbumQuery, ArtistPage, ArtistQuery, ArtistView,
    BrowseQuery, DecadesResponse, GenreList, LibraryAlbumStatus, LibraryMbids,
    LibraryMembershipRequest, LibraryMembershipResponse, LyricsView, PageQuery, RecentQuery,
    ResolveTracksRequest, ResolveTracksResponse, SearchQuery, SearchResults, StatsView,
    SuggestionsQuery, SuggestionsResponse, TrackPage, TrackQuery, TrackTags, TrackView,
};
use super::services;

/// Any authenticated user. Missing session or a session whose account is
/// gone reads as 401, mirroring the users role extractors (whose state
/// type cannot compose with these deps, hence the local copy).
pub struct LibraryUser(pub AuthContext);

impl FromRequestParts<LibraryDeps> for LibraryUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &LibraryDeps,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized = || ApiError::unauthorized("Authentication required");
        let session = parts
            .extensions
            .get::<CurrentSession>()
            .ok_or_else(unauthorized)?;
        let user = state
            .auth
            .users
            .get_by_id(&session.user_id)
            .await
            .map_err(|error| match error {
                UserStoreError::Conflict => ApiError::invalid_input("Conflicting state"),
                UserStoreError::Busy => {
                    ApiError::unavailable(Unavailable::ServerBusy, UserStoreError::Busy)
                }
                UserStoreError::Internal(cause) => ApiError::internal(cause),
            })?
            .ok_or_else(unauthorized)?;
        Ok(Self(AuthContext {
            user_id: user.id,
            username: user.username,
            role: user.role,
            session_id: session.session_id.clone(),
            session_kind: session.kind,
            via_cookie: session.transport == Transport::Cookie,
        }))
    }
}

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
    LibraryUser(ctx): LibraryUser,
    ValidQuery(query): ValidQuery<AlbumQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::list_albums(&deps, &ctx.user_id, &query)
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<AlbumDetailView>, ApiError> {
    services::get_album(&deps, &ctx.user_id, &id)
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::album_tracks(&deps, &ctx.user_id, &id, &query)
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::album_copies(&deps, &ctx.user_id, &id)
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
    LibraryUser(ctx): LibraryUser,
    ValidQuery(query): ValidQuery<ArtistQuery>,
) -> Result<Json<ArtistPage>, ApiError> {
    services::list_artists(&deps, &ctx.user_id, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One catalog artist.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists/{id}",
    params(("id" = String, Path, description = "Local artist id")),
    responses(
        (status = 200, description = "Artist", body = ArtistView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist id"),
    )
)]
pub async fn get_artist(
    State(deps): State<LibraryDeps>,
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<ArtistView>, ApiError> {
    services::get_artist(&deps, &ctx.user_id, &id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of albums led by the artist.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists/{id}/albums",
    params(("id" = String, Path, description = "Local artist id"), PageQuery),
    responses(
        (status = 200, description = "Led albums", body = AlbumPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist id"),
    )
)]
pub async fn list_artist_albums(
    State(deps): State<LibraryDeps>,
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::artist_albums(&deps, &ctx.user_id, &id, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One page of albums where the artist appears without leading.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/artists/{id}/appearances",
    params(("id" = String, Path, description = "Local artist id"), PageQuery),
    responses(
        (status = 200, description = "Appearance albums", body = AlbumPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown artist id"),
    )
)]
pub async fn list_artist_appearances(
    State(deps): State<LibraryDeps>,
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::artist_appearances(&deps, &ctx.user_id, &id, &query)
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
        (status = 200, description = "Track page", body = TrackPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_tracks(
    State(deps): State<LibraryDeps>,
    LibraryUser(ctx): LibraryUser,
    ValidQuery(query): ValidQuery<TrackQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::list_tracks(&deps, &ctx.user_id, &query)
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<TrackView>, ApiError> {
    services::get_track(&deps, &ctx.user_id, &id)
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<LyricsView>, ApiError> {
    let _ = ctx;
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
    LibraryUser(ctx): LibraryUser,
) -> Result<Json<StatsView>, ApiError> {
    services::stats(&deps, &ctx.user_id)
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
    LibraryUser(ctx): LibraryUser,
    ValidQuery(query): ValidQuery<RecentQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    services::recently_added(&deps, &ctx.user_id, &query)
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
    LibraryUser(_ctx): LibraryUser,
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
    LibraryUser(ctx): LibraryUser,
    Path(name): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::genre_tracks(&deps, &ctx.user_id, &name, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Album-card browse for the local library.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/local-library/albums",
    params(BrowseQuery),
    responses(
        (status = 200, description = "Album cards", body = AlbumCardPage),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn browse_albums(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
    ValidQuery(query): ValidQuery<BrowseQuery>,
) -> Result<Json<AlbumCardPage>, ApiError> {
    services::browse_albums(&deps, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Album plus track search.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/local-library/search",
    params(SearchQuery),
    responses(
        (status = 200, description = "Search results", body = SearchResults),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn search_library(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
    ValidQuery(query): ValidQuery<SearchQuery>,
) -> Result<Json<SearchResults>, ApiError> {
    services::search_library(&deps, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Newest album cards first, capped.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/local-library/recent",
    params(RecentQuery),
    responses(
        (status = 200, description = "Recent albums", body = Vec<super::models::AlbumCard>),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn recent_albums(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
    ValidQuery(query): ValidQuery<RecentQuery>,
) -> Result<Json<Vec<super::models::AlbumCard>>, ApiError> {
    services::recent_albums(&deps, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Decade shelves, oldest first.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/local-library/decades",
    responses(
        (status = 200, description = "Decades", body = DecadesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_decades(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
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
    path = "/api/v3/local-library/albums/match/{mbid}",
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
    LibraryUser(ctx): LibraryUser,
    Path(mbid): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    services::album_match(&deps, &ctx.user_id, &mbid, &query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Reason-tagged track suggestions.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/local-library/suggestions",
    params(SuggestionsQuery),
    responses(
        (status = 200, description = "Suggestions", body = SuggestionsResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_suggestions(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<TrackTags>, ApiError> {
    services::track_tags(&deps, ctx.role, &id)
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
    LibraryUser(_ctx): LibraryUser,
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
    LibraryUser(_ctx): LibraryUser,
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
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<LibraryAlbumStatus>, ApiError> {
    services::album_status(&deps, &ctx.user_id, &id)
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
    LibraryUser(_ctx): LibraryUser,
    ValidJson(body): ValidJson<ResolveTracksRequest>,
) -> Result<Json<ResolveTracksResponse>, ApiError> {
    services::resolve_tracks(&deps, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}
