//! The unified remote-source surface.
//!
//! One handle, three sources. Every browse concept (hub, albums, artists,
//! tracks, search, recent, favorites, genres, playlists, info, lyrics, top,
//! similar, sessions, history, images, covers, match) is one
//! [`RemoteSource`] method with one shape, no matter which server answers.
//! The three clients share one HTTP layer, [`RemoteHttp`], and are built in
//! one place, the factory on
//! [`ConnectionResolver`](super::connections::ConnectionResolver);
//! handlers resolve a handle per request from the caller's stored
//! connection and never touch a source client directly.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::jellyfin::JellyfinAdapter;
use super::models::{
    AlbumPage, AlbumView, ArtistIndexEntry, ArtistPage, ArtistView, FavoritesView, HistoryPage,
    HubView, ImportResult, InfoView, LyricsView, PlaylistDetail, PlaylistSummary, SearchResults,
    SessionsView, SourceName, StatsView, TrackPage, TrackView,
};
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;
use super::warmup::IndexedAlbum;
use droppedneedle_kernel::http_client::{BodyError, read_capped};

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

/// The most one remote image may weigh, the same 20 MiB as the cover-art
/// fetch (`providers::coverart::TRANSPORT_MAX_BODY_BYTES`). Jellyfin and
/// Navidrome resize to the size asked for, a few megabytes at most. Plex
/// may send the stored picture, so a very large scan can pass the cap and
/// then shows as a missing cover.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// The most one API answer may weigh. Paged calls ask for 500 items at
/// most, a few megabytes. Three lists come whole: a playlist's tracks,
/// the starred items and Navidrome's artist index. At 1 to 3 KB a track
/// that is tens of megabytes for a playlist of ten thousand tracks, so
/// 128 MiB leaves room for a far larger one and still bounds the read.
pub const MAX_API_BYTES: usize = 128 * 1024 * 1024;

/// Read one image body under [`MAX_IMAGE_BYTES`]. An oversized image is
/// no image: the caller answers as it does for a missing one.
async fn read_image(
    source: SourceName,
    endpoint: &str,
    response: reqwest::Response,
) -> Result<Vec<u8>, AdapterError> {
    match read_capped(response, MAX_IMAGE_BYTES).await {
        Ok(bytes) => Ok(bytes),
        Err(BodyError::TooLarge) => {
            tracing::warn!(
                source = source.as_str(),
                endpoint,
                cap_bytes = MAX_IMAGE_BYTES,
                "remote image is over the size cap, treating it as missing"
            );
            Err(AdapterError::NotFound)
        }
        Err(BodyError::Transport(detail)) => Err(AdapterError::Transport(detail)),
    }
}

/// Read one API answer under [`MAX_API_BYTES`]. One over the cap is an
/// upstream error like any other answer the adapter cannot use.
async fn read_api_body(
    source: SourceName,
    endpoint: &str,
    response: reqwest::Response,
) -> Result<Vec<u8>, AdapterError> {
    match read_capped(response, MAX_API_BYTES).await {
        Ok(bytes) => Ok(bytes),
        Err(BodyError::TooLarge) => {
            tracing::warn!(
                source = source.as_str(),
                endpoint,
                cap_bytes = MAX_API_BYTES,
                "remote API answer is over the size cap"
            );
            Err(AdapterError::Api(format!(
                "{endpoint} answer is over the size cap"
            )))
        }
        Err(BodyError::Transport(detail)) => Err(AdapterError::Transport(detail)),
    }
}

/// The longest id accepted from a request. Real ones are 36 characters at
/// most.
const MAX_REQUEST_ID_BYTES: usize = 128;

/// An id that came from a request, kept only when it is a plain id:
/// ASCII letters, digits, `-` and `_`, not empty and not over-long. Plex
/// and Jellyfin take ids as part of the request path, and the web
/// framework has already decoded the id, so `../Users` or `12?x=1` would
/// send the held credential to another path. Anything else is not found,
/// with no request made.
///
/// Shapes checked: every `Id`, `ItemId`, `AlbumId` and `ParentId` in the
/// recorded Jellyfin 10.10.6 answers under `server/tests/reference/compat/`
/// is 32 hex characters, and the in-repo mocks use forms like `jf-al-1` and
/// `px-pl-1`. Plex rating keys are digits and Jellyfin also accepts the
/// dashed UUID form; neither was probed against a live server here.
pub(crate) fn request_id(id: &str) -> Result<&str, AdapterError> {
    let plain = !id.is_empty()
        && id.len() <= MAX_REQUEST_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if plain {
        Ok(id)
    } else {
        Err(AdapterError::NotFound)
    }
}

/// The address a path is sent to, checked on what was built and not on
/// the text: the same server as the base address, exactly the path asked
/// for under it, and no query or fragment of the path's own.
fn checked_url(base: &str, path: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(&format!("{base}{path}")).ok()?;
    let base = reqwest::Url::parse(base).ok()?;
    let expected = format!("{}{path}", base.path().trim_end_matches('/'));
    let same_server = url.origin() == base.origin()
        && url.username() == base.username()
        && url.password() == base.password();
    (same_server && url.path() == expected && url.query().is_none() && url.fragment().is_none())
        .then_some(url)
}

