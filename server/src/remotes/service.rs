//! The remotes service: everything behind the `/api/v3/remotes` routes.
//!
//! Handlers parse the request, call one method here, and render the
//! result; this layer resolves the caller's connection, builds the source
//! adapter, threads the Navidrome folder scope, and turns adapter failures
//! into [`RemotesFailure`]. Only the handlers map failures to HTTP.

use std::future::Future;
use std::sync::Arc;

use super::adapter::{
    AdapterError, AlbumBrowse, ArtistBrowse, ImportSink, RemoteHandle, TrackBrowse, album_page,
    artist_page, track_page,
};
use super::connections::{ConnectionResolver, LinkSummary, ResolveError, SaveError, UserLink};
use super::folders::{FolderSaveError, FolderStore, checked_preference, resolve_scope};
use super::jellyfin::{JellyfinAdapter, MixSeed};
use super::models::{
    AlbumPage, AlbumView, AnalyticsView, ArtistIndexEntry, ArtistPage, ArtistView,
    ConnectionStatus, DiscoveryView, FavoritesView, FilterFacetsView, FolderResolutionView,
    HistoryPage, HubView, ImportResult, InfoView, LyricsView, MatchView, MusicFolderView,
    PlaylistCollection, PlaylistDetail, SearchResults, SessionsView, SourceName, StatsView,
    TrackPage, TrackView,
};
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;

/// Every way a remotes call can fail. Detail strings are log-only unless
/// the variant says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemotesFailure {
    /// The admin has not configured this server, or nobody has a usable
    /// credential for it.
    NotConfigured(SourceName),
    /// The server rejected the credential, or the stored link no longer
    /// opens. The user relinks; nothing retries.
    AuthFailed(SourceName),
    /// The server answered with an error or an unusable payload.
    Upstream {
        /// Which server.
        source: SourceName,
        /// Log-only cause.
        detail: String,
    },
    /// The server did not answer at all.
    Unreachable {
        /// Which server.
        source: SourceName,
        /// Log-only cause.
        detail: String,
    },
    /// Nothing behind this id.
    NotFound,
    /// The source has no such capability. The message is user-facing.
    Unsupported(String),
    /// Bad input. The message is user-facing.
    InvalidInput(String),
    /// Valid input against the wrong state. The message is user-facing.
    Conflict(String),
    /// Our own fault (store, sealing). Log-only cause.
    Internal(String),
}

impl std::fmt::Display for RemotesFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured(source) => write!(f, "{} is not connected", source.display()),
            Self::AuthFailed(source) => write!(f, "{} rejected the credential", source.display()),
            Self::Upstream { source, detail } => {
                write!(f, "{} answered with an error: {detail}", source.display())
            }
            Self::Unreachable { source, detail } => {
                write!(f, "{} is unreachable: {detail}", source.display())
            }
            Self::NotFound => f.write_str("remote item not found"),
            Self::Unsupported(message)
            | Self::InvalidInput(message)
            | Self::Conflict(message)
            | Self::Internal(message) => f.write_str(message),
        }
    }
}

/// Result alias for the service.
pub type RemotesResult<T> = Result<T, RemotesFailure>;

/// Map an adapter failure for one source.
pub fn adapter_failure(source: SourceName, error: AdapterError) -> RemotesFailure {
    match error {
        AdapterError::NotConfigured => RemotesFailure::NotConfigured(source),
        AdapterError::Auth => RemotesFailure::AuthFailed(source),
        AdapterError::Api(detail) => RemotesFailure::Upstream { source, detail },
        AdapterError::Transport(detail) => RemotesFailure::Unreachable { source, detail },
        AdapterError::NotFound => RemotesFailure::NotFound,
        AdapterError::Unsupported(message) => RemotesFailure::Unsupported(message),
    }
}

