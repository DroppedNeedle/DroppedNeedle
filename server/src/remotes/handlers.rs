//! Native `/api/v3` remote-source handlers and the router constructor.
//!
//! Each handler parses the caller, the path, and the query, calls one
//! [`RemotesService`] method, and renders the result. Failures map to the
//! shared envelope in [`render`]; query strings parse through
//! [`ValidQuery`] and bodies through [`ValidJson`] so malformed input stays
//! inside the envelope. `MediaSetup` mounts [`remotes_router`] inside the
//! session gate, and every handler takes the [`RemotesUser`] extractor, so
//! every route 401s anonymously. Operation ids carry a `remotes_` prefix so
//! they never collide with the library routes of the same shape.

use std::sync::Arc;

use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::UsersDeps;
use crate::auth::users::stores::StoreError as UserStoreError;
use crate::ids::IdGenerator;
use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use super::adapter::{AlbumBrowse, ArtistBrowse, TrackBrowse};
use super::error::{RemotesError, ValidJson, ValidQuery};
use super::jellyfin::MixSeed;
use super::models::{
    AlbumBrowseQuery, AlbumPage, AlbumView, AnalyticsView, ArtistBrowseQuery, ArtistIndex,
    ArtistPage, ArtistView, ConnectionSave, ConnectionStatus, DiscoveryQuery, DiscoveryView,
    FavoritesQuery, FavoritesView, FilterFacetsView, FolderResolutionView, FolderSave,
    GenreSongsQuery, HistoryPage, HistoryQuery, HubView, ImageQuery, ImportResult, InfoView,
    LinkedAccount, LinkedAccounts, LyricsQuery, LyricsView, MatchQuery, MatchView, MixQuery,
    MostPlayedQuery, PageQuery, PlaylistCollection, PlaylistDetail, RandomQuery, SearchQuery,
    SearchResults, SessionsView, SourceName, StatsView, TrackBrowseQuery, TrackPage,
};
use super::service::{AccountLogin, RemotesFailure, RemotesService};

/// Every dependency the remotes routes need, injected by constructor.
#[derive(Clone)]
pub struct RemotesDeps {
    /// The remotes service.
    pub service: RemotesService,
    /// Auth bundle, used only to resolve the caller.
    pub auth: UsersDeps,
    /// Fresh ids for 5xx error ids.
    pub ids: Arc<dyn IdGenerator>,
}

/// Authenticated router. Paths are relative: the app nests this under
/// `/api/v3` inside the deny-by-default session gate.
pub fn remotes_router(deps: RemotesDeps) -> axum::Router {
    axum::Router::new()
        .route("/me/connections", get(list_links))
        .route("/remotes/{source}/hub", get(get_hub))
        .route("/remotes/{source}/stats", get(get_stats))
        .route("/remotes/{source}/albums", get(list_albums))
        .route("/remotes/{source}/albums/{id}", get(get_album))
        .route(
            "/remotes/{source}/albums/{id}/tracks",
            get(list_album_tracks),
        )
        .route("/remotes/{source}/artists", get(list_artists))
        .route("/remotes/{source}/artists/index", get(get_artist_index))
        .route("/remotes/{source}/artists/{id}", get(get_artist))
        .route("/remotes/{source}/tracks", get(list_tracks))
        .route("/remotes/{source}/search", get(search))
        .route("/remotes/{source}/recent", get(get_recent))
        .route("/remotes/{source}/recently-added", get(get_recently_added))
        .route("/remotes/{source}/favorites", get(get_favorites))
        .route("/remotes/{source}/genres", get(list_genres))
        .route("/remotes/{source}/genres/songs", get(list_genre_songs))
        .route("/remotes/{source}/moods", get(list_moods))
        .route("/remotes/{source}/filters", get(get_filters))
        .route(
            "/remotes/{source}/most-played/albums",
            get(list_most_played_albums),
        )
        .route(
            "/remotes/{source}/most-played/artists",
            get(list_most_played_artists),
        )
        .route("/remotes/{source}/playlists", get(list_playlists))
        .route("/remotes/{source}/playlists/{id}", get(get_playlist))
        .route(
            "/remotes/{source}/playlists/{id}/import",
            post(import_playlist),
        )
        .route("/remotes/{source}/info/artists/{id}", get(get_artist_info))
        .route("/remotes/{source}/info/albums/{id}", get(get_album_info))
        .route("/remotes/{source}/lyrics/{id}", get(get_lyrics))
        .route("/remotes/{source}/top/{artist}", get(get_top_songs))
        .route("/remotes/{source}/similar/{id}", get(get_similar))
        .route("/remotes/{source}/random", get(get_random))
        .route("/remotes/{source}/discovery", get(get_discovery))
        .route("/remotes/{source}/mix/{id}", get(get_mix))
        .route("/remotes/{source}/sessions", get(list_sessions))
        .route("/remotes/{source}/history", get(list_history))
        .route("/remotes/{source}/analytics", get(get_analytics))
        .route("/remotes/{source}/images/{id}", get(get_image))
        .route(
            "/remotes/{source}/covers/playlists/{id}",
            get(get_playlist_cover),
        )
        .route("/remotes/{source}/match", get(match_album))
        .route(
            "/remotes/{source}/connection",
            get(get_connection)
                .put(put_connection)
                .delete(delete_connection),
        )
        .route(
            "/remotes/navidrome/folders",
            get(get_folders).put(put_folders),
        )
        .with_state(deps)
}

