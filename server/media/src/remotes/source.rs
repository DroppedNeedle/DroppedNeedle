//! The unified remote-source surface.
//!
//! One handle, three sources. Every browse concept (hub, albums, artists,
//! tracks, search, recent, favorites, genres, playlists, info, lyrics, top,
//! similar, sessions, history, images, covers, match) is one
//! [`RemoteSource`] method with one shape, no matter which server answers.
//! The three clients share one HTTP layer,
//! [`RemoteHttp`](super::transport::RemoteHttp), and are built in
//! one place, the factory on
//! [`ConnectionResolver`](super::connections::ConnectionResolver);
//! handlers resolve a handle per request from the caller's stored
//! connection and never touch a source client directly.

use std::future::Future;
use std::pin::Pin;

use super::jellyfin::JellyfinAdapter;
use super::models::{
    AlbumPage, AlbumView, ArtistIndexEntry, ArtistPage, ArtistView, FavoritesView, HistoryPage,
    HubView, ImportResult, InfoView, LyricsView, PlaylistDetail, PlaylistSummary, SearchResults,
    SessionsView, SourceName, StatsView, TrackPage, TrackView,
};
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;
use super::warmup::IndexedAlbum;

/// Boxed future for object-safe async ports. Hand-boxed because `async fn`
/// is not `dyn`-compatible; mirrors the other store traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a [`RemoteSource`] call answers.
pub type RemoteFuture<'a, T> = BoxFuture<'a, Result<T, AdapterError>>;

/// Every way a remote call can fail. Handlers map these to the wire; only
/// `NotFound` and `Unsupported` surface their message, the rest render as
/// fixed user-safe summaries while the detail goes to the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    /// No credential or base URL was supplied for this source.
    NotConfigured,
    /// The server rejected the credential (HTTP 401/403, Subsonic 40/41).
    /// Callers must surface relink, never retry blindly.
    Auth,
    /// The server answered with an error status or invalid payload.
    Api(String),
    /// Nothing behind this id (Jellyfin 404, empty Plex container).
    NotFound,
    /// The source has no such capability.
    Unsupported(String),
    /// No answer at all: DNS, connect, TLS, timeout, reset.
    Transport(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("remote source is not configured"),
            Self::Auth => f.write_str("remote credential was rejected"),
            Self::Api(detail) => write!(f, "remote API error: {detail}"),
            Self::NotFound => f.write_str("remote item not found"),
            Self::Unsupported(detail) => write!(f, "unsupported: {detail}"),
            Self::Transport(detail) => write!(f, "remote transport failed: {detail}"),
        }
    }
}

/// One page of remote records plus the upstream match total.
#[derive(Debug, Clone, PartialEq)]
pub struct RemotePage<T> {
    /// Page items.
    pub items: Vec<T>,
    /// Total matching records upstream.
    pub total: i64,
}

/// Album browse parameters shared by every source.
#[derive(Debug, Clone)]
pub struct AlbumBrowse {
    /// Max items.
    pub limit: i64,
    /// Records to skip.
    pub offset: i64,
    /// Sort field, source-native name or empty for the source default.
    pub sort_by: String,
    /// True for descending.
    pub descending: bool,
    /// Genre filter, when non-empty.
    pub genre: String,
    /// Year filter, when set.
    pub year: Option<i32>,
    /// Decade filter in `"2020s"` spelling, when non-empty.
    pub decade: String,
    /// Mood filter (Plex), when non-empty.
    pub mood: String,
    /// Tag filter (Jellyfin), when non-empty.
    pub tags: String,
    /// Studio filter (Jellyfin), when non-empty.
    pub studios: String,
}

impl Default for AlbumBrowse {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            sort_by: String::new(),
            descending: false,
            genre: String::new(),
            year: None,
            decade: String::new(),
            mood: String::new(),
            tags: String::new(),
            studios: String::new(),
        }
    }
}

