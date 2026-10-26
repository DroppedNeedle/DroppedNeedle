//! Native `/api/v3` remote-source handlers and the router constructor.
//!
//! Each handler parses the caller, the path, and the query, calls one
//! [`RemotesService`] method, and renders the result. Failures map to the
//! routes' error in [`render`] and from there onto `ApiError`; query
//! strings parse through [`ValidQuery`] and bodies through [`ValidJson`] so
//! malformed input stays inside the envelope. [`routes`] puts every route
//! in the user group, inside the session gate, and every handler takes the
//! [`User`] extractor, so every route 401s anonymously. Operation ids
//! carry a `remotes_` prefix so they never collide with the library routes
//! of the same shape.

use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};

use super::adapter::{AlbumBrowse, ArtistBrowse, TrackBrowse};
use super::error::RemotesError;
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
}

/// Every remote-source route, for any signed-in user.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    RemotesDeps: FromRef<S>,
{
    Routes::new()
        .user(routes!(list_links))
        .user(routes!(get_hub))
        .user(routes!(get_stats))
        .user(routes!(list_albums))
        .user(routes!(get_album))
        .user(routes!(list_album_tracks))
        .user(routes!(list_artists))
        .user(routes!(get_artist_index))
        .user(routes!(get_artist))
        .user(routes!(list_tracks))
        .user(routes!(search))
        .user(routes!(get_recent))
        .user(routes!(get_recently_added))
        .user(routes!(get_favorites))
        .user(routes!(list_genres))
        .user(routes!(list_genre_songs))
        .user(routes!(list_moods))
        .user(routes!(get_filters))
        .user(routes!(list_most_played_albums))
        .user(routes!(list_most_played_artists))
        .user(routes!(list_playlists))
        .user(routes!(get_playlist))
        .user(routes!(import_playlist))
        .user(routes!(get_artist_info))
        .user(routes!(get_album_info))
        .user(routes!(get_lyrics))
        .user(routes!(get_top_songs))
        .user(routes!(get_similar))
        .user(routes!(get_random))
        .user(routes!(get_discovery))
        .user(routes!(get_mix))
        .user(routes!(list_sessions))
        .user(routes!(list_history))
        .user(routes!(get_analytics))
        .user(routes!(get_image))
        .user(routes!(get_playlist_cover))
        .user(routes!(match_album))
        .user(routes!(get_connection, put_connection, delete_connection))
        .user(routes!(get_folders, put_folders))
}

/// Map a service failure to the routes' error. Upstream detail reaches the
/// log only, through the request boundary.
fn render(failure: RemotesFailure) -> RemotesError {
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
        RemotesFailure::Upstream { source, detail } => RemotesError::Unavailable {
            cause: format!("{} answered with an error: {detail}", source.display()),
        },
        RemotesFailure::Unreachable { source, detail } => RemotesError::Unavailable {
            cause: format!("{} is unreachable: {detail}", source.display()),
        },
        RemotesFailure::NotFound => RemotesError::NotFound,
        RemotesFailure::Unsupported(message) => RemotesError::Unsupported { message },
        RemotesFailure::InvalidInput(message) => RemotesError::InvalidInput { message },
        RemotesFailure::Conflict(message) => RemotesError::Conflict { message },
        RemotesFailure::Internal(cause) => RemotesError::Internal {
            cause: cause.to_string(),
        },
    }
}

/// Parse a `{source}` path segment into a source or a 400.
fn parse_source(raw: &str) -> Result<SourceName, RemotesError> {
    SourceName::parse(raw).ok_or_else(|| RemotesError::InvalidInput {
        message: format!("Unknown remote source '{raw}': want jellyfin, navidrome, or plex"),
    })
}

/// Shorthand for the handlers below: render a service result as JSON.
fn reply<T>(result: Result<T, RemotesFailure>) -> Result<Json<T>, ApiError> {
    result.map(Json).map_err(|failure| render(failure).into())
}

/// The caller's linked accounts across every service.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/me/connections", operation_id = "me_list_connections",
    responses((status = 200, description = "Linked accounts", body = LinkedAccounts)))]