/// Any authenticated user. A missing session or a session whose account
/// is gone reads as 401, mirroring the users role extractors.
pub struct RemotesUser(pub String);

impl FromRequestParts<RemotesDeps> for RemotesUser {
    type Rejection = RemotesError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &RemotesDeps,
    ) -> Result<Self, Self::Rejection> {
        let unauthorized = || RemotesError::Unauthorized {
            message: "Authentication required".to_owned(),
        };
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
                UserStoreError::Conflict => RemotesError::InvalidInput {
                    message: "Conflicting state".to_owned(),
                },
                UserStoreError::Internal(cause) => {
                    RemotesError::internal(&cause, state.ids.as_ref())
                }
            })?
            .ok_or_else(unauthorized)?;
        Ok(Self(user.id))
    }
}

/// Map a service failure to the wire. Upstream detail reaches the log
/// only; callers get a fixed user-safe summary per source.
fn render(failure: RemotesFailure, ids: &dyn IdGenerator) -> RemotesError {
    match failure {
        RemotesFailure::NotConfigured(source) => RemotesError::NotConfigured {
            message: format!("{} is not connected", source.display()),
        },
        RemotesFailure::AuthFailed(source) => RemotesError::AuthFailed {
            message: format!(
                "{} rejected the stored credential; reconnect it",
                source.display()
            ),
        },
        RemotesFailure::Upstream { source, detail } => {
            tracing::warn!(source = source.as_str(), %detail, "remote API error");
            RemotesError::Unavailable {
                message: format!("{} answered with an error", source.display()),
            }
        }
        RemotesFailure::Unreachable { source, detail } => {
            tracing::warn!(source = source.as_str(), %detail, "remote unreachable");
            RemotesError::Unavailable {
                message: format!("{} is unreachable", source.display()),
            }
        }
        RemotesFailure::NotFound => RemotesError::NotFound,
        RemotesFailure::Unsupported(message) => RemotesError::Unsupported { message },
        RemotesFailure::InvalidInput(message) => RemotesError::InvalidInput { message },
        RemotesFailure::Conflict(message) => RemotesError::Conflict { message },
        RemotesFailure::Internal(cause) => RemotesError::internal(&cause, ids),
    }
}

/// Parse a `{source}` path segment into a source or a 400.
fn parse_source(raw: &str) -> Result<SourceName, RemotesError> {
    SourceName::parse(raw).ok_or_else(|| RemotesError::InvalidInput {
        message: format!("Unknown remote source '{raw}': want jellyfin, navidrome, or plex"),
    })
}

/// Shorthand for the handlers below: render a service result as JSON.
fn reply<T>(
    deps: &RemotesDeps,
    result: Result<T, RemotesFailure>,
) -> Result<Json<T>, RemotesError> {
    result
        .map(Json)
        .map_err(|failure| render(failure, deps.ids.as_ref()))
}

/// The caller's linked accounts across every service.
#[utoipa::path(get, path = "/api/v3/me/connections", operation_id = "me_list_connections",
    responses((status = 200, description = "Linked accounts", body = LinkedAccounts)))]
pub async fn list_links(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
) -> Result<Json<LinkedAccounts>, RemotesError> {
    let links = deps.service.links(&user).await.map(|links| LinkedAccounts {
        connections: links
            .into_iter()
            .map(|link| LinkedAccount {
                service: link.service,
                enabled: link.enabled,
                username: link.username,
            })
            .collect(),
    });
    reply(&deps, links)
}