fn resolve_failure(source: SourceName, error: ResolveError) -> RemotesFailure {
    match error {
        ResolveError::NotConfigured => RemotesFailure::NotConfigured(source),
        ResolveError::Stale => RemotesFailure::AuthFailed(source),
        ResolveError::Store(cause) => RemotesFailure::Internal(cause),
    }
}

fn save_failure(error: SaveError) -> RemotesFailure {
    RemotesFailure::Internal(error.to_string())
}

/// What a user sends to link their own Navidrome or Jellyfin account.
#[derive(Clone)]
pub struct AccountLogin {
    /// Login name on the server.
    pub username: String,
    /// Password. Navidrome keeps it sealed; Jellyfin trades it for a token
    /// and drops it.
    pub password: String,
}

/// Plex listening analytics over the history feed.
pub const ANALYTICS_MAX_ENTRIES: i64 = 5_000;
const ANALYTICS_BATCH: i64 = 500;

/// The remotes service. Cheap to clone.
#[derive(Clone)]
pub struct RemotesService {
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    folders: Arc<dyn FolderStore>,
    imports: Arc<dyn ImportSink>,
}

impl RemotesService {
    /// Build the service over the shared HTTP client, the connection
    /// resolver, the folder preferences, and the playlist import target.
    pub fn new(
        http: reqwest::Client,
        resolver: Arc<ConnectionResolver>,
        folders: Arc<dyn FolderStore>,
        imports: Arc<dyn ImportSink>,
    ) -> Self {
        Self {
            http,
            resolver,
            folders,
            imports,
        }
    }

    /// The resolver behind this service.
    pub fn resolver(&self) -> &Arc<ConnectionResolver> {
        &self.resolver
    }

    // -- handles -----------------------------------------------------------