pub async fn list_links(
    State(deps): State<RemotesDeps>,
    User(user): User,
) -> Result<Json<LinkedAccounts>, ApiError> {
    let links = deps
        .service
        .links(user.user_id())
        .await
        .map(|links| LinkedAccounts {
            connections: links
                .into_iter()
                .map(|link| LinkedAccount {
                    service: link.service,
                    enabled: link.enabled,
                    username: link.username,
                })
                .collect(),
        });
    reply(links)
}

/// Hub highlights for one source.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/hub", operation_id = "remotes_get_hub",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Hub highlights", body = HubView)))]
pub async fn get_hub(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<HubView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.hub(user.user_id(), source).await)
}

/// Library totals for one source.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/stats", operation_id = "remotes_get_stats",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Library totals", body = StatsView)))]
pub async fn get_stats(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<StatsView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.stats(user.user_id(), source).await)
}

/// One page of albums.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/albums", operation_id = "remotes_list_albums",
    params(("source" = SourceName, Path, description = "Remote source"), AlbumBrowseQuery),
    responses((status = 200, description = "Album page", body = AlbumPage)))]
pub async fn list_albums(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<AlbumBrowseQuery>,
) -> Result<Json<AlbumPage>, ApiError> {
    let source = parse_source(&source)?;
    let browse = AlbumBrowse {
        limit: query.limit.unwrap_or(50).clamp(1, 500),
        offset: query.offset.unwrap_or(0).max(0),
        sort_by: query.sort_by.unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        genre: query.genre.unwrap_or_default(),
        year: query.year,
        decade: query.decade.unwrap_or_default(),
        mood: query.mood.unwrap_or_default(),
        tags: query.tags.unwrap_or_default(),
        studios: query.studios.unwrap_or_default(),
    };
    reply(deps.service.albums(user.user_id(), source, browse).await)
}

/// One album by id.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/albums/{id}", operation_id = "remotes_get_album",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote album id")),
    responses((status = 200, description = "Album detail", body = AlbumView)))]
pub async fn get_album(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<AlbumView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.album(user.user_id(), source, id).await)
}

/// Tracks of one album.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/albums/{id}/tracks",
    operation_id = "remotes_list_album_tracks",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote album id")),
    responses((status = 200, description = "Album tracks", body = TrackPage)))]
pub async fn list_album_tracks(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.album_tracks(user.user_id(), source, id).await)
}

/// One page of artists.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/artists", operation_id = "remotes_list_artists",
    params(("source" = SourceName, Path, description = "Remote source"), ArtistBrowseQuery),
    responses((status = 200, description = "Artist page", body = ArtistPage)))]
pub async fn list_artists(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<ArtistBrowseQuery>,
) -> Result<Json<ArtistPage>, ApiError> {
    let source = parse_source(&source)?;
    let browse = ArtistBrowse {
        limit: query.limit.unwrap_or(50).clamp(1, 500),
        offset: query.offset.unwrap_or(0).max(0),
        sort_by: query.sort_by.unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        search: query.search.unwrap_or_default(),
    };
    reply(deps.service.artists(user.user_id(), source, browse).await)
}

/// Full alphabetic artist index.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/artists/index",
    operation_id = "remotes_get_artist_index",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Artist index", body = ArtistIndex)))]
pub async fn get_artist_index(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<ArtistIndex>, ApiError> {
    let source = parse_source(&source)?;
    let index = deps
        .service
        .artist_index(user.user_id(), source)
        .await
        .map(|index| ArtistIndex { index });
    reply(index)
}

/// One artist by id.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/artists/{id}", operation_id = "remotes_get_artist",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote artist id")),
    responses((status = 200, description = "Artist detail", body = ArtistView)))]
pub async fn get_artist(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<ArtistView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.artist(user.user_id(), source, id).await)
}

/// One page of tracks.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/tracks", operation_id = "remotes_list_tracks",
    params(("source" = SourceName, Path, description = "Remote source"), TrackBrowseQuery),
    responses((status = 200, description = "Track page", body = TrackPage)))]