/// Hub highlights for one source.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/hub", operation_id = "remotes_get_hub",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Hub highlights", body = HubView)))]
pub async fn get_hub(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<HubView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.hub(&user, source).await)
}

/// Library totals for one source.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/stats", operation_id = "remotes_get_stats",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Library totals", body = StatsView)))]
pub async fn get_stats(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<StatsView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.stats(&user, source).await)
}

/// One page of albums.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/albums", operation_id = "remotes_list_albums",
    params(("source" = SourceName, Path, description = "Remote source"), AlbumBrowseQuery),
    responses((status = 200, description = "Album page", body = AlbumPage)))]
pub async fn list_albums(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<AlbumBrowseQuery>,
) -> Result<Json<AlbumPage>, RemotesError> {
    let source = parse_source(&source)?;
    let browse = AlbumBrowse {
        limit: query.limit.unwrap_or(50).clamp(1, 500),
        offset: query.offset.unwrap_or(0).max(0),
        sort_by: query.sort_by.unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        genre: query.genre.unwrap_or_default(),
        year: query.year,
        decade: query.decade.unwrap_or_default(),
    };
    reply(&deps, deps.service.albums(&user, source, browse).await)
}

/// One album by id.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/albums/{id}", operation_id = "remotes_get_album",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote album id")),
    responses((status = 200, description = "Album detail", body = AlbumView)))]
pub async fn get_album(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<AlbumView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.album(&user, source, id).await)
}

/// Tracks of one album.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/albums/{id}/tracks",
    operation_id = "remotes_list_album_tracks",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote album id")),
    responses((status = 200, description = "Album tracks", body = TrackPage)))]
pub async fn list_album_tracks(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.album_tracks(&user, source, id).await)
}

/// One page of artists.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/artists", operation_id = "remotes_list_artists",
    params(("source" = SourceName, Path, description = "Remote source"), ArtistBrowseQuery),
    responses((status = 200, description = "Artist page", body = ArtistPage)))]
pub async fn list_artists(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<ArtistBrowseQuery>,
) -> Result<Json<ArtistPage>, RemotesError> {
    let source = parse_source(&source)?;
    let browse = ArtistBrowse {
        limit: query.limit.unwrap_or(50).clamp(1, 500),
        offset: query.offset.unwrap_or(0).max(0),
        sort_by: query.sort_by.unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        search: query.search.unwrap_or_default(),
    };
    reply(&deps, deps.service.artists(&user, source, browse).await)
}

/// Full alphabetic artist index.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/artists/index",
    operation_id = "remotes_get_artist_index",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Artist index", body = ArtistIndex)))]
pub async fn get_artist_index(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<ArtistIndex>, RemotesError> {
    let source = parse_source(&source)?;
    let index = deps
        .service
        .artist_index(&user, source)
        .await
        .map(|index| ArtistIndex { index });
    reply(&deps, index)
}

/// One artist by id.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/artists/{id}", operation_id = "remotes_get_artist",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote artist id")),
    responses((status = 200, description = "Artist detail", body = ArtistView)))]
pub async fn get_artist(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<ArtistView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.artist(&user, source, id).await)
}

/// One page of tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/tracks", operation_id = "remotes_list_tracks",
    params(("source" = SourceName, Path, description = "Remote source"), TrackBrowseQuery),
    responses((status = 200, description = "Track page", body = TrackPage)))]
pub async fn list_tracks(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<TrackBrowseQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let browse = TrackBrowse {
        limit: query.limit.unwrap_or(50).clamp(1, 500),
        offset: query.offset.unwrap_or(0).max(0),
        sort_by: query.sort_by.unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        search: query.search.unwrap_or_default(),
        genre: query.genre.unwrap_or_default(),
    };
    reply(&deps, deps.service.tracks(&user, source, browse).await)
}

/// Unified search across artists, albums, and tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/search", operation_id = "remotes_search",
    params(("source" = SourceName, Path, description = "Remote source"), SearchQuery),
    responses((status = 200, description = "Search results", body = SearchResults)))]
pub async fn search(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<SearchQuery>,
) -> Result<Json<SearchResults>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    reply(
        &deps,
        deps.service.search(&user, source, query.q, limit).await,
    )
}

