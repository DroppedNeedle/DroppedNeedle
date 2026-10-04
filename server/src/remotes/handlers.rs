//! Native `/api/v3` remote-browse handlers and the router constructor.
//!
//! Handlers are thin: extract the caller, resolve their per-source handle,
//! call one adapter method, render the unified shape. Status mapping lives
//! in [`RemotesError`](super::error::RemotesError); query strings parse
//! through [`ValidQuery`] and bodies through [`ValidJson`] so malformed
//! input stays inside the shared envelope. The integrator mounts
//! [`remotes_router`] inside the session gate; every handler also takes the
//! slice-local user extractor, so every route 401s anonymously.

use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::UsersDeps;
use droppedneedle::auth::users::roles::AuthContext;
use droppedneedle::auth::users::stores::StoreError as UserStoreError;
use droppedneedle::ids::IdGenerator;
use serde::Deserialize;
use utoipa::IntoParams;

use super::adapter::{
    AdapterError, AlbumBrowse, ArtistBrowse, ImportSink, RemoteHandle, TrackBrowse, album_page,
    artist_page, playlist_collection, track_page,
};
use super::connections::{
    ConnectionDraft, ConnectionStore, CredentialCoder, ResolveError, SaveError, resolve_connection,
    save_connection,
};
use super::error::{RemotesError, ValidJson, ValidQuery};
use super::folders::{FolderSaveError, FolderStore, checked_preference, resolve_scope};
use super::jellyfin::JellyfinAdapter;
use super::models::{
    AlbumBrowseQuery, AlbumPage, ArtistBrowseQuery, ArtistIndex, ArtistPage, ConnectionSave,
    ConnectionStatus, DiscoveryQuery, DiscoveryView, FavoritesView, FolderResolutionView,
    FolderSave, GenreSongsQuery, HistoryPage, HistoryQuery, HubView, ImageQuery, ImportResult,
    InfoView, LyricsQuery, LyricsView, MatchQuery, MatchView, MusicFolderView, PageQuery,
    PlaylistCollection, PlaylistDetail, RandomQuery, SearchQuery, SearchResults, SessionsView,
    SourceName, StatsView, TrackBrowseQuery, TrackPage,
};
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;

/// Every dependency this slice needs, injected by constructor.
#[derive(Clone)]
pub struct RemotesDeps {
    /// Shared outbound HTTP client.
    pub http: reqwest::Client,
    /// Per-user connection rows.
    pub connections: Arc<dyn ConnectionStore>,
    /// Credential seal/open under the stage-2 key.
    pub coder: Arc<CredentialCoder>,
    /// Navidrome folder preferences.
    pub folders: Arc<dyn FolderStore>,
    /// Playlist import sink.
    pub imports: Arc<dyn ImportSink>,
    /// Auth bundle, used only to resolve the caller.
    pub auth: UsersDeps,
    /// Fresh ids for 5xx error ids.
    pub ids: Arc<dyn IdGenerator>,
}