pub async fn list_tracks(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<TrackBrowseQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    let browse = TrackBrowse {
        limit: query.limit.unwrap_or(50).clamp(1, 500),
        offset: query.offset.unwrap_or(0).max(0),
        sort_by: query.sort_by.unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        search: query.search.unwrap_or_default(),
        genre: query.genre.unwrap_or_default(),
    };
    reply(deps.service.tracks(user.user_id(), source, browse).await)
}

/// Unified search across artists, albums, and tracks.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/search", operation_id = "remotes_search",
    params(("source" = SourceName, Path, description = "Remote source"), SearchQuery),
    responses((status = 200, description = "Search results", body = SearchResults)))]
pub async fn search(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<SearchQuery>,
) -> Result<Json<SearchResults>, ApiError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    reply(
        deps.service
            .search(user.user_id(), source, query.q, limit)
            .await,
    )
}

/// Recently played albums.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/recent", operation_id = "remotes_list_recent",
    params(("source" = SourceName, Path, description = "Remote source"), PageQuery),
    responses((status = 200, description = "Recently played", body = Vec<AlbumView>)))]
pub async fn get_recent(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<Vec<AlbumView>>, ApiError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(deps.service.recent(user.user_id(), source, limit).await)
}

/// Recently added albums.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/recently-added",
    operation_id = "remotes_list_recently_added",
    params(("source" = SourceName, Path, description = "Remote source"), PageQuery),
    responses((status = 200, description = "Recently added", body = Vec<AlbumView>)))]
pub async fn get_recently_added(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<Vec<AlbumView>>, ApiError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(
        deps.service
            .recently_added(user.user_id(), source, limit)
            .await,
    )
}

/// Favorite artists, albums, and tracks.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/favorites", operation_id = "remotes_get_favorites",
    params(("source" = SourceName, Path, description = "Remote source"), FavoritesQuery),
    responses((status = 200, description = "Favorites", body = FavoritesView)))]
pub async fn get_favorites(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<FavoritesQuery>,
) -> Result<Json<FavoritesView>, ApiError> {
    let source = parse_source(&source)?;
    reply(
        deps.service
            .favorites(user.user_id(), source, query.clamped())
            .await,
    )
}

/// Genre labels.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/genres", operation_id = "remotes_list_genres",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Genre labels", body = Vec<String>)))]
pub async fn list_genres(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.genres(user.user_id(), source).await)
}

/// Tracks carrying one genre label.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/genres/songs",
    operation_id = "remotes_list_genre_songs",
    params(("source" = SourceName, Path, description = "Remote source"), GenreSongsQuery),
    responses((status = 200, description = "Genre tracks", body = TrackPage)))]
pub async fn list_genre_songs(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<GenreSongsQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let genres: Vec<String> = query
        .genres
        .iter()
        .flat_map(|list| list.split(','))
        .chain(query.genre.as_deref())
        .map(str::trim)
        .filter(|genre| !genre.is_empty())
        .map(str::to_owned)
        .collect();
    reply(
        deps.service
            .genre_songs(user.user_id(), source, genres, limit, offset)
            .await,
    )
}

/// Mood labels (Plex).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/moods", operation_id = "remotes_list_moods",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Mood labels", body = Vec<String>)))]
pub async fn list_moods(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.moods(user.user_id(), source).await)
}

/// Album filter facets (Jellyfin).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/filters", operation_id = "remotes_get_filters",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Filter facets", body = FilterFacetsView)))]
pub async fn get_filters(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<FilterFacetsView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.filters(user.user_id(), source).await)
}

/// Most-played albums (Jellyfin).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/most-played/albums",
    operation_id = "remotes_list_most_played_albums",
    params(("source" = SourceName, Path, description = "Remote source"), MostPlayedQuery),
    responses((status = 200, description = "Most-played albums", body = Vec<AlbumView>)))]
pub async fn list_most_played_albums(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MostPlayedQuery>,
) -> Result<Json<Vec<AlbumView>>, ApiError> {
    let source = parse_source(&source)?;
    reply(
        deps.service
            .most_played_albums(user.user_id(), source, query.clamped())
            .await,
    )
}

