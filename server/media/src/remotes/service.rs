//! The remotes service: everything behind the `/api/v3/remotes` routes.
//!
//! Handlers parse the request, call one method here, and render the
//! result; this layer resolves the caller's connection, builds the source
//! adapter, threads the Navidrome folder scope, and turns adapter failures
//! into [`RemotesFailure`]. Only the handlers map failures to HTTP.

use std::future::Future;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;

use super::adapter::{
    AdapterError, AlbumBrowse, ArtistBrowse, ImportSink, RemoteHandle, TrackBrowse, album_page,
    artist_page, track_page, unsupported,
};
use super::connections::{
    ConnectionResolver, LinkSummary, Reads, ResolveError, ResolvedConnection, SaveError, UserLink,
};
use super::folders::{
    FolderListCache, FolderSaveError, FolderStore, checked_preference, resolve_scope,
};
use super::jellyfin::{JellyfinAdapter, MixSeed};
use super::models::{
    AlbumPage, AlbumView, AnalyticsView, ArtistIndexEntry, ArtistPage, ArtistView,
    ConnectionStatus, DiscoveryView, FavoritesView, FilterFacetsView, FolderResolutionView,
    HistoryPage, HubView, ImportResult, InfoView, LyricsView, MatchQuery, MatchView,
    MusicFolderView, PlaylistCollection, PlaylistDetail, SearchResults, SessionsView, SourceName,
    StatsView, TrackPage, TrackView,
};
use super::warmup::MbidIndex;
use droppedneedle_kernel::db::fold::fold_text;

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

/// Albums one name search reads, as in v2.
const NAME_SEARCH_LIMIT: i64 = 50;
/// The folded compilation credit the name step never matches on: a title
/// like "Greatest Hits" under it names no one album.
const VARIOUS_ARTISTS: &str = "various artists";

/// A query value with something in it.
fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// The album's tracks, when it has any.
async fn playable(
    handle: &RemoteHandle,
    id: String,
) -> Result<Option<(String, Vec<TrackView>)>, AdapterError> {
    let tracks = handle.album_tracks(&id).await?;
    Ok((!tracks.is_empty()).then_some((id, tracks)))
}

/// Plex listening analytics over the history feed.
pub const ANALYTICS_MAX_ENTRIES: i64 = 5_000;
const ANALYTICS_BATCH: i64 = 500;
/// Most genres one genre-songs read merges, as in v2.
const MAX_GENRES: usize = 10;

/// The remotes service. Cheap to clone.
#[derive(Clone)]
pub struct RemotesService {
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    folders: Arc<dyn FolderStore>,
    folder_lists: Arc<FolderListCache>,
    imports: Arc<dyn ImportSink>,
    index: MbidIndex,
    clock: Arc<dyn Clock>,
}