/// Authenticated read router. Paths are relative: the app nests this under
/// `/api/v3` inside the deny-by-default session gate.
pub fn remotes_router(deps: RemotesDeps) -> axum::Router {
    axum::Router::new()
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

/// Any authenticated user. Missing session or a session whose account is
/// gone reads as 401, mirroring the sibling role extractors.
pub struct RemotesUser(pub AuthContext);

impl FromRequestParts<RemotesDeps> for RemotesUser {
    type Rejection = RemotesError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &RemotesDeps,
    ) -> Result<Self, Self::Rejection> {
        let session =
            parts
                .extensions
                .get::<CurrentSession>()
                .ok_or(RemotesError::Unauthorized {
                    message: "Authentication required".to_owned(),
                })?;
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
            .ok_or(RemotesError::Unauthorized {
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

/// Instant-mix seed query. `kind` is `item` (default), `artist`, or `genre`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MixQuery {
    /// Seed kind: `item`, `artist`, or `genre`.
    pub kind: Option<String>,
    /// Max tracks (1-200, default 50).
    pub limit: Option<i64>,
}

/// Parse a `{source}` path segment into a source or a 400.
fn parse_source(raw: &str) -> Result<SourceName, RemotesError> {
    SourceName::parse(raw).ok_or_else(|| RemotesError::InvalidInput {
        message: format!("Unknown remote source '{raw}': want jellyfin, navidrome, or plex"),
    })
}

/// Map an adapter failure to the wire. Upstream detail reaches the log
/// only; callers get a fixed user-safe summary per source.
fn failed(source: SourceName, error: AdapterError) -> RemotesError {
    let name = source.display();
    match error {
        AdapterError::NotConfigured => RemotesError::NotConfigured {
            message: format!("{name} is not connected"),
        },
        AdapterError::Auth => RemotesError::AuthFailed {
            message: format!("{name} rejected the stored credential; reconnect it"),
        },
        AdapterError::Api(detail) => {
            tracing::warn!(source = source.as_str(), %detail, "remote API error");
            RemotesError::Unavailable {
                message: format!("{name} answered with an error"),
            }
        }
        AdapterError::Transport(detail) => {
            tracing::warn!(source = source.as_str(), %detail, "remote unreachable");
            RemotesError::Unavailable {
                message: format!("{name} is unreachable"),
            }
        }
        AdapterError::NotFound => RemotesError::NotFound,
        AdapterError::Unsupported(message) => RemotesError::Unsupported { message },
    }
}

/// Resolve the caller's handle for one source, threading their Navidrome
/// folder scope when the source is Navidrome.
async fn handle_for(
    deps: &RemotesDeps,
    user_id: &str,
    source: SourceName,
) -> Result<RemoteHandle, RemotesError> {
    let resolved = resolve_connection(deps.connections.as_ref(), &deps.coder, user_id, source)
        .await
        .map_err(|error| match error {
            ResolveError::NotConfigured => RemotesError::NotConfigured {
                message: format!("{} is not connected", source.display()),
            },
            ResolveError::Stale => RemotesError::AuthFailed {
                message: format!(
                    "The stored {} credential no longer opens; reconnect it",
                    source.display()
                ),
            },
        })?;
    match source {
        SourceName::Jellyfin => Ok(RemoteHandle::Jellyfin(JellyfinAdapter::new(
            deps.http.clone(),
            resolved.base_url,
            resolved.credential,
            resolved.user_id,
        ))),
        SourceName::Navidrome => {
            let adapter = NavidromeAdapter::new(
                deps.http.clone(),
                resolved.base_url,
                resolved.username,
                resolved.credential,
            );
            let preference = deps.folders.get(user_id).await;
            let identity = adapter.server_identity();
            let folders = match adapter.music_folders().await {
                Ok(folders) => Some(folders),
                Err(AdapterError::Auth) => {
                    return Err(RemotesError::AuthFailed {
                        message: "Navidrome rejected the stored credential; reconnect it"
                            .to_owned(),
                    });
                }
                Err(_) => None,
            };
            let resolution = resolve_scope(&preference, folders.as_deref(), &identity);
            Ok(RemoteHandle::Navidrome(
                adapter.with_folders(resolution.scope.folder_ids),
            ))
        }
        SourceName::Plex => Ok(RemoteHandle::Plex(PlexAdapter::new(
            deps.http.clone(),
            resolved.base_url,
            resolved.credential,
            resolved.client_id,
            resolved.section_ids,
        ))),
    }
}

/// Hub highlights for one source.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/hub",
    responses((status = 200, description = "Hub highlights", body = HubView)))]
pub async fn get_hub(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<HubView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .hub()
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Library totals for one source.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/stats",
    responses((status = 200, description = "Library totals", body = StatsView)))]