/// Most-played artists (Jellyfin).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/most-played/artists",
    operation_id = "remotes_list_most_played_artists",
    params(("source" = SourceName, Path, description = "Remote source"), MostPlayedQuery),
    responses((status = 200, description = "Most-played artists", body = Vec<ArtistView>)))]
pub async fn list_most_played_artists(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MostPlayedQuery>,
) -> Result<Json<Vec<ArtistView>>, ApiError> {
    let source = parse_source(&source)?;
    reply(
        deps.service
            .most_played_artists(user.user_id(), source, query.clamped())
            .await,
    )
}

/// The caller's playlists on the server.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/playlists", operation_id = "remotes_list_playlists",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Playlists", body = PlaylistCollection)))]
pub async fn list_playlists(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<PlaylistCollection>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.playlists(user.user_id(), source).await)
}

/// One playlist with its tracks.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/playlists/{id}",
    operation_id = "remotes_get_playlist",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote playlist id")),
    responses((status = 200, description = "Playlist detail", body = PlaylistDetail)))]
pub async fn get_playlist(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<PlaylistDetail>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.playlist(user.user_id(), source, id).await)
}

/// Import one remote playlist into the caller's playlists.
#[utoipa::path(post, tag = "remotes", path = "/api/v3/remotes/{source}/playlists/{id}/import",
    operation_id = "remotes_import_playlist",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote playlist id")),
    responses((status = 200, description = "Import receipt", body = ImportResult)))]
pub async fn import_playlist(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<ImportResult>, ApiError> {
    let source = parse_source(&source)?;
    reply(
        deps.service
            .import_playlist(user.user_id(), source, id)
            .await,
    )
}

/// Artist info passthrough.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/info/artists/{id}",
    operation_id = "remotes_get_artist_info",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote artist id")),
    responses((status = 200, description = "Artist info", body = InfoView)))]
pub async fn get_artist_info(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<InfoView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.artist_info(user.user_id(), source, id).await)
}

/// Album info passthrough.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/info/albums/{id}",
    operation_id = "remotes_get_album_info",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote album id")),
    responses((status = 200, description = "Album info", body = InfoView)))]
pub async fn get_album_info(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<InfoView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.album_info(user.user_id(), source, id).await)
}

/// Lyrics for one track.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/lyrics/{id}", operation_id = "remotes_get_lyrics",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote track id"), LyricsQuery),
    responses((status = 200, description = "Lyrics", body = LyricsView)))]
pub async fn get_lyrics(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<LyricsQuery>,
) -> Result<Json<LyricsView>, ApiError> {
    let source = parse_source(&source)?;
    reply(
        deps.service
            .lyrics(user.user_id(), source, id, query.artist, query.title)
            .await,
    )
}

/// Top songs for one artist name.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/top/{artist}", operation_id = "remotes_list_top_songs",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("artist" = String, Path, description = "Artist name"), PageQuery),
    responses((status = 200, description = "Top songs", body = TrackPage)))]
pub async fn get_top_songs(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, artist)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(
        deps.service
            .top_songs(user.user_id(), source, artist, limit)
            .await,
    )
}

/// Tracks similar to one track.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/similar/{id}", operation_id = "remotes_list_similar",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote track id"), PageQuery),
    responses((status = 200, description = "Similar tracks", body = TrackPage)))]
pub async fn get_similar(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    reply(
        deps.service
            .similar(user.user_id(), source, id, limit)
            .await,
    )
}

/// Random tracks, optionally filtered by genre. Limits mirror the v2
/// Navidrome route (default 20, max 50); Plex answers unsupported.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/random", operation_id = "remotes_list_random",
    params(("source" = SourceName, Path, description = "Remote source"), RandomQuery),
    responses((status = 200, description = "Random tracks", body = TrackPage)))]
pub async fn get_random(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<RandomQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    reply(
        deps.service
            .random(
                user.user_id(),
                source,
                limit,
                query.genre.unwrap_or_default(),
            )
            .await,
    )
}

