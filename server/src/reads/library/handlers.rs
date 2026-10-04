//! Thin Axum handlers and the slice-local auth extractor.
//!
//! Every handler answers one route: extract, call the service, render.
//! Status mapping lives in [`LibraryError`](super::error::LibraryError).
//! Query strings parse through [`ValidQuery`], which keeps malformed input
//! inside the shared error envelope instead of Axum's default plain-text 400.

use axum::{
    Json,
    extract::{FromRequestParts, Path, Query, Request, State},
    http::request::Parts,
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::roles::AuthContext;
use droppedneedle::auth::users::stores::StoreError as UserStoreError;
use serde::de::DeserializeOwned;

use super::LibraryDeps;
use super::error::LibraryError;
use super::models::{
    AlbumCardPage, AlbumPage, AlbumQuery, AlbumView, ArtistPage, ArtistQuery, ArtistView,
    BrowseQuery, DecadesResponse, GenreList, LyricsView, PageQuery, RecentQuery, SearchQuery,
    SearchResults, StatsView, SuggestionsQuery, SuggestionsResponse, TrackPage, TrackQuery,
    TrackView,
};
use super::services::{self, LibraryFailure};

/// Any authenticated user. Missing session or a session whose account is
/// gone reads as 401, mirroring the sibling role extractors (whose state
/// type cannot compose with this slice's deps, hence the local copy).
pub struct LibraryUser(pub AuthContext);

impl FromRequestParts<LibraryDeps> for LibraryUser {
    type Rejection = LibraryError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &LibraryDeps,
    ) -> Result<Self, Self::Rejection> {
        let session =
            parts
                .extensions
                .get::<CurrentSession>()
                .ok_or(LibraryError::Unauthorized {
                    message: "Authentication required".to_owned(),
                })?;
        let user = state
            .auth
            .users
            .get_by_id(&session.user_id)
            .await
            .map_err(|error| match error {
                UserStoreError::Conflict => LibraryError::InvalidInput {
                    message: "Conflicting state".to_owned(),
                },
                UserStoreError::Internal(cause) => {
                    LibraryError::internal(&cause, state.ids.as_ref())
                }
            })?
            .ok_or(LibraryError::Unauthorized {
                message: "Authentication required".to_owned(),
            })?;
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

/// Query extractor that renders failures in the shared envelope.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> axum::extract::FromRequest<S> for ValidQuery<T> {
    type Rejection = LibraryError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| LibraryError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

/// Map a domain failure to the wire. Internal causes reach the log only.
fn failed(failure: LibraryFailure, deps: &LibraryDeps) -> LibraryError {
    match failure {
        LibraryFailure::NotFound => LibraryError::NotFound,
        LibraryFailure::InvalidInput(message) => LibraryError::InvalidInput { message },
        LibraryFailure::Internal(cause) => LibraryError::internal(&cause, deps.ids.as_ref()),
    }
}

/// One page of catalog albums.
#[utoipa::path(
    get,
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
) -> Result<Json<AlbumPage>, LibraryError> {
    services::list_albums(&deps, &ctx.user_id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One catalog album.
#[utoipa::path(
    get,
    path = "/api/v3/library/albums/{id}",
    params(("id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "Album", body = AlbumView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album id"),
    )
)]
pub async fn get_album(
    State(deps): State<LibraryDeps>,
    LibraryUser(ctx): LibraryUser,
    Path(id): Path<String>,
) -> Result<Json<AlbumView>, LibraryError> {
    services::get_album(&deps, &ctx.user_id, &id)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of an album's streamable tracks.
#[utoipa::path(
    get,
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
) -> Result<Json<TrackPage>, LibraryError> {
    services::album_tracks(&deps, &ctx.user_id, &id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Other local albums sharing the album's release group.
#[utoipa::path(
    get,
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
) -> Result<Json<AlbumPage>, LibraryError> {
    services::album_copies(&deps, &ctx.user_id, &id)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of catalog artists.
#[utoipa::path(
    get,
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
) -> Result<Json<ArtistPage>, LibraryError> {
    services::list_artists(&deps, &ctx.user_id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One catalog artist.
#[utoipa::path(
    get,
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
) -> Result<Json<ArtistView>, LibraryError> {
    services::get_artist(&deps, &ctx.user_id, &id)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of albums led by the artist.
#[utoipa::path(
    get,
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
) -> Result<Json<AlbumPage>, LibraryError> {
    services::artist_albums(&deps, &ctx.user_id, &id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of albums where the artist appears without leading.
#[utoipa::path(
    get,
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
) -> Result<Json<AlbumPage>, LibraryError> {
    services::artist_appearances(&deps, &ctx.user_id, &id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of streamable tracks.
#[utoipa::path(
    get,
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
) -> Result<Json<TrackPage>, LibraryError> {
    services::list_tracks(&deps, &ctx.user_id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One streamable track.
#[utoipa::path(
    get,
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
) -> Result<Json<TrackView>, LibraryError> {
    services::get_track(&deps, &ctx.user_id, &id)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Stored lyrics for one track.
#[utoipa::path(
    get,
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
) -> Result<Json<LyricsView>, LibraryError> {
    let _ = ctx;
    services::lyrics(&deps, &id)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Library totals plus the caller's favorite counts.
#[utoipa::path(
    get,
    path = "/api/v3/library/stats",
    responses(
        (status = 200, description = "Library totals", body = StatsView),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_stats(
    State(deps): State<LibraryDeps>,
    LibraryUser(ctx): LibraryUser,
) -> Result<Json<StatsView>, LibraryError> {
    services::stats(&deps, &ctx.user_id)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Newest albums first, capped.
#[utoipa::path(
    get,
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
) -> Result<Json<AlbumPage>, LibraryError> {
    services::recently_added(&deps, &ctx.user_id, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Full genre listing.
#[utoipa::path(
    get,
    path = "/api/v3/library/genres",
    responses(
        (status = 200, description = "Genres", body = GenreList),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_genres(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
) -> Result<Json<GenreList>, LibraryError> {
    services::genres(&deps)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of a genre's streamable tracks.
#[utoipa::path(
    get,
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
) -> Result<Json<TrackPage>, LibraryError> {
    services::genre_tracks(&deps, &ctx.user_id, &name, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Album-card browse for the local library.
#[utoipa::path(
    get,
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
) -> Result<Json<AlbumCardPage>, LibraryError> {
    services::browse_albums(&deps, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Album plus track search.
#[utoipa::path(
    get,
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
) -> Result<Json<SearchResults>, LibraryError> {
    services::search_library(&deps, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Newest album cards first, capped.
#[utoipa::path(
    get,
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
) -> Result<Json<Vec<super::models::AlbumCard>>, LibraryError> {
    services::recent_albums(&deps, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Decade shelves, oldest first.
#[utoipa::path(
    get,
    path = "/api/v3/local-library/decades",
    responses(
        (status = 200, description = "Decades", body = DecadesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_decades(
    State(deps): State<LibraryDeps>,
    LibraryUser(_ctx): LibraryUser,
) -> Result<Json<DecadesResponse>, LibraryError> {
    services::decades(&deps)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// One page of an album's streamable tracks by release-group mbid or
/// local album id.
#[utoipa::path(
    get,
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
) -> Result<Json<TrackPage>, LibraryError> {
    services::album_match(&deps, &ctx.user_id, &mbid, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}

/// Reason-tagged track suggestions.
#[utoipa::path(
    get,
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
) -> Result<Json<SuggestionsResponse>, LibraryError> {
    services::suggestions(&deps, &query)
        .await
        .map(Json)
        .map_err(|failure| failed(failure, &deps))
}