pub async fn get_stats(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<StatsView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .stats()
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// One page of albums.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/albums",
    responses((status = 200, description = "Album page", body = AlbumPage)))]
pub async fn list_albums(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<AlbumBrowseQuery>,
) -> Result<Json<AlbumPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let browse = AlbumBrowse {
        limit,
        offset,
        sort_by: query.sort_by.clone().unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        genre: query.genre.clone().unwrap_or_default(),
        year: query.year,
        decade: query.decade.clone().unwrap_or_default(),
    };
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let page = handle
        .albums(&browse)
        .await
        .map_err(|error| failed(source, error))?;
    Ok(Json(album_page(page, offset, limit)))
}

/// One album by id.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/albums/{id}",
    responses((status = 200, description = "Album detail")))]
pub async fn get_album(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<super::models::AlbumView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let album = handle
        .album_detail(&id)
        .await
        .map_err(|error| failed(source, error))?
        .ok_or(RemotesError::NotFound)?;
    Ok(Json(album))
}

/// Tracks of one album.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/albums/{id}/tracks",
    responses((status = 200, description = "Album tracks", body = TrackPage)))]
pub async fn list_album_tracks(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let items = handle
        .album_tracks(&id)
        .await
        .map_err(|error| failed(source, error))?;
    let total = items.len() as i64;
    Ok(Json(TrackPage {
        items,
        total,
        offset: 0,
        limit: total,
    }))
}

/// One page of artists.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/artists",
    responses((status = 200, description = "Artist page", body = ArtistPage)))]
pub async fn list_artists(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<ArtistBrowseQuery>,
) -> Result<Json<ArtistPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let browse = ArtistBrowse {
        limit,
        offset,
        sort_by: query.sort_by.clone().unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        search: query.search.clone().unwrap_or_default(),
    };
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let page = handle
        .artists(&browse)
        .await
        .map_err(|error| failed(source, error))?;
    Ok(Json(artist_page(page, offset, limit)))
}

/// Full alphabetic artist index.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/artists/index",
    responses((status = 200, description = "Artist index", body = ArtistIndex)))]
pub async fn get_artist_index(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<ArtistIndex>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let index = handle
        .artist_index()
        .await
        .map_err(|error| failed(source, error))?;
    Ok(Json(ArtistIndex { index }))
}

/// One artist by id.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/artists/{id}",
    responses((status = 200, description = "Artist detail")))]
pub async fn get_artist(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<super::models::ArtistView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let artist = handle
        .artist_detail(&id)
        .await
        .map_err(|error| failed(source, error))?
        .ok_or(RemotesError::NotFound)?;
    Ok(Json(artist))
}

/// One page of tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/tracks",
    responses((status = 200, description = "Track page", body = TrackPage)))]
pub async fn list_tracks(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<TrackBrowseQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let browse = TrackBrowse {
        limit,
        offset,
        sort_by: query.sort_by.clone().unwrap_or_default(),
        descending: matches!(query.sort_order.as_deref(), Some("desc")),
        search: query.search.clone().unwrap_or_default(),
        genre: query.genre.clone().unwrap_or_default(),
    };
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let page = handle
        .tracks(&browse)
        .await
        .map_err(|error| failed(source, error))?;
    Ok(Json(track_page(page, offset, limit)))
}

/// Unified search across artists, albums, and tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/search",
    responses((status = 200, description = "Search results", body = SearchResults)))]
pub async fn search(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<SearchQuery>,
) -> Result<Json<SearchResults>, RemotesError> {
    let source = parse_source(&source)?;
    if query.q.trim().is_empty() {
        return Err(RemotesError::InvalidInput {
            message: "Search query must not be empty".to_owned(),
        });
    }
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .search(&query.q, limit)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Recently played albums.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/recent",
    responses((status = 200, description = "Recently played")))]