/// Recently played albums.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/recent", operation_id = "remotes_list_recent",
    params(("source" = SourceName, Path, description = "Remote source"), PageQuery),
    responses((status = 200, description = "Recently played", body = Vec<AlbumView>)))]
pub async fn get_recent(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<Vec<AlbumView>>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(&deps, deps.service.recent(&user, source, limit).await)
}

/// Recently added albums.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/recently-added",
    operation_id = "remotes_list_recently_added",
    params(("source" = SourceName, Path, description = "Remote source"), PageQuery),
    responses((status = 200, description = "Recently added", body = Vec<AlbumView>)))]
pub async fn get_recently_added(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<Vec<AlbumView>>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(
        &deps,
        deps.service.recently_added(&user, source, limit).await,
    )
}

/// Favorite artists, albums, and tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/favorites", operation_id = "remotes_get_favorites",
    params(("source" = SourceName, Path, description = "Remote source"), FavoritesQuery),
    responses((status = 200, description = "Favorites", body = FavoritesView)))]
pub async fn get_favorites(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<FavoritesQuery>,
) -> Result<Json<FavoritesView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(
        &deps,
        deps.service.favorites(&user, source, query.clamped()).await,
    )
}

/// Genre labels.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/genres", operation_id = "remotes_list_genres",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Genre labels", body = Vec<String>)))]
pub async fn list_genres(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<Vec<String>>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.genres(&user, source).await)
}

/// Tracks carrying one genre label.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/genres/songs",
    operation_id = "remotes_list_genre_songs",
    params(("source" = SourceName, Path, description = "Remote source"), GenreSongsQuery),
    responses((status = 200, description = "Genre tracks", body = TrackPage)))]
pub async fn list_genre_songs(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<GenreSongsQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    reply(
        &deps,
        deps.service
            .genre_songs(&user, source, query.genre, limit, offset)
            .await,
    )
}

/// Mood labels (Plex).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/moods", operation_id = "remotes_list_moods",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Mood labels", body = Vec<String>)))]
pub async fn list_moods(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<Vec<String>>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.moods(&user, source).await)
}

/// Album filter facets (Jellyfin).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/filters", operation_id = "remotes_get_filters",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Filter facets", body = FilterFacetsView)))]
pub async fn get_filters(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<FilterFacetsView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.filters(&user, source).await)
}

/// Most-played albums (Jellyfin).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/most-played/albums",
    operation_id = "remotes_list_most_played_albums",
    params(("source" = SourceName, Path, description = "Remote source"), MostPlayedQuery),
    responses((status = 200, description = "Most-played albums", body = Vec<AlbumView>)))]
pub async fn list_most_played_albums(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MostPlayedQuery>,
) -> Result<Json<Vec<AlbumView>>, RemotesError> {
    let source = parse_source(&source)?;
    reply(
        &deps,
        deps.service
            .most_played_albums(&user, source, query.clamped())
            .await,
    )
}

/// Most-played artists (Jellyfin).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/most-played/artists",
    operation_id = "remotes_list_most_played_artists",
    params(("source" = SourceName, Path, description = "Remote source"), MostPlayedQuery),
    responses((status = 200, description = "Most-played artists", body = Vec<ArtistView>)))]
pub async fn list_most_played_artists(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MostPlayedQuery>,
) -> Result<Json<Vec<ArtistView>>, RemotesError> {
    let source = parse_source(&source)?;
    reply(
        &deps,
        deps.service
            .most_played_artists(&user, source, query.clamped())
            .await,
    )
}

/// The caller's playlists on the server.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/playlists", operation_id = "remotes_list_playlists",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Playlists", body = PlaylistCollection)))]
pub async fn list_playlists(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<PlaylistCollection>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.playlists(&user, source).await)
}

/// One playlist with its tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/playlists/{id}",
    operation_id = "remotes_get_playlist",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote playlist id")),
    responses((status = 200, description = "Playlist detail", body = PlaylistDetail)))]
pub async fn get_playlist(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<PlaylistDetail>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.playlist(&user, source, id).await)
}

/// Import one remote playlist into the caller's playlists.
#[utoipa::path(post, path = "/api/v3/remotes/{source}/playlists/{id}/import",
    operation_id = "remotes_import_playlist",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote playlist id")),
    responses((status = 200, description = "Import receipt", body = ImportResult)))]
pub async fn import_playlist(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<ImportResult>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.import_playlist(&user, source, id).await)
}