impl RemotesService {
    /// Build the service over the shared HTTP client, the connection
    /// resolver, the folder preferences, the playlist import target, the
    /// MBID index the warmups fill and the clock listening analytics count
    /// back from.
    pub fn new(
        http: reqwest::Client,
        resolver: Arc<ConnectionResolver>,
        folders: Arc<dyn FolderStore>,
        imports: Arc<dyn ImportSink>,
        index: MbidIndex,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            http,
            resolver,
            folders,
            folder_lists: Arc::default(),
            imports,
            index,
            clock,
        }
    }

    /// The resolver behind this service.
    pub fn resolver(&self) -> &Arc<ConnectionResolver> {
        &self.resolver
    }

    /// The MBID index the match reads.
    pub fn index(&self) -> &MbidIndex {
        &self.index
    }

    // -- handles -----------------------------------------------------------

    /// The caller's handle for one source, folder-scoped for Navidrome.
    /// The folder list comes from the cache while it is fresh.
    pub async fn handle(&self, user_id: &str, source: SourceName) -> RemotesResult<RemoteHandle> {
        let handle = self.unscoped(user_id, source, Reads::User).await?;
        let RemoteHandle::Navidrome(adapter) = handle else {
            return Ok(handle);
        };
        let preference = self
            .folders
            .get(user_id)
            .await
            .map_err(RemotesFailure::Internal)?;
        let identity = adapter.server_identity();
        let cached = self
            .folder_lists
            .get(user_id, &identity, self.clock.now_ms());
        let fetched = match cached {
            Some(folders) => Ok(folders),
            None => adapter.music_folders().await.inspect(|folders| {
                self.folder_lists
                    .put(user_id, &identity, self.clock.now_ms(), folders.clone());
            }),
        };
        let folders = match fetched {
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
        Ok(RemoteHandle::Navidrome(
            adapter.with_folders(resolution.scope.folder_ids),
        ))
    }

    /// The caller's handle for one source, with no folder scope.
    async fn unscoped(
        &self,
        user_id: &str,
        source: SourceName,
        reads: Reads,
    ) -> RemotesResult<RemoteHandle> {
        let resolved = self
            .resolver
            .resolve(user_id, source)
            .await
            .map_err(|error| resolve_failure(source, error))?;
        Ok(self.resolver.client(&self.http, resolved, reads))
    }

    /// Run one adapter call that addresses an item by id. It skips the
    /// folder scope, as the stream reader does: the scope is a view filter
    /// for browse lists, and the id came from one of them.
    async fn call_by_id<T, F, Fut>(
        &self,
        user_id: &str,
        source: SourceName,
        reads: Reads,
        call: F,
    ) -> RemotesResult<T>
    where
        F: FnOnce(RemoteHandle) -> Fut,
        Fut: Future<Output = Result<T, AdapterError>>,
    {
        let handle = self.unscoped(user_id, source, reads).await?;
        call(handle)
            .await
            .map_err(|error| adapter_failure(source, error))
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

    /// [`Self::call`] for `what` only `wanted` has. Any other source
    /// answers `Unsupported` before anything is resolved.
    async fn call_only<T, F, Fut>(
        &self,
        user_id: &str,
        source: SourceName,
        wanted: SourceName,
        what: &str,
        call: F,
    ) -> RemotesResult<T>
    where
        F: FnOnce(RemoteHandle) -> Fut,
        Fut: Future<Output = Result<T, AdapterError>>,
    {
        if source != wanted {
            return Err(adapter_failure(source, unsupported(source, what)));
        }
        self.call(user_id, source, call).await
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
        self.call_by_id(user_id, source, Reads::User, move |handle| async move {
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
            .call_by_id(user_id, source, Reads::User, move |handle| async move {
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
        mut genres: Vec<String>,
        limit: i64,
        offset: i64,
    ) -> RemotesResult<TrackPage> {
        if genres.is_empty() {
            return Err(RemotesFailure::InvalidInput(
                "Name at least one genre.".to_owned(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        genres.retain(|genre| seen.insert(genre.to_lowercase()));
        genres.truncate(MAX_GENRES);
        let items = self
            .call(user_id, source, move |handle| async move {
                handle.genres_songs(&genres, limit, offset).await
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
        let what = "mood labels";
        self.call_only(
            user_id,
            source,
            SourceName::Plex,
            what,
            |handle| async move { handle.plex(what)?.moods().await },
        )
        .await
    }

    /// Jellyfin filter facets (years, tags, studios).
    pub async fn filters(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> RemotesResult<FilterFacetsView> {
        let what = "filter facets";
        self.call_only(
            user_id,
            source,
            SourceName::Jellyfin,
            what,
            |handle| async move { handle.jellyfin(what)?.filter_facets().await },
        )
        .await
    }

    /// Most-played albums (Jellyfin play counts).
    pub async fn most_played_albums(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<Vec<AlbumView>> {
        let what = "play counts";
        self.call_only(
            user_id,
            source,
            SourceName::Jellyfin,
            what,
            |handle| async move { handle.jellyfin(what)?.most_played_albums(limit).await },
        )
        .await
    }

    /// Most-played artists (Jellyfin play counts).
    pub async fn most_played_artists(
        &self,
        user_id: &str,
        source: SourceName,
        limit: i64,
    ) -> RemotesResult<Vec<ArtistView>> {
        let what = "play counts";
        self.call_only(
            user_id,
            source,
            SourceName::Jellyfin,
            what,
            |handle| async move { handle.jellyfin(what)?.most_played_artists(limit).await },
        )
        .await
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
        // The imported flag is a nicety: a failed read leaves it unset.
        let imported = match self.imports.imported_ids(user_id, source).await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, "imported playlist ids unavailable; flags left unset");
                Default::default()
            }
        };
        let items = items
            .into_iter()
            .map(|mut playlist| {
                playlist.is_imported = imported.contains(&playlist.id);
                playlist
            })
            .collect();
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
            .await
            .map_err(RemotesFailure::Internal)?;
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
        let what = "discovery shelves";
        self.call_only(
            user_id,
            source,
            SourceName::Plex,
            what,
            |handle| async move { handle.plex(what)?.discovery(count).await },
        )
        .await
    }

    /// Jellyfin instant mix.
    pub async fn mix(
        &self,
        user_id: &str,
        source: SourceName,
        seed: MixSeed,
        limit: i64,
    ) -> RemotesResult<TrackPage> {
        let what = "instant-mix endpoint";
        let items = self
            .call_only(
                user_id,
                source,
                SourceName::Jellyfin,
                what,
                |handle| async move { handle.jellyfin(what)?.mix(&seed, limit).await },
            )
            .await?;
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
        let now_unix = self.clock.now_unix();
        let what = "listening analytics";
        if source != SourceName::Plex {
            return Err(adapter_failure(source, unsupported(source, what)));
        }
        let handle = self.handle(user_id, source).await?;
        let plex = handle
            .plex(what)
            .map_err(|error| adapter_failure(source, error))?;
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

    /// Image bytes for one item. Images stay outside the per-source read
    /// bound: a grid asks for dozens at once, and a click must not queue
    /// behind them. Each is read under the image size cap.
    pub async fn image(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
        size: i64,
    ) -> RemotesResult<(Vec<u8>, String)> {
        self.call_by_id(
            user_id,
            source,
            Reads::Background,
            move |handle| async move { handle.image_bytes(&id, size).await },
        )
        .await
    }

    /// Playlist cover bytes, outside the read bound as images are.
    pub async fn playlist_cover(
        &self,
        user_id: &str,
        source: SourceName,
        id: String,
        size: i64,
    ) -> RemotesResult<(Vec<u8>, String)> {
        self.call_by_id(
            user_id,
            source,
            Reads::Background,
            move |handle| async move { handle.playlist_cover_bytes(&id, size).await },
        )
        .await
    }

    /// The caller's copy of an album on one source.
    ///
    /// A remote album matches when it carries the album's release or
    /// release-group id, from the index or the server's MBID search, taking
    /// the first in the server's order. Otherwise, on Plex and Navidrome
    /// only, it matches by name: when the query has a title and an artist
    /// that is not Various Artists, and exactly one album in the title
    /// search's answer has that title and album artist once folded. Two or
    /// more mean no match, since a wrong match plays the wrong album. Every
    /// candidate counts only when the caller's own connection can open it.
    pub async fn match_album(
        &self,
        user_id: &str,
        source: SourceName,
        query: MatchQuery,
    ) -> RemotesResult<MatchView> {
        let mbid = query.mbid.trim();
        if mbid.is_empty() {
            return Err(RemotesFailure::InvalidInput(
                "Match mbid must not be empty".to_owned(),
            ));
        }
        let handle = self.handle(user_id, source).await?;
        // The index is used only for the server it was read from: after the
        // admin points the source elsewhere, its ids name other albums.
        let server = self
            .resolver
            .server(source)
            .map_err(|error| resolve_failure(source, error))?
            .map(|server| server.base_url)
            .unwrap_or_default();
        let found = self
            .find_album(&handle, source, &server, mbid, &query)
            .await
            .map_err(|error| adapter_failure(source, error))?;
        Ok(match found {
            Some((id, tracks)) => MatchView {
                source,
                found: true,
                remote_album_id: Some(id),
                tracks,
            },
            None => MatchView {
                source,
                found: false,
                remote_album_id: None,
                tracks: Vec::new(),
            },
        })
    }

    async fn find_album(
        &self,
        handle: &RemoteHandle,
        source: SourceName,
        server: &str,
        mbid: &str,
        query: &MatchQuery,
    ) -> Result<Option<(String, Vec<TrackView>)>, AdapterError> {
        for album in self.index.lookup(source, server, mbid) {
            if !handle.holds(album.scope.as_deref()).await? {
                continue;
            }
            match handle.album_tracks(&album.id).await {
                Ok(tracks) if !tracks.is_empty() => return Ok(Some((album.id, tracks))),
                // The server no longer has it: the next pass drops it. Any
                // other failure stops the match, so an outage never falls
                // through to a guess by name.
                Ok(_) | Err(AdapterError::NotFound) => {
                    tracing::debug!(source = source.as_str(), "indexed album is gone");
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(id) = handle.mbid_search(mbid).await?
            && let Some(found) = playable(handle, id).await?
        {
            return Ok(Some(found));
        }
        if source == SourceName::Jellyfin {
            return Ok(None);
        }
        let (Some(title), Some(artist)) = (
            present(query.title.as_deref()),
            present(query.artist.as_deref()),
        ) else {
            return Ok(None);
        };
        let (title_key, artist_key) = (fold_text(title), fold_text(artist));
        if artist_key == VARIOUS_ARTISTS {
            return Ok(None);
        }
        let results = handle.search(title, NAME_SEARCH_LIMIT).await?;
        let mut named = results.albums.into_iter().filter(|album| {
            fold_text(&album.title) == title_key && fold_text(&album.artist_name) == artist_key
        });
        match (named.next(), named.next()) {
            (Some(album), None) => playable(handle, album.id).await,
            _ => Ok(None),
        }
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
                let handle = self.resolver.client(
                    &self.http,
                    ResolvedConnection {
                        username: login.username.clone(),
                        ..ResolvedConnection::unsaved(
                            SourceName::Navidrome,
                            server.base_url.clone(),
                            login.password.clone(),
                        )
                    },
                    Reads::User,
                );
                handle.validate_connection().await.map_err(|error| {
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
        let handle = self
            .unscoped(user_id, SourceName::Navidrome, Reads::User)
            .await?;
        let adapter = handle
            .navidrome("music folders")
            .map_err(|error| adapter_failure(SourceName::Navidrome, error))?;
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
        let handle = self
            .unscoped(user_id, SourceName::Navidrome, Reads::User)
            .await?;
        let adapter = handle
            .navidrome("music folders")
            .map_err(|error| adapter_failure(SourceName::Navidrome, error))?;
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
        self.folder_lists.forget(user_id);
        self.folders(user_id).await
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