pub async fn get_recent(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<Vec<super::models::AlbumView>>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .recent(limit)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Recently added albums.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/recently-added",
    responses((status = 200, description = "Recently added")))]
pub async fn get_recently_added(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<Vec<super::models::AlbumView>>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .recently_added(limit)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Favorites grouped by kind.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/favorites",
    responses((status = 200, description = "Favorites", body = FavoritesView)))]
pub async fn get_favorites(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<FavoritesView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .favorites()
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Genre labels.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/genres",
    responses((status = 200, description = "Genre labels")))]
pub async fn list_genres(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<Vec<String>>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .genres()
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Tracks carrying one genre label.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/genres/songs",
    responses((status = 200, description = "Genre tracks", body = TrackPage)))]
pub async fn list_genre_songs(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<GenreSongsQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let items = handle
        .genre_songs(&query.genre, limit, offset)
        .await
        .map_err(|error| failed(source, error))?;
    let total = items.len() as i64;
    Ok(Json(TrackPage {
        items,
        total,
        offset,
        limit,
    }))
}

/// Playlists.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/playlists",
    responses((status = 200, description = "Playlists", body = PlaylistCollection)))]
pub async fn list_playlists(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<PlaylistCollection>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let items = handle
        .playlists()
        .await
        .map_err(|error| failed(source, error))?;
    Ok(Json(playlist_collection(items)))
}

/// One playlist with its tracks.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/playlists/{id}",
    responses((status = 200, description = "Playlist detail", body = PlaylistDetail)))]
pub async fn get_playlist(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<PlaylistDetail>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let detail = handle
        .playlist_detail(&id)
        .await
        .map_err(|error| failed(source, error))?
        .ok_or(RemotesError::NotFound)?;
    Ok(Json(detail))
}

/// Import one remote playlist into the local catalog.
#[utoipa::path(post, path = "/api/v3/remotes/{source}/playlists/{id}/import",
    responses((status = 200, description = "Import receipt", body = ImportResult)))]
pub async fn import_playlist(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<ImportResult>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let detail = handle
        .playlist_detail(&id)
        .await
        .map_err(|error| failed(source, error))?
        .ok_or(RemotesError::NotFound)?;
    let receipt = deps
        .imports
        .import(
            &ctx.user_id,
            source,
            &id,
            &detail.playlist.name,
            detail.tracks,
        )
        .await;
    Ok(Json(ImportResult::from(receipt)))
}

/// Artist info passthrough.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/info/artists/{id}",
    responses((status = 200, description = "Artist info", body = InfoView)))]
pub async fn get_artist_info(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<InfoView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .artist_info(&id)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Album info passthrough.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/info/albums/{id}",
    responses((status = 200, description = "Album info", body = InfoView)))]
pub async fn get_album_info(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
) -> Result<Json<InfoView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .album_info(&id)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Lyrics passthrough.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/lyrics/{id}",
    responses((status = 200, description = "Lyrics", body = LyricsView)))]
pub async fn get_lyrics(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<LyricsQuery>,
) -> Result<Json<LyricsView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let lyrics = handle
        .lyrics(&id, query.artist.as_deref(), query.title.as_deref())
        .await
        .map_err(|error| failed(source, error))?
        .ok_or(RemotesError::NotFound)?;
    Ok(Json(lyrics))
}

/// Top songs for one artist name.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/top/{artist}",
    responses((status = 200, description = "Top songs", body = TrackPage)))]
pub async fn get_top_songs(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, artist)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let items = handle
        .top_songs(&artist, limit)
        .await
        .map_err(|error| failed(source, error))?;
    let total = items.len() as i64;
    Ok(Json(TrackPage {
        items,
        total,
        offset: 0,
        limit,
    }))
}

/// Tracks similar to one track.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/similar/{id}",
    responses((status = 200, description = "Similar tracks", body = TrackPage)))]
