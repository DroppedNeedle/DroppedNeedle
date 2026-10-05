//! The unified remote-source adapter surface (R1).
//!
//! One handle, three sources. [`RemoteHandle`] is enum-dispatched over the
//! per-source adapters so every browse concept (hub, albums, artists,
//! tracks, search, recent, favorites, genres, playlists, info, lyrics, top,
//! similar, sessions, history, images, covers, match) is one method with one
//! shape, no matter which server answers. Per-source constructors live on
//! the adapter structs; handlers resolve a handle per request from the
//! caller's stored connection and never touch a source client directly.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use super::jellyfin::JellyfinAdapter;
use super::models::{
    AlbumPage, AlbumView, ArtistIndexEntry, ArtistPage, ArtistView, FavoritesView, HistoryPage,
    HubView, ImportResult, InfoView, LyricsView, MatchView, PlaylistCollection, PlaylistDetail,
    SearchResults, SessionsView, SourceName, StatsView, TrackPage, TrackView,
};
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;

/// Boxed future for object-safe async ports. Hand-boxed because `async fn`
/// is not `dyn`-compatible; mirrors the sibling store traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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

/// Where imported playlist tracks land. The native playlist store plugs in
/// here; the slice ships a memory sink for tests and standalone use.
pub trait ImportSink: Send + Sync {
    /// Store `tracks` under `playlist_name` for `owner_id`, keyed by the
    /// remote identity for idempotency.
    fn import<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
        remote_playlist_id: &'a str,
        playlist_name: &'a str,
        tracks: Vec<TrackView>,
    ) -> BoxFuture<'a, ImportReceipt>;
}

/// In-memory import sink. Idempotency key is
/// `owner + source + remote playlist id`, mirroring the v2 import flow
/// which refuses to duplicate an already-imported remote playlist.
#[derive(Debug, Default)]
pub struct MemoryImportSink {
    inner: Mutex<MemoryImports>,
}

#[derive(Debug, Default)]
struct MemoryImports {
    receipts: HashMap<String, ImportReceipt>,
    stored: HashMap<String, Vec<TrackView>>,
    next_id: u64,
}

impl MemoryImportSink {
    /// Empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tracks stored under one local playlist id, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn stored(&self, local_playlist_id: &str) -> Vec<TrackView> {
        self.inner
            .lock()
            .map(|guard| {
                guard
                    .stored
                    .get(local_playlist_id)
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }
}

impl ImportSink for MemoryImportSink {
    fn import<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
        remote_playlist_id: &'a str,
        playlist_name: &'a str,
        tracks: Vec<TrackView>,
    ) -> BoxFuture<'a, ImportReceipt> {
        let _ = playlist_name;
        Box::pin(async move {
            let key = format!("{owner_id}\0{}\0{remote_playlist_id}", source.as_str());
            let mut guard = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(receipt) = guard.receipts.get(&key) {
                let mut repeat = receipt.clone();
                repeat.already_imported = true;
                return repeat;
            }
            guard.next_id += 1;
            let local_playlist_id = format!("local-playlist-{}", guard.next_id);
            guard
                .stored
                .insert(local_playlist_id.clone(), tracks.clone());
            let receipt = ImportReceipt {
                local_playlist_id: local_playlist_id.clone(),
                tracks_imported: tracks.len() as i64,
                tracks_failed: 0,
                already_imported: false,
            };
            guard.receipts.insert(key, receipt.clone());
            receipt
        })
    }
}

/// One adapter for all remote sources. Each variant holds its constructed
/// source client; every method below is the unified spelling of one browse
/// concept. Sources without a concept answer `Unsupported`, never garbage.
#[derive(Debug)]
pub enum RemoteHandle {
    /// Jellyfin source client.
    Jellyfin(JellyfinAdapter),
    /// Navidrome source client.
    Navidrome(NavidromeAdapter),
    /// Plex source client.
    Plex(PlexAdapter),
}

impl RemoteHandle {
    /// Which source this handle talks to.
    pub fn source(&self) -> SourceName {
        match self {
            Self::Jellyfin(_) => SourceName::Jellyfin,
            Self::Navidrome(_) => SourceName::Navidrome,
            Self::Plex(_) => SourceName::Plex,
        }
    }