/// Artist browse parameters shared by every source.
#[derive(Debug, Clone)]
pub struct ArtistBrowse {
    /// Max items.
    pub limit: i64,
    /// Records to skip.
    pub offset: i64,
    /// Sort field, source-native name or empty for the source default.
    pub sort_by: String,
    /// True for descending.
    pub descending: bool,
    /// Name filter, when non-empty.
    pub search: String,
}

impl Default for ArtistBrowse {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            sort_by: String::new(),
            descending: false,
            search: String::new(),
        }
    }
}

/// Track browse parameters shared by every source.
#[derive(Debug, Clone)]
pub struct TrackBrowse {
    /// Max items.
    pub limit: i64,
    /// Records to skip.
    pub offset: i64,
    /// Sort field, source-native name or empty for the source default.
    pub sort_by: String,
    /// True for descending.
    pub descending: bool,
    /// Title filter, when non-empty.
    pub search: String,
    /// Genre filter, when non-empty.
    pub genre: String,
}

impl Default for TrackBrowse {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            sort_by: String::new(),
            descending: false,
            search: String::new(),
            genre: String::new(),
        }
    }
}

/// Receipt for one playlist import. The sink owns idempotency: importing
/// the same remote playlist twice returns the first receipt with
/// `already_imported` set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReceipt {
    /// Local playlist id holding the tracks.
    pub local_playlist_id: String,
    /// Tracks stored.
    pub tracks_imported: i64,
    /// Tracks that failed to resolve.
    pub tracks_failed: i64,
    /// True when this import was a repeat.
    pub already_imported: bool,
}

impl From<ImportReceipt> for ImportResult {
    fn from(receipt: ImportReceipt) -> Self {
        Self {
            local_playlist_id: receipt.local_playlist_id,
            tracks_imported: receipt.tracks_imported,
            tracks_failed: receipt.tracks_failed,
            already_imported: receipt.already_imported,
        }
    }
}

/// Where imported playlist tracks land: the user's playlists in
/// production, a memory sink in tests.
pub trait ImportSink: Send + Sync {
    /// Store `tracks` under `playlist_name` for `owner_id`, keyed by the
    /// remote identity for idempotency. The error text goes to the log.
    fn import<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
        remote_playlist_id: &'a str,
        playlist_name: &'a str,
        tracks: Vec<TrackView>,
    ) -> BoxFuture<'a, Result<ImportReceipt, String>>;

    /// Remote ids of the playlists from `source` that `owner_id` already
    /// imported. The error text goes to the log.
    fn imported_ids<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
    ) -> BoxFuture<'a, Result<std::collections::HashSet<String>, String>>;
}

/// The browse surface every remote source answers. A source without a
/// concept answers `Unsupported`, never garbage. Calls only one source has
/// stay on its client, reached through [`RemoteHandle::jellyfin`],
/// [`RemoteHandle::navidrome`] and [`RemoteHandle::plex`].
pub trait RemoteSource: Send + Sync {
    /// Connectivity probe. Returns a label naming the server.
    fn validate_connection(&self) -> RemoteFuture<'_, String>;

    /// Hub highlights: stats, recents, favorites, previews, genres.
    fn hub(&self) -> RemoteFuture<'_, HubView>;

    /// Library totals.
    fn stats(&self) -> RemoteFuture<'_, StatsView>;

    /// One page of albums.
    fn albums<'a>(&'a self, browse: &'a AlbumBrowse) -> RemoteFuture<'a, RemotePage<AlbumView>>;

    /// One album by id. None is absence.
    fn album_detail<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Option<AlbumView>>;

    /// Tracks of one album, in disc/track order.
    fn album_tracks<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Vec<TrackView>>;

    /// One page of artists.
    fn artists<'a>(&'a self, browse: &'a ArtistBrowse) -> RemoteFuture<'a, RemotePage<ArtistView>>;

    /// Full alphabetic artist index.
    fn artist_index(&self) -> RemoteFuture<'_, Vec<ArtistIndexEntry>>;

    /// One artist by id. None is absence.
    fn artist_detail<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Option<ArtistView>>;