pub async fn get_similar(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<PageQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, _) = query.clamped();
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let items = handle
        .similar(&id, limit)
        .await
        .map_err(|error| failed(source, error))?;
    let total = items.len() as i64;
    Ok(Json(TrackPage {
        items,
        total,
        offset: 0,
        limit,
    }))
}

/// Random tracks, optionally filtered by genre. Limits mirror the v1
/// Navidrome route (default 20, max 50); Plex answers unsupported.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/random",
    responses((status = 200, description = "Random tracks", body = TrackPage)))]
pub async fn get_random(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<RandomQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let genre = query.genre.as_deref().unwrap_or("");
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let items = handle
        .random(limit, genre)
        .await
        .map_err(|error| failed(source, error))?;
    let total = items.len() as i64;
    Ok(Json(TrackPage {
        items,
        total,
        offset: 0,
        limit,
    }))
}

/// Plex discovery shelves (Plex only, following the mix-route precedent:
/// the shape is source-specific, so the gate lives in the handler).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/discovery",
    responses((status = 200, description = "Discovery shelves", body = DiscoveryView)))]
pub async fn get_discovery(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<DiscoveryQuery>,
) -> Result<Json<DiscoveryView>, RemotesError> {
    let source = parse_source(&source)?;
    if source != SourceName::Plex {
        return Err(RemotesError::Unsupported {
            message: format!("{} has no discovery shelves", source.display()),
        });
    }
    let count = query.count.unwrap_or(10).clamp(1, 20);
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let RemoteHandle::Plex(adapter) = handle else {
        return Err(RemotesError::internal(
            &"discovery resolved a non-Plex handle",
            deps.ids.as_ref(),
        ));
    };
    adapter
        .discovery(count)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Instant mix for an item, artist, or genre id (Jellyfin only).
#[utoipa::path(get, path = "/api/v3/remotes/{source}/mix/{id}",
    responses((status = 200, description = "Instant mix", body = TrackPage)))]
pub async fn get_mix(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<MixQuery>,
) -> Result<Json<TrackPage>, RemotesError> {
    let source = parse_source(&source)?;
    if source != SourceName::Jellyfin {
        return Err(RemotesError::Unsupported {
            message: format!("{} has no instant-mix endpoint", source.display()),
        });
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let seed = match query.kind.as_deref().unwrap_or("item") {
        "item" => super::jellyfin::MixSeed::Item(id),
        "artist" => super::jellyfin::MixSeed::Artist(id),
        "genre" => super::jellyfin::MixSeed::Genre(id),
        other => {
            return Err(RemotesError::InvalidInput {
                message: format!("Unknown mix kind '{other}': want item, artist, or genre"),
            });
        }
    };
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let RemoteHandle::Jellyfin(adapter) = handle else {
        return Err(RemotesError::internal(
            &"mix resolved a non-Jellyfin handle",
            deps.ids.as_ref(),
        ));
    };
    let items = adapter
        .mix(&seed, limit)
        .await
        .map_err(|error| failed(source, error))?;
    let total = items.len() as i64;
    Ok(Json(TrackPage {
        items,
        total,
        offset: 0,
        limit,
    }))
}

/// Active audio sessions.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/sessions",
    responses((status = 200, description = "Sessions", body = SessionsView)))]
pub async fn list_sessions(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<SessionsView>, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .sessions()
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Listening history, newest first.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/history",
    responses((status = 200, description = "History page", body = HistoryPage)))]
pub async fn list_history(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<HistoryQuery>,
) -> Result<Json<HistoryPage>, RemotesError> {
    let source = parse_source(&source)?;
    let (limit, offset) = query.clamped();
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .history(limit, offset)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Item image bytes. Immutable and cacheable for a year: image URLs carry
/// the upstream tag, so a new tag is a new URL.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/images/{id}",
    responses((status = 200, description = "Image bytes")))]