/// Artist info passthrough.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/info/artists/{id}",
    operation_id = "remotes_get_artist_info",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote artist id")),
    responses((status = 200, description = "Artist info", body = InfoView)))]
pub async fn get_artist_info(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<InfoView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.artist_info(&user, source, id).await)
}

/// Album info passthrough.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/info/albums/{id}",
    operation_id = "remotes_get_album_info",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote album id")),
    responses((status = 200, description = "Album info", body = InfoView)))]
pub async fn get_album_info(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<InfoView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.album_info(&user, source, id).await)
}

/// Lyrics for one track.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/lyrics/{id}", operation_id = "remotes_get_lyrics",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote track id"), LyricsQuery),
    responses((status = 200, description = "Lyrics", body = LyricsView)))]
pub async fn get_lyrics(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<LyricsQuery>,
) -> Result<Json<LyricsView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(
        &deps,
        deps.service
            .lyrics(&user, source, id, query.artist, query.title)
            .await,
    )
}

/// Top songs for one artist name.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/top/{artist}", operation_id = "remotes_list_top_songs",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("artist" = String, Path, description = "Artist name"), PageQuery),
    responses((status = 200, description = "Top songs", body = TrackPage)))]
pub async fn get_top_songs(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, artist)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(
        &deps,
        deps.service.top_songs(&user, source, artist, limit).await,
    )
}

/// Tracks similar to one track.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/similar/{id}", operation_id = "remotes_list_similar",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote track id"), PageQuery),
    responses((status = 200, description = "Similar tracks", body = TrackPage)))]
pub async fn get_similar(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(&deps, deps.service.similar(&user, source, id, limit).await)
}

/// Random tracks, optionally filtered by genre. Limits mirror the v2
/// Navidrome route (default 20, max 50); Plex answers unsupported.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/random", operation_id = "remotes_list_random",
    params(("source" = SourceName, Path, description = "Remote source"), RandomQuery),
    responses((status = 200, description = "Random tracks", body = TrackPage)))]
pub async fn get_random(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<RandomQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    reply(
        &deps,
        deps.service
            .random(&user, source, limit, query.genre.unwrap_or_default())
            .await,
    )
}

/// Discovery shelves (Plex).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/discovery", operation_id = "remotes_get_discovery",
    params(("source" = SourceName, Path, description = "Remote source"), DiscoveryQuery),
    responses((status = 200, description = "Discovery shelves", body = DiscoveryView)))]
pub async fn get_discovery(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<DiscoveryQuery>,
) -> Result<Json<DiscoveryView>, RemotesError> {
    let source = parse_source(&source)?;
    let count = query.count.unwrap_or(10).clamp(1, 20);
    reply(&deps, deps.service.discovery(&user, source, count).await)
}

/// Instant mix for an item, artist, or genre id (Jellyfin).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/mix/{id}", operation_id = "remotes_get_mix",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Seed id or genre name"), MixQuery),
    responses((status = 200, description = "Instant mix", body = TrackPage)))]
pub async fn get_mix(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<MixQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let seed = match query.kind.as_deref().unwrap_or("item") {
        "item" => MixSeed::Item(id),
        "artist" => MixSeed::Artist(id),
        "genre" => MixSeed::Genre(id),
        other => {
            return Err(RemotesError::InvalidInput {
                message: format!("Unknown mix kind '{other}': want item, artist, or genre"),
            });
        }
    };
    reply(&deps, deps.service.mix(&user, source, seed, limit).await)
}

/// Active audio sessions.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/sessions", operation_id = "remotes_list_sessions",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Sessions", body = SessionsView)))]
pub async fn list_sessions(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<SessionsView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.sessions(&user, source).await)
}

/// Listening history, newest first.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/history", operation_id = "remotes_list_history",
    params(("source" = SourceName, Path, description = "Remote source"), HistoryQuery),
    responses((status = 200, description = "History page", body = HistoryPage)))]
pub async fn list_history(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<Json<HistoryPage>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, offset) = query.clamped();
    reply(
        &deps,
        deps.service.history(&user, source, limit, offset).await,
    )
}

/// Listening analytics over the history (Plex).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/analytics", operation_id = "remotes_get_analytics",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Listening analytics", body = AnalyticsView)))]
pub async fn get_analytics(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<AnalyticsView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.analytics(&user, source).await)
}

/// Item image bytes. Immutable and cacheable for a year: image URLs carry
/// the upstream tag, so a new tag is a new URL.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/images/{id}", operation_id = "remotes_get_image",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote image id"), ImageQuery),
    responses((status = 200, description = "Image bytes")))]