/// Discovery shelves (Plex).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/discovery", operation_id = "remotes_get_discovery",
    params(("source" = SourceName, Path, description = "Remote source"), DiscoveryQuery),
    responses((status = 200, description = "Discovery shelves", body = DiscoveryView)))]
pub async fn get_discovery(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<DiscoveryQuery>,
) -> Result<Json<DiscoveryView>, ApiError> {
    let source = parse_source(&source)?;
    let count = query.count.unwrap_or(10).clamp(1, 20);
    reply(deps.service.discovery(user.user_id(), source, count).await)
}

/// Instant mix for an item, artist, or genre id (Jellyfin).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/mix/{id}", operation_id = "remotes_get_mix",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Seed id or genre name"), MixQuery),
    responses((status = 200, description = "Instant mix", body = TrackPage)))]
pub async fn get_mix(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<MixQuery>,
) -> Result<Json<TrackPage>, ApiError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let seed = match query.kind.as_deref().unwrap_or("item") {
        "item" => MixSeed::Item(id),
        "artist" => MixSeed::Artist(id),
        "genre" => MixSeed::Genre(id),
        other => {
            return Err(RemotesError::InvalidInput {
                message: format!("Unknown mix kind '{other}': want item, artist, or genre"),
            }
            .into());
        }
    };
    reply(deps.service.mix(user.user_id(), source, seed, limit).await)
}

/// Active audio sessions.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/sessions", operation_id = "remotes_list_sessions",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Sessions", body = SessionsView)))]
pub async fn list_sessions(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<SessionsView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.sessions(user.user_id(), source).await)
}

/// Listening history, newest first.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/history", operation_id = "remotes_list_history",
    params(("source" = SourceName, Path, description = "Remote source"), HistoryQuery),
    responses((status = 200, description = "History page", body = HistoryPage)))]
pub async fn list_history(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<Json<HistoryPage>, ApiError> {
    let source = parse_source(&source)?;
    let (limit, offset) = query.clamped();
    reply(
        deps.service
            .history(user.user_id(), source, limit, offset)
            .await,
    )
}

/// Listening analytics over the history (Plex).
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/analytics", operation_id = "remotes_get_analytics",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Listening analytics", body = AnalyticsView)))]
pub async fn get_analytics(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<AnalyticsView>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.analytics(user.user_id(), source).await)
}

/// Item image bytes. Immutable and cacheable for a year: image URLs carry
/// the upstream tag, so a new tag is a new URL.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/images/{id}", operation_id = "remotes_get_image",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote image id"), ImageQuery),
    responses((status = 200, description = "Image bytes")))]
pub async fn get_image(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<ImageQuery>,
) -> Result<Response, ApiError> {
    let source = parse_source(&source)?;
    let (bytes, content_type) = deps
        .service
        .image(user.user_id(), source, id, query.clamped())
        .await
        .map_err(render)?;
    bytes_response(
        source,
        bytes,
        &content_type,
        "public, max-age=31536000, immutable",
    )
}

/// Playlist cover bytes. Never cached: playlist art follows membership.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/covers/playlists/{id}",
    operation_id = "remotes_get_playlist_cover",
    params(("source" = SourceName, Path, description = "Remote source"),
        ("id" = String, Path, description = "Remote playlist id"), ImageQuery),
    responses((status = 200, description = "Playlist cover bytes")))]
pub async fn get_playlist_cover(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<ImageQuery>,
) -> Result<Response, ApiError> {
    let source = parse_source(&source)?;
    let (bytes, content_type) = deps
        .service
        .playlist_cover(user.user_id(), source, id, query.clamped())
        .await
        .map_err(render)?;
    bytes_response(source, bytes, &content_type, "private, no-store")
}

/// The image types the routes relay, matched on the type alone without
/// its parameters. Anything else the remote server sent is not an image
/// here. SVG is left out because it can carry script.
fn image_content_type(content_type: &str) -> Option<&'static str> {
    let essence = content_type.split(';').next().unwrap_or_default().trim();
    [
        ("image/jpeg", "image/jpeg"),
        ("image/jpg", "image/jpeg"),
        ("image/png", "image/png"),
        ("image/webp", "image/webp"),
        ("image/gif", "image/gif"),
        ("image/avif", "image/avif"),
    ]
    .into_iter()
    .find(|(sent, _)| essence.eq_ignore_ascii_case(sent))
    .map(|(_, served)| served)
}