pub async fn get_image(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<ImageQuery>,
) -> Result<Response, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let (bytes, content_type) = handle
        .image_bytes(&id, query.clamped())
        .await
        .map_err(|error| failed(source, error))?;
    Ok(bytes_response(
        bytes,
        &content_type,
        "public, max-age=31536000, immutable",
    ))
}

/// Playlist cover bytes. Never cached: playlist art follows membership.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/covers/playlists/{id}",
    responses((status = 200, description = "Playlist cover bytes")))]
pub async fn get_playlist_cover(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path((source, id)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<ImageQuery>,
) -> Result<Response, RemotesError> {
    let source = parse_source(&source)?;
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    let (bytes, content_type) = handle
        .playlist_cover_bytes(&id, query.clamped())
        .await
        .map_err(|error| failed(source, error))?;
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

/// MBID match for one source.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/match",
    responses((status = 200, description = "Match result", body = MatchView)))]
pub async fn match_album(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidQuery(query): ValidQuery<MatchQuery>,
) -> Result<Json<MatchView>, RemotesError> {
    let source = parse_source(&source)?;
    if query.mbid.trim().is_empty() {
        return Err(RemotesError::InvalidInput {
            message: "Match mbid must not be empty".to_owned(),
        });
    }
    let handle = handle_for(&deps, &ctx.user_id, source).await?;
    handle
        .match_album(&query.mbid)
        .await
        .map(Json)
        .map_err(|error| failed(source, error))
}

/// Connection status for one source. Never carries credential material.
#[utoipa::path(get, path = "/api/v3/remotes/{source}/connection",
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn get_connection(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<ConnectionStatus>, RemotesError> {
    let source = parse_source(&source)?;
    let status = match resolve_connection(
        deps.connections.as_ref(),
        &deps.coder,
        &ctx.user_id,
        source,
    )
    .await
    {
        Ok(resolved) => ConnectionStatus {
            source,
            connected: true,
            account_mode: resolved.account_mode,
            account_label: resolved.account_label,
        },
        Err(ResolveError::Stale) => ConnectionStatus {
            source,
            connected: false,
            account_mode: "linked".to_owned(),
            account_label: "Reconnect required".to_owned(),
        },
        Err(ResolveError::NotConfigured) => ConnectionStatus {
            source,
            connected: false,
            account_mode: "linked".to_owned(),
            account_label: String::new(),
        },
    };
    Ok(Json(status))
}

/// Save one source connection. Secrets stay write-only.
#[utoipa::path(put, path = "/api/v3/remotes/{source}/connection",
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn put_connection(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
    ValidJson(payload): ValidJson<ConnectionSave>,
) -> Result<Json<ConnectionStatus>, RemotesError> {
    let source = parse_source(&source)?;
    save_connection(
        deps.connections.as_ref(),
        &deps.coder,
        &ctx.user_id,
        source,
        ConnectionDraft {
            base_url: payload.base_url,
            username: payload.username,
            credential: payload.credential,
            client_id: payload.client_id,
            user_id: payload.user_id,
            section_id: payload.section_id,
        },
    )
    .await
    .map_err(|error| match error {
        SaveError::SealFailed => RemotesError::internal(&error, deps.ids.as_ref()),
        SaveError::MissingCredential | SaveError::MissingBaseUrl => RemotesError::InvalidInput {
            message: error.to_string(),
        },
    })?;
    get_connection(
        State(deps),
        RemotesUser(ctx),
        Path(source.as_str().to_owned()),
    )
    .await
}

/// Delete one source connection.
#[utoipa::path(delete, path = "/api/v3/remotes/{source}/connection",
    responses((status = 200, description = "Connection status", body = ConnectionStatus)))]
pub async fn delete_connection(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    Path(source): Path<String>,
) -> Result<Json<ConnectionStatus>, RemotesError> {
    let source = parse_source(&source)?;
    deps.connections.delete(&ctx.user_id, source).await;
    Ok(Json(ConnectionStatus {
        source,
        connected: false,
        account_mode: "linked".to_owned(),
        account_label: String::new(),
    }))
}