    /// One page of tracks.
    fn tracks<'a>(&'a self, browse: &'a TrackBrowse) -> RemoteFuture<'a, RemotePage<TrackView>>;

    /// Unified search across artists, albums, and tracks.
    fn search<'a>(&'a self, query: &'a str, limit: i64) -> RemoteFuture<'a, SearchResults>;

    /// Recently played albums, newest first.
    fn recent(&self, limit: i64) -> RemoteFuture<'_, Vec<AlbumView>>;

    /// Recently added albums, newest first.
    fn recently_added(&self, limit: i64) -> RemoteFuture<'_, Vec<AlbumView>>;

    /// Favorites grouped by kind, up to `limit` of each.
    fn favorites(&self, limit: i64) -> RemoteFuture<'_, FavoritesView>;

    /// Genre labels.
    fn genres(&self) -> RemoteFuture<'_, Vec<String>>;

    /// Tracks carrying one genre label.
    fn genre_songs<'a>(
        &'a self,
        genre: &'a str,
        limit: i64,
        offset: i64,
    ) -> RemoteFuture<'a, Vec<TrackView>>;

    /// Playlists.
    fn playlists(&self) -> RemoteFuture<'_, Vec<PlaylistSummary>>;

    /// One playlist with its tracks. None is absence.
    fn playlist_detail<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Option<PlaylistDetail>>;

    /// Artist info passthrough (biography, image, similar artists).
    fn artist_info<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, InfoView>;

    /// Album info passthrough (notes, links, image).
    fn album_info<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, InfoView>;

    /// Lyrics passthrough. `artist`/`title` feed the Navidrome classic
    /// lookup when the id lookup comes back empty.
    fn lyrics<'a>(
        &'a self,
        id: &'a str,
        artist: Option<&'a str>,
        title: Option<&'a str>,
    ) -> RemoteFuture<'a, Option<LyricsView>>;

    /// Top songs for one artist name.
    fn top_songs<'a>(&'a self, artist: &'a str, limit: i64) -> RemoteFuture<'a, Vec<TrackView>>;

    /// Random tracks, optionally filtered by genre. Navidrome is a
    /// `getRandomSongs` passthrough, Jellyfin sorts Audio by `Random`,
    /// Plex has no such endpoint and answers `Unsupported`.
    fn random<'a>(&'a self, limit: i64, genre: &'a str) -> RemoteFuture<'a, Vec<TrackView>>;

    /// Tracks similar to one track.
    fn similar<'a>(&'a self, id: &'a str, limit: i64) -> RemoteFuture<'a, Vec<TrackView>>;

    /// Active audio sessions on the source.
    fn sessions(&self) -> RemoteFuture<'_, SessionsView>;

    /// Listening history, newest first.
    fn history(&self, limit: i64, offset: i64) -> RemoteFuture<'_, HistoryPage>;

    /// Item image bytes plus content type. The id is source-native: a
    /// Jellyfin item id, a Navidrome cover-art id, or a Plex rating key.
    fn image_bytes<'a>(&'a self, id: &'a str, size: i64) -> RemoteFuture<'a, (Vec<u8>, String)>;

    /// Playlist cover bytes plus content type.
    fn playlist_cover_bytes<'a>(
        &'a self,
        id: &'a str,
        size: i64,
    ) -> RemoteFuture<'a, (Vec<u8>, String)>;

    /// The direct audio request for one key, unsent. The key is
    /// source-native: a Jellyfin item id, a Navidrome song id, or a Plex
    /// part key. This is the stream gateway's remote read; folder scoping
    /// does not apply.
    fn audio_request(&self, key: &str) -> Result<reqwest::Request, AdapterError>;

    /// The first album in the server's answer to a search for `mbid` that
    /// carries it as its release or release-group id, inside this handle's
    /// scope.
    fn mbid_search<'a>(&'a self, mbid: &'a str) -> RemoteFuture<'a, Option<String>>;

    /// Every album that carries a MusicBrainz id, in the server's order and
    /// at most `cap` of them, each with the Navidrome folder or Plex section
    /// it was listed under.
    fn album_index(&self, cap: usize) -> RemoteFuture<'_, Vec<IndexedAlbum>>;

    /// Whether an album listed under `scope` lies inside this handle's
    /// Navidrome folders or Plex sections. Jellyfin has no such scope: what
    /// the account can read is checked by reading it.
    fn holds<'a>(&'a self, scope: Option<&'a str>) -> RemoteFuture<'a, bool>;
}