/// Relay image bytes from a remote server. The bytes are that server's
/// and go out under this app's address, so the type is one of ours, never
/// the server's own header, and the browser is told not to guess another
/// and not to run anything in it if it is opened as a page.
fn bytes_response(
    source: SourceName,
    bytes: Vec<u8>,
    content_type: &str,
    cache_control: &'static str,
) -> Result<Response, ApiError> {
    let Some(content_type) = image_content_type(content_type) else {
        let sent: String = content_type.chars().take(100).collect();
        tracing::warn!(
            source = source.as_str(),
            content_type = %sent,
            "remote image has a content type that is not an image, treating it as missing"
        );
        return Err(ApiError::not_found());
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// The caller's copy of an album on one source.
///
/// Found by MusicBrainz id, or on Plex and Navidrome by title and artist
/// when no album there carries the id.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/match", operation_id = "remotes_match_album",
    params(("source" = SourceName, Path, description = "Remote source"), MatchQuery),
    responses((status = 200, description = "Match result", body = MatchView)))]
pub async fn match_album(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MatchQuery>,
) -> Result<Json<MatchView>, ApiError> {
    let source = parse_source(&source)?;
    reply(
        deps.service
            .match_album(user.user_id(), source, query)
            .await,
    )
}

/// Connection status for one source. Never carries credential material.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/{source}/connection",
    operation_id = "remotes_get_connection",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn get_connection(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<ConnectionStatus>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.connection(user.user_id(), source).await)
}

/// Link the caller's own Navidrome or Jellyfin account. The password is
/// checked against the server first and never echoed.
#[utoipa::path(put, tag = "remotes", path = "/api/v3/remotes/{source}/connection",
    operation_id = "remotes_put_connection",
    params(("source" = SourceName, Path, description = "Remote source")),
    request_body = ConnectionSave,
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn put_connection(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
    ValidJson(payload): ValidJson<ConnectionSave>,
) -> Result<Json<ConnectionStatus>, ApiError> {
    let source = parse_source(&source)?;
    let login = AccountLogin {
        username: payload.username,
        password: payload.password,
    };
    reply(deps.service.connect(user.user_id(), source, login).await)
}

/// Remove the caller's own link for one source.
#[utoipa::path(delete, tag = "remotes", path = "/api/v3/remotes/{source}/connection",
    operation_id = "remotes_delete_connection",
    params(("source" = SourceName, Path, description = "Remote source")),
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn delete_connection(
    State(deps): State<RemotesDeps>,
    User(user): User,
    Path(source): Path<String>,
) -> Result<Json<ConnectionStatus>, ApiError> {
    let source = parse_source(&source)?;
    reply(deps.service.disconnect(user.user_id(), source).await)
}

/// Navidrome folder preference resolution for the caller.
#[utoipa::path(get, tag = "remotes", path = "/api/v3/remotes/navidrome/folders",
    operation_id = "remotes_get_navidrome_folders",
    responses((status = 200, description = "Folder resolution", body = FolderResolutionView)))]
pub async fn get_folders(
    State(deps): State<RemotesDeps>,
    User(user): User,
) -> Result<Json<FolderResolutionView>, ApiError> {
    reply(deps.service.folders(user.user_id()).await)
}

/// Save the Navidrome folder preference.
#[utoipa::path(put, tag = "remotes", path = "/api/v3/remotes/navidrome/folders",
    operation_id = "remotes_put_navidrome_folders",
    request_body = FolderSave,
    responses((status = 200, description = "Folder resolution", body = FolderResolutionView)))]
pub async fn put_folders(
    State(deps): State<RemotesDeps>,
    User(user): User,
    ValidJson(payload): ValidJson<FolderSave>,
) -> Result<Json<FolderResolutionView>, ApiError> {
    reply(
        deps.service
            .save_folders(user.user_id(), &payload.mode, &payload.selected_folder_ids)
            .await,
    )
}