pub async fn get_image(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<ImageQuery>,
) -> Result<Response, RemotesError> {
    let source = parse_source(&source)?;
    let (bytes, content_type) = deps
        .service
        .image(&user, source, id, query.clamped())
        .await
        .map_err(|failure| render(failure, deps.ids.as_ref()))?;
    Ok(bytes_response(
        bytes,
        &content_type,
        "public, max-age=31536000, immutable",
    ))
}

/// Playlist cover bytes. Never cached: playlist art follows membership.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/covers/playlists/{id}",
    operation_id = "remotes_get_playlist_cover",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote playlist id"), ImageQuery),
    responses((status = 200, description = "Playlist cover bytes")))]
pub async fn get_playlist_cover(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<ImageQuery>,
) -> Result<Response, RemotesError> {
    let source = parse_source(&source)?;
    let (bytes, content_type) = deps
        .service
        .playlist_cover(&user, source, id, query.clamped())
        .await
        .map_err(|failure| render(failure, deps.ids.as_ref()))?;
    Ok(bytes_response(bytes, &content_type, "private, no-store"))
}

/// Upstream image content type, infallible: an unparseable one falls back
/// to `application/octet-stream` instead of 500ing the download.
fn image_content_type(content_type: &str) -> HeaderValue {
    HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

fn bytes_response(bytes: Vec<u8>, content_type: &str, cache_control: &'static str) -> Response {
    let content_type = image_content_type(content_type);
    let cache = HeaderValue::from_static(cache_control);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response()
}

/// The remote album behind a MusicBrainz id.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/match", operation_id = "remotes_match_album",
    params(("source" = SourceName, Path, description = "Remote source"), MatchQuery),
    responses((status = 200, description = "Match result", body = MatchView)))]
pub async fn match_album(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MatchQuery>,
) -> Result<Json<MatchView>, RemotesError> {
    let source = parse_source(&source)?;
    reply(
        &deps,
        deps.service.match_album(&user, source, query.mbid).await,
    )
}

/// Connection status for one source. Never carries credential material.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/connection",
    operation_id = "remotes_get_connection",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn get_connection(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<ConnectionStatus>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.connection(&user, source).await)
}

/// Link the caller's own Navidrome or Jellyfin account. The password is
/// checked against the server first and never echoed.
#[utoipa::path(put, path = "/api/v3/remotes/{source}/connection",
    operation_id = "remotes_put_connection",
    params(("source" = SourceName, Path, description = "Remote source")),
    request_body = ConnectionSave,
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn put_connection(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
    ValidJson(payload): ValidJson<ConnectionSave>,
) -> Result<Json<ConnectionStatus>, RemotesError> {
    let source = parse_source(&source)?;
    let login = AccountLogin {
        username: payload.username,
        password: payload.password,
    };
    reply(&deps, deps.service.connect(&user, source, login).await)
}

/// Remove the caller's own link for one source.
#[utoipa::path(delete, path = "/api/v3/remotes/{source}/connection",
    operation_id = "remotes_delete_connection",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn delete_connection(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<ConnectionStatus>, RemotesError> {
    let source = parse_source(&source)?;
    reply(&deps, deps.service.disconnect(&user, source).await)
}

/// Navidrome folder preference resolution for the caller.
#[utoipa::path(get, path = "/api/v3/remotes/navidrome/folders",
    operation_id = "remotes_get_navidrome_folders",
    responses((status = 200, description = "Folder resolution", body = FolderResolutionView)))]
pub async fn get_folders(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
) -> Result<Json<FolderResolutionView>, RemotesError> {
    reply(&deps, deps.service.folders(&user).await)
}

/// Save the Navidrome folder preference.
#[utoipa::path(put, path = "/api/v3/remotes/navidrome/folders",
    operation_id = "remotes_put_navidrome_folders",
    request_body = FolderSave,
    responses((status = 200, description = "Folder resolution", body = FolderResolutionView)))]
pub async fn put_folders(
    State(deps): State<RemotesDeps>,
    RemotesUser(user): RemotesUser,
    ValidJson(payload): ValidJson<FolderSave>,
) -> Result<Json<FolderResolutionView>, RemotesError> {
    reply(
        &deps,
        deps.service
            .save_folders(&user, &payload.mode, &payload.selected_folder_ids)
            .await,
    )
}