/// How long one remote API call may take, matching the v2 repositories.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The most API answers a person's reads download from one source at
/// once. Each answer may weigh up to [`MAX_API_BYTES`], so ten parallel
/// downloads from a hostile server could hold over a gigabyte for a
/// moment; a place is held until the body is read and freed before it is
/// decoded. Four keeps a page's own parallel reads (the hub reads a few at
/// once) moving. Images, audio streams and background work are not
/// counted.
pub const USER_READS_PER_SOURCE: usize = 4;

/// A transport failure with the address left out: it can carry
/// credentials (Subsonic `u`/`t`/`s`, Plex and Jellyfin tokens).
pub(crate) fn transport(cause: reqwest::Error) -> AdapterError {
    AdapterError::Transport(cause.without_url().to_string().chars().take(200).collect())
}

/// The failure a status stands for, `None` on success: 401 and 403 are a
/// rejected credential, anything else an API error naming `what`.
pub(crate) fn status_error(status: reqwest::StatusCode, what: &str) -> Option<AdapterError> {
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        Some(AdapterError::Auth)
    } else if status.is_success() {
        None
    } else {
        Some(AdapterError::Api(format!("{what} failed ({status})")))
    }
}

/// Decode one payload. A shape we cannot read (a missing id included) is
/// an upstream contract error; the detail stays in the log.
pub(crate) fn decode<T: DeserializeOwned>(
    source: SourceName,
    endpoint: &str,
    bytes: &[u8],
) -> Result<T, AdapterError> {
    serde_json::from_slice(bytes).map_err(|error| {
        AdapterError::Api(format!(
            "{} returned an unreadable {endpoint} payload: {error}",
            source.display()
        ))
    })
}

/// The HTTP side the three source clients share: the timeout, the status
/// mapping, capped reads, JSON decoding and addresses built only from the
/// parsed base. Each client adds its own auth to the requests it builds.
#[derive(Clone)]
pub(crate) struct RemoteHttp {
    client: reqwest::Client,
    source: SourceName,
    bound: Option<Arc<Semaphore>>,
}

impl RemoteHttp {
    /// Calls to `source` over `client`. With a `bound`, each call waits for
    /// a place in it and holds that place until its body is read.
    pub(crate) fn new(
        client: reqwest::Client,
        source: SourceName,
        bound: Option<Arc<Semaphore>>,
    ) -> Self {
        Self {
            client,
            source,
            bound,
        }
    }

    /// The client requests are built on.
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// The address of `path` on the server at `base`. A base that does not
    /// parse names no server, which reads as not reaching it; a path that
    /// would land anywhere but exactly there is no such item.
    pub(crate) fn url(base: &str, path: &str) -> Result<reqwest::Url, AdapterError> {
        if reqwest::Url::parse(base).is_err() {
            return Err(AdapterError::Transport(
                "the server address does not parse".to_owned(),
            ));
        }
        checked_url(base, path).ok_or(AdapterError::NotFound)
    }

    /// Send one request for `endpoint` under the timeout. No answer is a
    /// transport error, and 401 or 403 a rejected credential.
    pub(crate) async fn send(
        &self,
        endpoint: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<Answer, AdapterError> {
        let request = request
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(transport)?;
        let what = format!("{} {endpoint}", request.method());
        // The semaphore is never closed, so a place always comes.
        let permit = match &self.bound {
            Some(bound) => Arc::clone(bound).acquire_owned().await.ok(),
            None => None,
        };
        let response = self.client.execute(request).await.map_err(transport)?;
        if let Some(AdapterError::Auth) = status_error(response.status(), &what) {
            return Err(AdapterError::Auth);
        }
        Ok(Answer {
            response,
            source: self.source,
            endpoint: endpoint.to_owned(),
            what,
            _permit: permit,
        })
    }
}

/// One answer from a remote server. It keeps its place in the source's
/// bound until its body is read or it is dropped.
pub(crate) struct Answer {
    response: reqwest::Response,
    source: SourceName,
    endpoint: String,
    what: String,
    _permit: Option<OwnedSemaphorePermit>,
}

impl Answer {
    /// The answer's status.
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    /// The address that answered, after any redirect.
    pub(crate) fn url(&self) -> &reqwest::Url {
        self.response.url()
    }

    fn success(&self) -> Result<(), AdapterError> {
        status_error(self.response.status(), &self.what).map_or(Ok(()), Err)
    }

    /// A success status, the body left unread.
    pub(crate) fn done(self) -> Result<(), AdapterError> {
        self.success()
    }

    /// The body of a successful answer, read under [`MAX_API_BYTES`].
    pub(crate) async fn body(self) -> Result<Vec<u8>, AdapterError> {
        self.success()?;
        read_api_body(self.source, &self.endpoint, self.response).await
    }

    /// The JSON body of a successful answer.
    pub(crate) async fn json<T: DeserializeOwned>(self) -> Result<T, AdapterError> {
        let (source, endpoint) = (self.source, self.endpoint.clone());
        let bytes = self.body().await?;
        decode(source, &endpoint, &bytes)
    }

    /// The image body of a successful answer, read under
    /// [`MAX_IMAGE_BYTES`], with its content type or
    /// `fallback_content_type` when it names none. A 404 is an item with
    /// no image, which is absence.
    pub(crate) async fn image(
        self,
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if self.response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AdapterError::NotFound);
        }
        self.success()?;
        let content_type = self
            .response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(fallback_content_type)
            .to_owned();
        let bytes = read_image(self.source, &self.endpoint, self.response).await?;
        Ok((bytes, content_type))
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