/// `impl RemoteSource` for a client whose own methods carry the trait's
/// names and arguments, plus the methods written out in braces.
macro_rules! remote_source {
    ($client:ty { $($own:tt)* }) => {
        impl RemoteSource for $client {
            fn validate_connection(&self) -> RemoteFuture<'_, String> {
                Box::pin(<$client>::validate_connection(self))
            }
            fn hub(&self) -> RemoteFuture<'_, HubView> {
                Box::pin(<$client>::hub(self))
            }
            fn stats(&self) -> RemoteFuture<'_, StatsView> {
                Box::pin(<$client>::stats(self))
            }
            fn albums<'a>(
                &'a self,
                browse: &'a AlbumBrowse,
            ) -> RemoteFuture<'a, RemotePage<AlbumView>> {
                Box::pin(<$client>::albums(self, browse))
            }
            fn album_detail<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Option<AlbumView>> {
                Box::pin(<$client>::album_detail(self, id))
            }
            fn album_tracks<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Vec<TrackView>> {
                Box::pin(<$client>::album_tracks(self, id))
            }
            fn artists<'a>(
                &'a self,
                browse: &'a ArtistBrowse,
            ) -> RemoteFuture<'a, RemotePage<ArtistView>> {
                Box::pin(<$client>::artists(self, browse))
            }
            fn artist_index(&self) -> RemoteFuture<'_, Vec<ArtistIndexEntry>> {
                Box::pin(<$client>::artist_index(self))
            }
            fn artist_detail<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, Option<ArtistView>> {
                Box::pin(<$client>::artist_detail(self, id))
            }
            fn tracks<'a>(
                &'a self,
                browse: &'a TrackBrowse,
            ) -> RemoteFuture<'a, RemotePage<TrackView>> {
                Box::pin(<$client>::tracks(self, browse))
            }
            fn search<'a>(&'a self, query: &'a str, limit: i64) -> RemoteFuture<'a, SearchResults> {
                Box::pin(<$client>::search(self, query, limit))
            }
            fn recent(&self, limit: i64) -> RemoteFuture<'_, Vec<AlbumView>> {
                Box::pin(<$client>::recent(self, limit))
            }
            fn recently_added(&self, limit: i64) -> RemoteFuture<'_, Vec<AlbumView>> {
                Box::pin(<$client>::recently_added(self, limit))
            }
            fn favorites(&self, limit: i64) -> RemoteFuture<'_, FavoritesView> {
                Box::pin(<$client>::favorites(self, limit))
            }
            fn genres(&self) -> RemoteFuture<'_, Vec<String>> {
                Box::pin(<$client>::genres(self))
            }
            fn genre_songs<'a>(
                &'a self,
                genre: &'a str,
                limit: i64,
                offset: i64,
            ) -> RemoteFuture<'a, Vec<TrackView>> {
                Box::pin(<$client>::genre_songs(self, genre, limit, offset))
            }
            fn playlists(&self) -> RemoteFuture<'_, Vec<PlaylistSummary>> {
                Box::pin(<$client>::playlists(self))
            }
            fn playlist_detail<'a>(
                &'a self,
                id: &'a str,
            ) -> RemoteFuture<'a, Option<PlaylistDetail>> {
                Box::pin(<$client>::playlist_detail(self, id))
            }
            fn artist_info<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, InfoView> {
                Box::pin(<$client>::artist_info(self, id))
            }
            fn album_info<'a>(&'a self, id: &'a str) -> RemoteFuture<'a, InfoView> {
                Box::pin(<$client>::album_info(self, id))
            }
            fn top_songs<'a>(
                &'a self,
                artist: &'a str,
                limit: i64,
            ) -> RemoteFuture<'a, Vec<TrackView>> {
                Box::pin(<$client>::top_songs(self, artist, limit))
            }
            fn random<'a>(&'a self, limit: i64, genre: &'a str) -> RemoteFuture<'a, Vec<TrackView>> {
                Box::pin(<$client>::random(self, limit, genre))
            }
            fn similar<'a>(&'a self, id: &'a str, limit: i64) -> RemoteFuture<'a, Vec<TrackView>> {
                Box::pin(<$client>::similar(self, id, limit))
            }
            fn sessions(&self) -> RemoteFuture<'_, SessionsView> {
                Box::pin(<$client>::sessions(self))
            }
            fn history(&self, limit: i64, offset: i64) -> RemoteFuture<'_, HistoryPage> {
                Box::pin(<$client>::history(self, limit, offset))
            }
            fn image_bytes<'a>(
                &'a self,
                id: &'a str,
                size: i64,
            ) -> RemoteFuture<'a, (Vec<u8>, String)> {
                Box::pin(<$client>::image_bytes(self, id, size))
            }
            fn playlist_cover_bytes<'a>(
                &'a self,
                id: &'a str,
                size: i64,
            ) -> RemoteFuture<'a, (Vec<u8>, String)> {
                Box::pin(<$client>::playlist_cover_bytes(self, id, size))
            }
            fn audio_request(&self, key: &str) -> Result<reqwest::Request, AdapterError> {
                <$client>::audio_request(self, key)
            }
            fn mbid_search<'a>(&'a self, mbid: &'a str) -> RemoteFuture<'a, Option<String>> {
                Box::pin(<$client>::mbid_search(self, mbid))
            }
            fn album_index(&self, cap: usize) -> RemoteFuture<'_, Vec<IndexedAlbum>> {
                Box::pin(<$client>::album_index(self, cap))
            }
            $($own)*
        }
    };
}