/// Navidrome folder preference resolution for the caller.
#[utoipa::path(get, path = "/api/v3/remotes/navidrome/folders",
    responses((status = 200, description = "Folder resolution", body = FolderResolutionView)))]
pub async fn get_folders(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
) -> Result<Json<FolderResolutionView>, RemotesError> {
    let preference = deps.folders.get(&ctx.user_id).await;
    let resolved = resolve_connection(
        deps.connections.as_ref(),
        &deps.coder,
        &ctx.user_id,
        SourceName::Navidrome,
    )
    .await
    .map_err(|error| match error {
        ResolveError::NotConfigured => RemotesError::NotConfigured {
            message: "Navidrome is not connected".to_owned(),
        },
        ResolveError::Stale => RemotesError::AuthFailed {
            message: "The stored Navidrome credential no longer opens; reconnect it".to_owned(),
        },
    })?;
    let adapter = NavidromeAdapter::new(
        deps.http.clone(),
        resolved.base_url,
        resolved.username,
        resolved.credential,
    );
    let identity = adapter.server_identity();
    let folders = adapter.music_folders().await;
    let resolution = match &folders {
        Ok(folders) => resolve_scope(&preference, Some(folders), &identity),
        Err(error) => {
            tracing::warn!(
                error = ?error,
                "navidrome folders unavailable; rendering the degraded view"
            );
            resolve_scope(&preference, None, &identity)
        }
    };
    Ok(Json(FolderResolutionView {
        mode: resolution.scope.mode,
        folder_ids: resolution.scope.folder_ids.unwrap_or_default(),
        available_folders: resolution
            .available_folders
            .into_iter()
            .map(|(id, name)| MusicFolderView { id, name })
            .collect(),
        stale_folder_ids: resolution.stale_folder_ids,
        source_available: resolution.source_available,
    }))
}

/// Save the Navidrome folder preference.
#[utoipa::path(put, path = "/api/v3/remotes/navidrome/folders",
    responses((status = 200, description = "Folder resolution", body = FolderResolutionView)))]
pub async fn put_folders(
    State(deps): State<RemotesDeps>,
    RemotesUser(ctx): RemotesUser,
    ValidJson(payload): ValidJson<FolderSave>,
) -> Result<Json<FolderResolutionView>, RemotesError> {
    let resolved = resolve_connection(
        deps.connections.as_ref(),
        &deps.coder,
        &ctx.user_id,
        SourceName::Navidrome,
    )
    .await
    .map_err(|error| match error {
        ResolveError::NotConfigured => RemotesError::NotConfigured {
            message: "Navidrome is not connected".to_owned(),
        },
        ResolveError::Stale => RemotesError::AuthFailed {
            message: "The stored Navidrome credential no longer opens; reconnect it".to_owned(),
        },
    })?;
    let adapter = NavidromeAdapter::new(
        deps.http.clone(),
        resolved.base_url,
        resolved.username,
        resolved.credential,
    );
    let identity = adapter.server_identity();
    let folders = adapter
        .music_folders()
        .await
        .map_err(|error| failed(SourceName::Navidrome, error))?;
    let preference = checked_preference(
        &payload.mode,
        &payload.selected_folder_ids,
        &folders,
        &identity,
    )
    .map_err(|error: FolderSaveError| match error {
        FolderSaveError::DuplicateIds => RemotesError::Conflict {
            message: error.to_string(),
        },
        FolderSaveError::InvalidMode
        | FolderSaveError::AllWithIds
        | FolderSaveError::EmptySelection
        | FolderSaveError::UnknownIds => RemotesError::InvalidInput {
            message: error.to_string(),
        },
    })?;
    deps.folders.set(&ctx.user_id, preference).await;
    get_folders(State(deps), RemotesUser(ctx)).await
}