    /// The caller's handle for one source, folder-scoped for Navidrome.
    pub async fn handle(&self, user_id: &str, source: SourceName) -> RemotesResult<RemoteHandle> {
        let resolved = self
            .resolver
            .resolve(user_id, source)
            .await
            .map_err(|error| resolve_failure(source, error))?;
        Ok(match source {
            SourceName::Jellyfin => RemoteHandle::Jellyfin(JellyfinAdapter::new(
                self.http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.user_id,
            )),
            SourceName::Navidrome => {
                let adapter = NavidromeAdapter::new(
                    self.http.clone(),
                    resolved.base_url,
                    resolved.username,
                    resolved.credential,
                );
                let preference = self
                    .folders
                    .get(user_id)
                    .await
                    .map_err(RemotesFailure::Internal)?;
                let identity = adapter.server_identity();
                let folders = match adapter.music_folders().await {
                    Ok(folders) => Some(folders),
                    Err(AdapterError::Auth) => {
                        return Err(RemotesFailure::AuthFailed(SourceName::Navidrome));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "navidrome folders unavailable; using the stored scope");
                        None
                    }
                };
                let resolution = resolve_scope(&preference, folders.as_deref(), &identity);
                RemoteHandle::Navidrome(adapter.with_folders(resolution.scope.folder_ids))
            }
            SourceName::Plex => RemoteHandle::Plex(PlexAdapter::new(
                self.http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.client_id,
                resolved.section_ids,
            )),
        })
    }

    /// Resolve a handle and run one adapter call on it.
    async fn call<T, F, Fut>(&self, user_id: &str, source: SourceName, call: F) -> RemotesResult<T>
    where
        F: FnOnce(RemoteHandle) -> Fut,
        Fut: Future<Output = Result<T, AdapterError>>,
    {
        let handle = self.handle(user_id, source).await?;
        call(handle)
            .await
            .map_err(|error| adapter_failure(source, error))
    }

    async fn jellyfin(&self, user_id: &str) -> RemotesResult<JellyfinAdapter> {
        match self.handle(user_id, SourceName::Jellyfin).await? {
            RemoteHandle::Jellyfin(adapter) => Ok(adapter),
            _ => Err(RemotesFailure::Internal(
                "jellyfin resolved another source".to_owned(),
            )),
        }
    }

    async fn plex(&self, user_id: &str) -> RemotesResult<PlexAdapter> {
        match self.handle(user_id, SourceName::Plex).await? {
            RemoteHandle::Plex(adapter) => Ok(adapter),
            _ => Err(RemotesFailure::Internal(
                "plex resolved another source".to_owned(),
            )),
        }
    }

    fn only(source: SourceName, wanted: SourceName, what: &str) -> RemotesResult<()> {
        if source == wanted {
            Ok(())
        } else {
            Err(RemotesFailure::Unsupported(format!(
                "{} has no {what}",
                source.display()
            )))
        }
    }

    // -- browse ------------------------------------------------------------

    /// Hub highlights.
    pub async fn hub(&self, user_id: &str, source: SourceName) -> RemotesResult<HubView> {
        self.call(user_id, source, |handle| async move { handle.hub().await })
            .await
    }

    /// Library totals.
    pub async fn stats(&self, user_id: &str, source: SourceName) -> RemotesResult<StatsView> {
        self.call(
            user_id,
            source,
            |handle| async move { handle.stats().await },
        )
        .await
    }

    /// One page of albums.
    pub async fn albums(
        &self,
        user_id: &str,
        source: SourceName,
        browse: AlbumBrowse,
    ) -> RemotesResult<AlbumPage> {
        let (offset, limit) = (browse.offset, browse.limit);
        let page = self
            .call(user_id, source, move |handle| async move {
                handle.albums(&browse).await
            })
            .await?;
        Ok(album_page(page, offset, limit))
    }

    /// One album.
    pub async fn album(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<AlbumView> {
        self.call(user_id, source, move |handle| async move {
            handle.album_detail(&id).await
        })
        .await?
        .ok_or(RemotesFailure::NotFound)
    }

    /// Tracks of one album.
    pub async fn album_tracks(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<TrackPage> {
        let items = self
            .call(user_id, source, move |handle| async move {
                handle.album_tracks(&id).await
            })
            .await?;
        Ok(whole_page(items))
    }

    /// One page of artists.
    pub async fn artists(
        &self,
        user_id: &str,
        source: SourceName,
        browse: ArtistBrowse,
    ) -> RemotesResult<ArtistPage> {
        let (offset, limit) = (browse.offset, browse.limit);
        let page = self
            .call(user_id, source, move |handle| async move {
                handle.artists(&browse).await
            })
            .await?;
        Ok(artist_page(page, offset, limit))
    }

    /// The alphabetic artist index.
    pub async fn artist_index(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<Vec<ArtistIndexEntry>> {
        self.call(user_id, source, |handle| async move {
            handle.artist_index().await
        })
        .await
    }

    /// One artist.
    pub async fn artist(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<ArtistView> {
        self.call(user_id, source, move |handle| async move {
            handle.artist_detail(&id).await
        })
        .await?
        .ok_or(RemotesFailure::NotFound)
    }

    /// One page of tracks.
    pub async fn tracks(
        &self,
        user_id: &str,
        source: SourceName,
        browse: TrackBrowse,
    ) -> RemotesResult<TrackPage> {
        let (offset, limit) = (browse.offset, browse.limit);
        let page = self
            .call(user_id, source, move |handle| async move {
                handle.tracks(&browse).await
            })
            .await?;
        Ok(track_page(page, offset, limit))
    }

    /// Search across artists, albums, and tracks.
    pub async fn search(
        &self,
        user_id: &str,
        source: SourceName,
        query: String,
        limit: i64,
    ) -> RemotesResult<SearchResults> {
        if query.trim().is_empty() {
            return Err(RemotesFailure::InvalidInput(
                "Search query must not be empty".to_owned(),
            ));
        }
        self.call(user_id, source, move |handle| async move {
            handle.search(&query, limit).await
        })
        .await
    }

    /// Recently played albums.
    pub async fn recent(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<Vec<AlbumView>> {
        self.call(user_id, source, move |handle| async move {
            handle.recent(limit).await
        })
        .await
    }

    /// Recently added albums.
    pub async fn recently_added(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<Vec<AlbumView>> {
        self.call(user_id, source, move |handle| async move {
            handle.recently_added(limit).await
        })
        .await
    }

    /// Favorite artists, albums, and tracks.
    pub async fn favorites(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<FavoritesView> {
        self.call(user_id, source, move |handle| async move {
            handle.favorites(limit).await
        })
        .await
    }

    /// Genre labels.
    pub async fn genres(&self, user_id: &str, source: SourceName) -> RemotesResult<Vec<String>> {
        self.call(
            user_id,
            source,
            |handle| async move { handle.genres().await },
        )
        .await
    }

    /// Tracks carrying one genre.
    pub async fn genre_songs(
        &self,
        user_id: &str,
        source: SourceName,
        genre: String,
        limit: i64,
        offset: i64,
    ) -> RemotesResult<TrackPage> {
        let items = self
            .call(user_id, source, move |handle| async move {
                handle.genre_songs(&genre, limit, offset).await
            })
            .await?;
        let total = offset + items.len() as i64;
        Ok(TrackPage {
            items,
            total,
            offset,
            limit,
        })
    }

    /// Plex mood labels.
    pub async fn moods(&self, user_id: &str, source: SourceName) -> RemotesResult<Vec<String>> {
        Self::only(source, SourceName::Plex, "mood labels")?;
        self.plex(user_id)
            .await?
            .moods()
            .await
            .map_err(|error| adapter_failure(source, error))
    }

    /// Jellyfin filter facets (years, tags, studios).
    pub async fn filters(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<FilterFacetsView> {
        Self::only(source, SourceName::Jellyfin, "filter facets")?;
        self.jellyfin(user_id)
            .await?
            .filter_facets()
            .await
            .map_err(|error| adapter_failure(source, error))
    }

    /// Most-played albums (Jellyfin play counts).
    pub async fn most_played_albums(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<Vec<AlbumView>> {
        Self::only(source, SourceName::Jellyfin, "play counts")?;
        self.jellyfin(user_id)
            .await?
            .most_played_albums(limit)
            .await
            .map_err(|error| adapter_failure(source, error))
    }

    /// Most-played artists (Jellyfin play counts).
    pub async fn most_played_artists(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<Vec<ArtistView>> {
        Self::only(source, SourceName::Jellyfin, "play counts")?;
        self.jellyfin(user_id)
            .await?
            .most_played_artists(limit)
            .await
            .map_err(|error| adapter_failure(source, error))
    }

    // -- playlists ---------------------------------------------------------

    /// The caller's playlists on the server.
    pub async fn playlists(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<PlaylistCollection> {
        let items = self
            .call(
                user_id,
                source,
                |handle| async move { handle.playlists().await },
            )
            .await?;
        Ok(PlaylistCollection { items })
    }

    /// One playlist with its tracks.
    pub async fn playlist(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<PlaylistDetail> {
        self.call(user_id, source, move |handle| async move {
            handle.playlist_detail(&id).await
        })
        .await?
        .ok_or(RemotesFailure::NotFound)
    }

    /// Import one remote playlist into the caller's playlists.
    pub async fn import_playlist(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<ImportResult> {
        let detail = self.playlist(user_id, source, id.clone()).await?;
        let receipt = self
            .imports
            .import(user_id, source, &id, &detail.playlist.name, detail.tracks)
            .await;
        Ok(ImportResult::from(receipt))
    }

    // -- info and extras ---------------------------------------------------

    /// Artist info passthrough.
    pub async fn artist_info(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<InfoView> {
        self.call(user_id, source, move |handle| async move {
            handle.artist_info(&id).await
        })
        .await
    }

    /// Album info passthrough.
    pub async fn album_info(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
    ) -> RemotesResult<InfoView> {
        self.call(user_id, source, move |handle| async move {
            handle.album_info(&id).await
        })
        .await
    }

    /// Lyrics for one track.
    pub async fn lyrics(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
        artist: Option<String>,
        title: Option<String>,
    ) -> RemotesResult<LyricsView> {
        self.call(user_id, source, move |handle| async move {
            handle
                .lyrics(&id, artist.as_deref(), title.as_deref())
                .await
        })
        .await?
        .ok_or(RemotesFailure::NotFound)
    }

    /// Top songs for one artist name.
    pub async fn top_songs(
        &self,
        user_id: &str,
        source: SourceName,
        artist: String,
        limit: i64,
    ) -> RemotesResult<TrackPage> {
        let items = self
            .call(user_id, source, move |handle| async move {
                handle.top_songs(&artist, limit).await
            })
            .await?;
        Ok(capped_page(items, limit))
    }

    /// Tracks similar to one track.
    pub async fn similar(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
        limit: i64,
    ) -> RemotesResult<TrackPage> {
        let items = self
            .call(user_id, source, move |handle| async move {
                handle.similar(&id, limit).await
            })
            .await?;
        Ok(capped_page(items, limit))
    }

    /// Random tracks.
    pub async fn random(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
        genre: String,
    ) -> RemotesResult<TrackPage> {
        let items = self
            .call(user_id, source, move |handle| async move {
                handle.random(limit, &genre).await
            })
            .await?;
        Ok(capped_page(items, limit))
    }

    /// Plex discovery shelves.
    pub async fn discovery(
        &self,
        user_id: &str,
        source: SourceName,
        count: i64,
    ) -> RemotesResult<DiscoveryView> {
        Self::only(source, SourceName::Plex, "discovery shelves")?;
        self.plex(user_id)
            .await?
            .discovery(count)
            .await
            .map_err(|error| adapter_failure(source, error))
    }

    /// Jellyfin instant mix.
    pub async fn mix(
        &self,
        user_id: &str,
        source: SourceName,
        seed: MixSeed,
        limit: i64,
    ) -> RemotesResult<TrackPage> {
        Self::only(source, SourceName::Jellyfin, "instant-mix endpoint")?;
        let items = self
            .jellyfin(user_id)
            .await?
            .mix(&seed, limit)
            .await
            .map_err(|error| adapter_failure(source, error))?;
        Ok(capped_page(items, limit))
    }

    /// Active audio sessions.
    pub async fn sessions(&self, user_id: &str, source: SourceName) -> RemotesResult<SessionsView> {
        self.call(
            user_id,
            source,
            |handle| async move { handle.sessions().await },
        )
        .await
    }

    /// Listening history, newest first.
    pub async fn history(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
        offset: i64,
    ) -> RemotesResult<HistoryPage> {
        self.call(user_id, source, move |handle| async move {
            handle.history(limit, offset).await
        })
        .await
    }

    /// Plex listening analytics: top artists, albums, and tracks over the
    /// most recent history (v2 reads up to 5,000 entries, 500 per call).
    pub async fn analytics(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<AnalyticsView> {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0);
        Self::only(source, SourceName::Plex, "listening analytics")?;
        let plex = self.plex(user_id).await?;
        let mut entries = Vec::new();
        let mut offset = 0;
        let mut total_available = 0;
        while (entries.len() as i64) < ANALYTICS_MAX_ENTRIES {
            let page = plex
                .history(ANALYTICS_BATCH, offset)
                .await
                .map_err(|error| adapter_failure(source, error))?;
            if page.total > 0 {
                total_available = page.total;
            }
            if page.items.is_empty() {
                break;
            }
            entries.extend(page.items);
            offset += ANALYTICS_BATCH;
            if offset >= page.total {
                break;
            }
        }
        Ok(super::analytics::summarize(
            &entries,
            total_available,
            now_unix,
        ))
    }

    /// Image bytes for one item.
    pub async fn image(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
        size: i64,
    ) -> RemotesResult<(Vec<u8>, String)> {
        self.call(user_id, source, move |handle| async move {
            handle.image_bytes(&id, size).await
        })
        .await
    }

    /// Playlist cover bytes.
    pub async fn playlist_cover(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
        size: i64,
    ) -> RemotesResult<(Vec<u8>, String)> {
        self.call(user_id, source, move |handle| async move {
            handle.playlist_cover_bytes(&id, size).await
        })
        .await
    }

    /// The remote album behind a MusicBrainz id.
    pub async fn match_album(
        &self,
        user_id: &str,
        source: SourceName,
        mbid: String,
    ) -> RemotesResult<MatchView> {
        if mbid.trim().is_empty() {
            return Err(RemotesFailure::InvalidInput(
                "Match mbid must not be empty".to_owned(),
            ));
        }
        self.call(user_id, source, move |handle| async move {
            handle.match_album(&mbid).await
        })
        .await
    }

    // -- connections -------------------------------------------------------

    /// Connection status for one source. Never carries credential material.
    pub async fn connection(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<ConnectionStatus> {
        // Browsing falls back to the shared account past a stale link, but
        // the status still asks the user to relink their own.
        let mut resolved = self.resolver.resolve(user_id, source).await;
        if let Ok(shared) = &resolved
            && shared.account_mode == "shared"
            && let Ok(Some(server)) = self.resolver.server(source)
            && matches!(
                self.resolver.usable_link(user_id, &server, source).await,
                Err(ResolveError::Stale)
            )
        {
            resolved = Err(ResolveError::Stale);
        }
        Ok(match resolved {
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
            Err(ResolveError::Store(cause)) => return Err(RemotesFailure::Internal(cause)),
        })
    }

    /// Link the caller's own Navidrome or Jellyfin account on the admin's
    /// server. The credentials are checked live first; Navidrome keeps the
    /// password sealed, Jellyfin trades it for a user token. Plex links
    /// through its sign-in flow instead.
    pub async fn connect(
        &self,
        user_id: &str,
        source: SourceName,
        login: AccountLogin,
    ) -> RemotesResult<ConnectionStatus> {
        let server = self
            .resolver
            .server(source)
            .map_err(|error| resolve_failure(source, error))?
            .ok_or_else(|| {
                RemotesFailure::InvalidInput(format!(
                    "{} is not configured by the administrator",
                    source.display()
                ))
            })?;
        if login.username.trim().is_empty() || login.password.is_empty() {
            return Err(RemotesFailure::InvalidInput(format!(
                "A {} username and password are required",
                source.display()
            )));
        }
        let link = match source {
            SourceName::Navidrome => {
                let adapter = NavidromeAdapter::new(
                    self.http.clone(),
                    server.base_url.clone(),
                    login.username.clone(),
                    login.password.clone(),
                );
                adapter.validate_connection().await.map_err(|error| {
                    rejected_login(SourceName::Navidrome, error, "username or password")
                })?;
                UserLink::Navidrome {
                    username: login.username,
                    password: login.password,
                }
            }
            SourceName::Jellyfin => {
                let session = JellyfinAdapter::authenticate_by_name(
                    &self.http,
                    &server.base_url,
                    &server.client_id,
                    &login.username,
                    &login.password,
                )
                .await
                .map_err(|error| {
                    rejected_login(SourceName::Jellyfin, error, "username or password")
                })?;
                UserLink::Jellyfin {
                    access_token: session.access_token,
                    jellyfin_user_id: session.user_id,
                    username: session.user_name,
                }
            }
            SourceName::Plex => {
                return Err(RemotesFailure::Unsupported(
                    "Plex accounts link through the Plex sign-in flow".to_owned(),
                ));
            }
        };
        self.resolver
            .save_link(user_id, &link)
            .await
            .map_err(save_failure)?;
        self.connection(user_id, source).await
    }

    /// Remove the caller's own link. The shared admin account, when there
    /// is one, keeps the server usable.
    pub async fn disconnect(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<ConnectionStatus> {
        self.resolver
            .delete_link(user_id, source.as_str())
            .await
            .map_err(save_failure)?;
        self.connection(user_id, source).await
    }

    /// Every account the caller linked, across services.
    pub async fn links(&self, user_id: &str) -> RemotesResult<Vec<LinkSummary>> {
        self.resolver
            .list_links(user_id)
            .await
            .map_err(|error| RemotesFailure::Internal(error.to_string()))
    }

    // -- navidrome folders -------------------------------------------------

    /// The caller's Navidrome folder preference, resolved against the
    /// folders the server exposes now.
    pub async fn folders(&self, user_id: &str) -> RemotesResult<FolderResolutionView> {
        let preference = self
            .folders
            .get(user_id)
            .await
            .map_err(RemotesFailure::Internal)?;
        let adapter = self.navidrome_unscoped(user_id).await?;
        let identity = adapter.server_identity();
        let resolution = match adapter.music_folders().await {
            Ok(folders) => resolve_scope(&preference, Some(&folders), &identity),
            Err(error) => {
                tracing::warn!(%error, "navidrome folders unavailable; rendering the degraded view");
                resolve_scope(&preference, None, &identity)
            }
        };
        Ok(FolderResolutionView {
            mode: resolution.scope.mode,
            folder_ids: resolution.scope.folder_ids.unwrap_or_default(),
            available_folders: resolution
                .available_folders
                .into_iter()
                .map(|(id, name)| MusicFolderView { id, name })
                .collect(),
            stale_folder_ids: resolution.stale_folder_ids,
            source_available: resolution.source_available,
        })
    }

    /// Save the caller's Navidrome folder preference.
    pub async fn save_folders(
        &self,
        user_id: &str,
        mode: &str,
        selected_folder_ids: &[String],
    ) -> RemotesResult<FolderResolutionView> {
        let adapter = self.navidrome_unscoped(user_id).await?;
        let identity = adapter.server_identity();
        let folders = adapter
            .music_folders()
            .await
            .map_err(|error| adapter_failure(SourceName::Navidrome, error))?;
        let preference = checked_preference(mode, selected_folder_ids, &folders, &identity)
            .map_err(|error| match error {
                FolderSaveError::DuplicateIds => RemotesFailure::Conflict(error.to_string()),
                FolderSaveError::InvalidMode
                | FolderSaveError::AllWithIds
                | FolderSaveError::EmptySelection
                | FolderSaveError::UnknownIds => RemotesFailure::InvalidInput(error.to_string()),
            })?;
        self.folders
            .set(user_id, preference)
            .await
            .map_err(RemotesFailure::Internal)?;
        self.folders(user_id).await
    }

    async fn navidrome_unscoped(&self, user_id: &str) -> RemotesResult<NavidromeAdapter> {
        let resolved = self
            .resolver
            .resolve(user_id, SourceName::Navidrome)
            .await
            .map_err(|error| resolve_failure(SourceName::Navidrome, error))?;
        Ok(NavidromeAdapter::new(
            self.http.clone(),
            resolved.base_url,
            resolved.username,
            resolved.credential,
        ))
    }
}

/// A live credential check that failed: a rejection is the user's input,
/// anything else is the server.
fn rejected_login(source: SourceName, error: AdapterError, what: &str) -> RemotesFailure {
    match error {
        AdapterError::Auth => {
            RemotesFailure::InvalidInput(format!("Invalid {} {what}", source.display()))
        }
        other => adapter_failure(source, other),
    }
}

fn whole_page(items: Vec<TrackView>) -> TrackPage {
    let total = items.len() as i64;
    TrackPage {
        items,
        total,
        offset: 0,
        limit: total,
    }
}

fn capped_page(items: Vec<TrackView>, limit: i64) -> TrackPage {
    let total = items.len() as i64;
    TrackPage {
        items,
        total,
        offset: 0,
        limit,
    }
}