remote_source!(JellyfinAdapter {
    fn lyrics<'a>(
        &'a self,
        id: &'a str,
        _artist: Option<&'a str>,
        _title: Option<&'a str>,
    ) -> RemoteFuture<'a, Option<LyricsView>> {
        Box::pin(JellyfinAdapter::lyrics(self, id))
    }
    fn holds<'a>(&'a self, _scope: Option<&'a str>) -> RemoteFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
});

remote_source!(NavidromeAdapter {
    fn lyrics<'a>(
        &'a self,
        id: &'a str,
        artist: Option<&'a str>,
        title: Option<&'a str>,
    ) -> RemoteFuture<'a, Option<LyricsView>> {
        Box::pin(NavidromeAdapter::lyrics(self, id, artist, title))
    }
    fn holds<'a>(&'a self, scope: Option<&'a str>) -> RemoteFuture<'a, bool> {
        let held = NavidromeAdapter::holds(self, scope);
        Box::pin(async move { Ok(held) })
    }
});

remote_source!(PlexAdapter {
    fn lyrics<'a>(
        &'a self,
        id: &'a str,
        _artist: Option<&'a str>,
        _title: Option<&'a str>,
    ) -> RemoteFuture<'a, Option<LyricsView>> {
        Box::pin(PlexAdapter::lyrics(self, id))
    }
    fn holds<'a>(&'a self, scope: Option<&'a str>) -> RemoteFuture<'a, bool> {
        Box::pin(PlexAdapter::holds(self, scope))
    }
});

/// One remote source's client. It answers every [`RemoteSource`] call
/// through `Deref`; a call only one source has goes through the typed
/// accessors, which fail with `Unsupported` for another source.
#[derive(Debug)]
pub enum RemoteHandle {
    /// Jellyfin source client.
    Jellyfin(JellyfinAdapter),
    /// Navidrome source client.
    Navidrome(NavidromeAdapter),
    /// Plex source client.
    Plex(PlexAdapter),
}