    /// Hub highlights: stats, recents, favorites, previews, genres.
    pub async fn hub(&self) -> Result<HubView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.hub().await,
            Self::Navidrome(inner) => inner.hub().await,
            Self::Plex(inner) => inner.hub().await,
        }
    }

    /// Library totals.
    pub async fn stats(&self) -> Result<StatsView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.stats().await,
            Self::Navidrome(inner) => inner.stats().await,
            Self::Plex(inner) => inner.stats().await,
        }
    }

    /// One page of albums.
    pub async fn albums(
        &self,
        browse: &AlbumBrowse,
    ) -> Result<RemotePage<AlbumView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.albums(browse).await,
            Self::Navidrome(inner) => inner.albums(browse).await,
            Self::Plex(inner) => inner.albums(browse).await,
        }
    }

    /// One album by id. None is absence.
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.album_detail(id).await,
            Self::Navidrome(inner) => inner.album_detail(id).await,
            Self::Plex(inner) => inner.album_detail(id).await,
        }
    }

    /// Tracks of one album, in disc/track order.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.album_tracks(id).await,
            Self::Navidrome(inner) => inner.album_tracks(id).await,
            Self::Plex(inner) => inner.album_tracks(id).await,
        }
    }

    /// One page of artists.
    pub async fn artists(
        &self,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.artists(browse).await,
            Self::Navidrome(inner) => inner.artists(browse).await,
            Self::Plex(inner) => inner.artists(browse).await,
        }
    }

    /// Full alphabetic artist index.
    pub async fn artist_index(&self) -> Result<Vec<ArtistIndexEntry>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.artist_index().await,
            Self::Navidrome(inner) => inner.artist_index().await,
            Self::Plex(inner) => inner.artist_index().await,
        }
    }

    /// One artist by id. None is absence.
    pub async fn artist_detail(&self, id: &str) -> Result<Option<ArtistView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.artist_detail(id).await,
            Self::Navidrome(inner) => inner.artist_detail(id).await,
            Self::Plex(inner) => inner.artist_detail(id).await,
        }
    }

    /// One page of tracks.
    pub async fn tracks(
        &self,
        browse: &TrackBrowse,
    ) -> Result<RemotePage<TrackView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.tracks(browse).await,
            Self::Navidrome(inner) => inner.tracks(browse).await,
            Self::Plex(inner) => inner.tracks(browse).await,
        }
    }

    /// Unified search across artists, albums, and tracks.
    pub async fn search(&self, query: &str, limit: i64) -> Result<SearchResults, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.search(query, limit).await,
            Self::Navidrome(inner) => inner.search(query, limit).await,
            Self::Plex(inner) => inner.search(query, limit).await,
        }
    }

    /// Recently played albums, newest first.
    pub async fn recent(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.recent(limit).await,
            Self::Navidrome(inner) => inner.recent(limit).await,
            Self::Plex(inner) => inner.recent(limit).await,
        }
    }

    /// Recently added albums, newest first.
    pub async fn recently_added(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.recently_added(limit).await,
            Self::Navidrome(inner) => inner.recently_added(limit).await,
            Self::Plex(inner) => inner.recently_added(limit).await,
        }
    }

    /// Favorites grouped by kind.
    pub async fn favorites(&self) -> Result<FavoritesView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.favorites().await,
            Self::Navidrome(inner) => inner.favorites().await,
            Self::Plex(inner) => inner.favorites().await,
        }
    }

    /// Genre labels.
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.genres().await,
            Self::Navidrome(inner) => inner.genres().await,
            Self::Plex(inner) => inner.genres().await,
        }
    }

    /// Tracks carrying one genre label.
    pub async fn genre_songs(
        &self,
        genre: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.genre_songs(genre, limit, offset).await,
            Self::Navidrome(inner) => inner.genre_songs(genre, limit, offset).await,
            Self::Plex(inner) => inner.genre_songs(genre, limit, offset).await,
        }
    }

    /// Playlists.
    pub async fn playlists(&self) -> Result<Vec<super::models::PlaylistSummary>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.playlists().await,
            Self::Navidrome(inner) => inner.playlists().await,
            Self::Plex(inner) => inner.playlists().await,
        }
    }

    /// One playlist with its tracks. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.playlist_detail(id).await,
            Self::Navidrome(inner) => inner.playlist_detail(id).await,
            Self::Plex(inner) => inner.playlist_detail(id).await,
        }
    }

    /// Artist info passthrough (biography, image, similar artists).
    pub async fn artist_info(&self, id: &str) -> Result<InfoView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.artist_info(id).await,
            Self::Navidrome(inner) => inner.artist_info(id).await,
            Self::Plex(inner) => inner.artist_info(id).await,
        }
    }

    /// Album info passthrough (notes, links, image).
    pub async fn album_info(&self, id: &str) -> Result<InfoView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.album_info(id).await,
            Self::Navidrome(inner) => inner.album_info(id).await,
            Self::Plex(inner) => inner.album_info(id).await,
        }
    }

    /// Lyrics passthrough. `artist`/`title` feed the Navidrome classic
    /// lookup when the id lookup comes back empty.
    pub async fn lyrics(
        &self,
        id: &str,
        artist: Option<&str>,
        title: Option<&str>,
    ) -> Result<Option<LyricsView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.lyrics(id).await,
            Self::Navidrome(inner) => inner.lyrics(id, artist, title).await,
            Self::Plex(inner) => inner.lyrics(id).await,
        }
    }

    /// Top songs for one artist name.
    pub async fn top_songs(
        &self,
        artist: &str,
        limit: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.top_songs(artist, limit).await,
            Self::Navidrome(inner) => inner.top_songs(artist, limit).await,
            Self::Plex(inner) => inner.top_songs(artist, limit).await,
        }
    }

    /// Random tracks, optionally filtered by genre. Navidrome is a
    /// `getRandomSongs` passthrough, Jellyfin sorts Audio by `Random`,
    /// Plex has no such endpoint and answers `Unsupported`.
    pub async fn random(&self, limit: i64, genre: &str) -> Result<Vec<TrackView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.random(limit, genre).await,
            Self::Navidrome(inner) => inner.random(limit, genre).await,
            Self::Plex(inner) => inner.random(limit, genre).await,
        }
    }

    /// Tracks similar to one track.
    pub async fn similar(&self, id: &str, limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.similar(id, limit).await,
            Self::Navidrome(inner) => inner.similar(id, limit).await,
            Self::Plex(inner) => inner.similar(id, limit).await,
        }
    }

    /// Active audio sessions on the source.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.sessions().await,
            Self::Navidrome(inner) => inner.sessions().await,
            Self::Plex(inner) => inner.sessions().await,
        }
    }

    /// Listening history, newest first.
    pub async fn history(&self, limit: i64, offset: i64) -> Result<HistoryPage, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.history(limit, offset).await,
            Self::Navidrome(inner) => inner.history(limit, offset).await,
            Self::Plex(inner) => inner.history(limit, offset).await,
        }
    }

    /// Item image bytes plus content type. The id is source-native: a
    /// Jellyfin item id, a Navidrome cover-art id, or a Plex rating key.
    pub async fn image_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.image_bytes(id, size).await,
            Self::Navidrome(inner) => inner.image_bytes(id, size).await,
            Self::Plex(inner) => inner.image_bytes(id, size).await,
        }
    }

    /// Playlist cover bytes plus content type.
    pub async fn playlist_cover_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.playlist_cover_bytes(id, size).await,
            Self::Navidrome(inner) => inner.playlist_cover_bytes(id, size).await,
            Self::Plex(inner) => inner.playlist_cover_bytes(id, size).await,
        }
    }

    /// Direct audio bytes plus content type. The key is source-native: a
    /// Jellyfin item id, a Navidrome song id, or a Plex part key. This is
    /// the stream gateway's remote read; folder scoping does not apply.
    pub async fn audio_bytes(&self, key: &str) -> Result<(Vec<u8>, String), AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.audio_bytes(key).await,
            Self::Navidrome(inner) => inner.audio_bytes(key).await,
            Self::Plex(inner) => inner.audio_bytes(key).await,
        }
    }

    /// Resolve a MusicBrainz release or release-group id to a remote album.
    pub async fn match_album(&self, mbid: &str) -> Result<MatchView, AdapterError> {
        match self {
            Self::Jellyfin(inner) => inner.match_album(mbid).await,
            Self::Navidrome(inner) => inner.match_album(mbid).await,
            Self::Plex(inner) => inner.match_album(mbid).await,
        }
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

/// Build a [`PlaylistCollection`] from playlist summaries.
pub fn playlist_collection(items: Vec<super::models::PlaylistSummary>) -> PlaylistCollection {
    PlaylistCollection { items }
}