impl std::ops::Deref for RemoteHandle {
    type Target = dyn RemoteSource;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Jellyfin(client) => client,
            Self::Navidrome(client) => client,
            Self::Plex(client) => client,
        }
    }
}

/// The answer for `what`, a call `source` does not have.
pub fn unsupported(source: SourceName, what: &str) -> AdapterError {
    AdapterError::Unsupported(format!("{} has no {what}", source.display()))
}

impl RemoteHandle {
    /// The source this handle reads.
    pub fn source(&self) -> SourceName {
        match self {
            Self::Jellyfin(_) => SourceName::Jellyfin,
            Self::Navidrome(_) => SourceName::Navidrome,
            Self::Plex(_) => SourceName::Plex,
        }
    }

    /// The Jellyfin client, for `what` only Jellyfin has.
    pub fn jellyfin(&self, what: &str) -> Result<&JellyfinAdapter, AdapterError> {
        match self {
            Self::Jellyfin(client) => Ok(client),
            _ => Err(unsupported(self.source(), what)),
        }
    }

    /// The Navidrome client, for `what` only Navidrome has.
    pub fn navidrome(&self, what: &str) -> Result<&NavidromeAdapter, AdapterError> {
        match self {
            Self::Navidrome(client) => Ok(client),
            _ => Err(unsupported(self.source(), what)),
        }
    }

    /// The Plex client, for `what` only Plex has.
    pub fn plex(&self, what: &str) -> Result<&PlexAdapter, AdapterError> {
        match self {
            Self::Plex(client) => Ok(client),
            _ => Err(unsupported(self.source(), what)),
        }
    }

    /// Ask the server to rescan its music: Jellyfin's library refresh,
    /// Plex's section refresh, Navidrome's Subsonic scan.
    pub async fn rescan(&self) -> Result<(), AdapterError> {
        match self {
            Self::Jellyfin(client) => client.refresh_library().await,
            Self::Navidrome(client) => client.start_scan().await,
            Self::Plex(client) => client.refresh_sections().await,
        }
    }

    /// Tracks of any of several genres, merged in genre order without
    /// duplicates. Each genre is read from its start up to the end of the
    /// page, so the merged list has the same order on every page and the
    /// page is a plain slice of it. v2 applied the offset to each genre
    /// instead, which skipped tracks on every page after the first.
    pub async fn genres_songs(
        &self,
        genres: &[String],
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        if let [genre] = genres {
            return self.genre_songs(genre, limit, offset).await;
        }
        let reach = offset + limit;
        let pages = futures_util::future::try_join_all(
            genres.iter().map(|genre| self.genre_songs(genre, reach, 0)),
        )
        .await?;
        let mut seen = std::collections::HashSet::new();
        Ok(pages
            .into_iter()
            .flatten()
            .filter(|track| seen.insert(track.id.clone()))
            .skip(usize::try_from(offset).unwrap_or(0))
            .take(usize::try_from(limit).unwrap_or(0))
            .collect())
    }
}

/// Build an [`AlbumPage`] from a remote page and the echoed query.
pub fn album_page(page: RemotePage<AlbumView>, offset: i64, limit: i64) -> AlbumPage {
    AlbumPage {
        items: page.items,
        total: page.total,
        offset,
        limit,
    }
}

/// Build an [`ArtistPage`] from a remote page and the echoed query.
pub fn artist_page(page: RemotePage<ArtistView>, offset: i64, limit: i64) -> ArtistPage {
    ArtistPage {
        items: page.items,
        total: page.total,
        offset,
        limit,
    }
}

/// Build a [`TrackPage`] from a remote page and the echoed query.
pub fn track_page(page: RemotePage<TrackView>, offset: i64, limit: i64) -> TrackPage {
    TrackPage {
        items: page.items,
        total: page.total,
        offset,
        limit,
    }
}
