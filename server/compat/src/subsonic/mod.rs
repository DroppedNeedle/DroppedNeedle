//! Subsonic/OpenSubsonic compat API.
//!
//! Pinned protocol `1.16.1`, ported quirk-for-quirk from v2's Subsonic
//! compat package. Every behavior below cites its source; the golden tests
//! pin the wire bytes.
//!
//! # Endpoint x fields x auth
//!
//! All endpoints live under `GET|POST|HEAD /subsonic/rest/{endpoint}`
//! (one `.view` suffix stripped, name casefolded). Unknown methods are
//! a plain 404. Only `getOpenSubsonicExtensions` is public; everything else,
//! binary endpoints included, needs app-password auth. Formats: `f` in
//! `xml` (default), `json`, `jsonp` (unsafe callbacks fall back to JSON).
//! Beside them, `GET|HEAD /subsonic/img` serves album and artist pictures
//! from signed links without sign-in ([`picture`]).
//!
//! | Endpoint | Key | Auth | Notes |
//! |---|---|---|---|
//! | ping | ok | yes | auth probe |
//! | getLicense | license | yes | static valid:true |
//! | getOpenSubsonicExtensions | openSubsonicExtensions | PUBLIC | 3 exts x v1 |
//! | getMusicFolders | musicFolders | yes | only folder 1; others 70 |
//! | getArtists/getIndexes | artists/indexes | yes | article-stripped A-Z buckets |
//! | getArtist/getAlbum/getSong | artist/album/song | yes | artist embeds albums, album embeds songs |
//! | getAlbumList2/getAlbumList | albumList2/albumList | yes | `highest` = caller's rated, best first; byYear needs from+to |
//! | getRandomSongs | randomSongs | yes | filters |
//! | getMusicDirectory | directory | yes | 1 -> artists; artist -> albums; album -> songs |
//! | search3/search2 | searchResult3/2 | yes | missing/empty/`""` query = match-all |
//! | getCoverArt | bytes | yes | RG art/artist image/playlist file; miss -> WebP; size buckets |
//! | stream/download | bytes | yes | decide() policy; ranges; 416; HEAD |
//! | getTranscodeDecision | transcodeDecision | yes | POST-only, JSON-only |
//! | getTranscodeStream | bytes | yes | signed params |
//! | getPlaylists/getPlaylist | playlists/playlist | yes | streamable-only counts (#181) |
//! | createPlaylist | playlist | yes | with playlistId = replace |
//! | updatePlaylist/deletePlaylist | ok | yes | rename/public/add/remove-by-index |
//! | star/unstar | ok | yes | prefix-routed, deduped; empty -> 10 |
//! | getStarred2/getStarred | starred2/starred | yes | ID3 vs file shapes |
//! | setRating | ok | yes | per user, 1-5; 0 clears; unknown target 70 |
//! | scrobble | ok | yes | parallel time[]; submission=false -> now-playing |
//! | getNowPlaying | nowPlaying | yes | presence |
//! | reportPlayback | ok | yes | GET/form/JSON; extension playbackReport:1 |
//! | getPlayQueue/savePlayQueue | playQueue/ok | yes | <=500; current consistency |
//! | getPlayQueueByIndex/savePlayQueueByIndex | playQueueByIndex/ok | yes | extension indexBasedQueue:1 |
//! | getBookmarks/createBookmark/deleteBookmark | bookmarks/ok | yes | pinned bookmark fields |
//! | getArtistInfo[2]/getAlbumInfo[2] | artistInfo[2]/albumInfo | yes | projected MBIDs + sized open picture links |
//! | getAvatar | bytes | yes, self-only | non-self -> 403-as-text |
//! | getLyricsBySongId | lyricsList | yes | extension songLyrics:1 |
//! | getLyrics | lyrics | yes | exact title (+artist) match; miss -> empty value |
//! | getGenres/getSongsByGenre | genres/songsByGenre | yes | counts are ints |
//! | getUser | user | yes | username param ignored, returns caller |
//! | getScanStatus/startScan | scanStatus | yes | startScan admin-only -> 50 |
//! | getTopSongs | topSongs | yes | exact artist-name match else 70 |
//! | getSimilarSongs[2] | similarSongs[2] | yes | takes an artist id |
//!
//! # Quirk citations (all ported, none dropped)
//!
//! - Match-all search (Symfonium #129 / Arpeggi / gonic#229): missing,
//!   empty, or quote-only `query` matches everything; surrounding
//!   quotes stripped (`browse::normalize_search_query`).
//! - Feishin bitrate-0 (#464/#468): client bitrate caps <= 0 mean unset,
//!   in `decide()` and in getTranscodeDecision client parsing.
//! - Feishin lowercase paths: case-insensitive route matching lives in
//!   the edge middleware (`compat::shared::path_case`);
//!   endpoint names are casefolded here too.
//! - Feishin playlist cover art (#287): getCoverArt serves playlist art.
//! - Navidrome 0.62.0: repeated-same musicFolderId accepted, any other
//!   folder id is 70 (`browse::validate_music_folder`).
//! - Navidrome 0.62.0 (recordings `error.unknown-method` and
//!   `error.stream-unknown-id`): an unknown method is a plain 404 text in
//!   every format; a `stream` failure with code 70 (an unknown id, or no
//!   audio behind a known one) is the envelope over HTTP 200. `download`
//!   and `getCoverArt` misses were not recorded, so they stay text 404s.
//! - Navidrome 0.62.0 ratings (recording `navidrome-0.62.0-ratings`): a
//!   song, album or ID3 artist carries the caller's `userRating` and the
//!   mean of every user's as `averageRating` (whole means as integers).
//!   Each is left out when there is none, except an album's
//!   `userRating`, which is 0. `highest` lists only the albums the
//!   caller rated.
//! - Streamable-only playlist counts (#181).
//! - getAvatar 403-as-text for non-self usernames: the only 403-as-text;
//!   code 50 anywhere in the dispatch path stays enveloped.
//! - Binary-vs-envelope split: binary dispatch errors outside
//!   {10,40,41,42,43,44,50} render `text/plain` (70 -> 404, else 404),
//!   except a `stream` failure with code 70, which is enveloped.
//! - Transcode hints on song children only when transcoding is enabled
//!   and ffmpeg is present.
//! - getOpenSubsonicExtensions lists every served extension (all v1):
//!   apiKeyAuthentication, formPost, transcodeOffset, songLyrics,
//!   playbackReport, indexBasedQueue, transcoding.
//!
//! # What the handlers call (in `compat::adapters`, built in `compat::setup`)
//!
//! - Auth: [`auth::Credentials`] classification here; secret verification
//!   lives in `auth::compat_auth::subsonic::authenticate`, and
//!   [`SubsonicVerifier`](crate::adapters::principal::SubsonicVerifier)
//!   delegates to it. The HTTP adapter (`compat::http`) gates, verifies
//!   and runs the method from [`ENDPOINTS`].
//! - Data: [`SubsonicStore`] (library, playlists, favorites, scrobble,
//!   queues, bookmarks, lyrics, avatars, scan, cover art, advanced
//!   transcode).
//! - Audio bytes: [`GatewayAudio`](crate::adapters::audio::GatewayAudio)
//!   on the stream gateway (ranges, HEAD, leases per caller).
//! - Edge (`compat::shared`): enablement kill-switch default, rate limits,
//!   CORS, case-insensitive paths, access-log redaction. The enablement
//!   gate itself (`enabled` in [`Settings`]) runs in the HTTP adapter
//!   before lookup.

pub mod advanced;
pub mod auth;
pub mod browse;
pub mod error;
pub mod ids;
pub mod library;
pub mod media;
pub mod models;
pub mod params;
pub mod picture;
pub mod store;
pub mod stream;
pub mod value;
pub mod views;

use std::future::Future;
use std::pin::Pin;

use droppedneedle_kernel::runtime_config::sections::DownloadAccess;
use serde::Serialize;

use crate::adapters::library::CompatError;
use crate::adapters::principal::CompatPrincipal;
use crate::adapters::subsonic_store::SubsonicStore;
use crate::setup::CompatAudio;

pub use auth::{Credentials, classify};
pub use error::{GENERIC, PARAM_MISSING, SubsonicError};
pub use params::SubsonicParameters;
pub use value::{
    Payload, Rendered, SubsonicFormat, parse_format, render_binary_error, render_error, render_ok,
    sniff_format,
};

/// Codes that stay enveloped on the binary dispatch path (v2 `_AUTH_CODES`).
pub const AUTH_CODES: &[u8] = &[10, 40, 41, 42, 43, 44, 50];

/// getAvatar refusal for a non-self username, v2 message verbatim.
pub const AVATAR_FORBIDDEN_MESSAGE: &str = "Avatar access is limited to the authenticated user";

/// Normalize an endpoint name: casefold, strip one `.view` suffix
/// (v2 `_dispatch`).
pub fn normalize_endpoint(raw: &str) -> String {
    let folded = raw.to_lowercase();
    folded.strip_suffix(".view").unwrap_or(&folded).to_owned()
}

/// A handler's answer, still to be awaited.
pub type HandlerFuture<'c> =
    Pin<Box<dyn Future<Output = Result<Outcome, SubsonicError>> + Send + 'c>>;

/// A method's handler.
pub type Handler = for<'c> fn(&'c Ctx<'c>) -> HandlerFuture<'c>;

/// One Subsonic method (v2 `_HANDLERS`, `_BINARY` and `_PUBLIC`).
pub struct Endpoint {
    /// Normalized name.
    pub name: &'static str,
    /// The handler.
    pub handler: Handler,
    /// Answers bytes: dispatch-path errors outside [`AUTH_CODES`] render
    /// `text/plain`.
    pub binary: bool,
    /// Answers without sign-in.
    pub public: bool,
}

impl Endpoint {
    const fn envelope(name: &'static str, handler: Handler) -> Self {
        Self {
            name,
            handler,
            binary: false,
            public: false,
        }
    }

    const fn binary(name: &'static str, handler: Handler) -> Self {
        Self {
            name,
            handler,
            binary: true,
            public: false,
        }
    }

    const fn public(name: &'static str, handler: Handler) -> Self {
        Self {
            name,
            handler,
            binary: false,
            public: true,
        }
    }
}

/// Every Subsonic method. Only `getOpenSubsonicExtensions` is public;
/// stream, download, getCoverArt, getAvatar and getTranscodeStream answer
/// bytes.
pub static ENDPOINTS: &[Endpoint] = &[
    Endpoint::envelope("ping", |c| Box::pin(browse::ping(c))),
    Endpoint::envelope("getlicense", |c| Box::pin(browse::license(c))),
    Endpoint::public("getopensubsonicextensions", |c| {
        Box::pin(browse::extensions(c))
    }),
    Endpoint::envelope("getmusicfolders", |c| Box::pin(browse::music_folders(c))),
    Endpoint::envelope("getartists", |c| Box::pin(browse::artists(c))),
    Endpoint::envelope("getindexes", |c| Box::pin(browse::indexes(c))),
    Endpoint::envelope("getartist", |c| Box::pin(browse::artist(c))),
    Endpoint::envelope("getalbum", |c| Box::pin(browse::album(c))),
    Endpoint::envelope("getsong", |c| Box::pin(browse::song(c))),
    Endpoint::envelope("getalbumlist2", |c| Box::pin(browse::album_list2(c))),
    Endpoint::envelope("getalbumlist", |c| Box::pin(browse::album_list(c))),
    Endpoint::envelope("getrandomsongs", |c| Box::pin(browse::random_songs(c))),
    Endpoint::envelope("getmusicdirectory", |c| {
        Box::pin(browse::music_directory(c))
    }),
    Endpoint::envelope("search3", |c| Box::pin(browse::search3(c))),
    Endpoint::envelope("search2", |c| Box::pin(browse::search2(c))),
    Endpoint::binary("getcoverart", |c| Box::pin(media::cover_art(c))),
    Endpoint::binary("stream", |c| Box::pin(media::stream(c))),
    Endpoint::binary("download", |c| Box::pin(media::download(c))),
    Endpoint::envelope("gettranscodedecision", |c| {
        Box::pin(media::transcode_decision(c))
    }),
    Endpoint::binary("gettranscodestream", |c| {
        Box::pin(media::transcode_stream(c))
    }),
    Endpoint::envelope("getplaylists", |c| Box::pin(library::playlists(c))),
    Endpoint::envelope("getplaylist", |c| Box::pin(library::playlist(c))),
    Endpoint::envelope("createplaylist", |c| Box::pin(library::create_playlist(c))),
    Endpoint::envelope("updateplaylist", |c| Box::pin(library::update_playlist(c))),
    Endpoint::envelope("deleteplaylist", |c| Box::pin(library::delete_playlist(c))),
    Endpoint::envelope("star", |c| Box::pin(library::star(c, true))),
    Endpoint::envelope("unstar", |c| Box::pin(library::star(c, false))),
    Endpoint::envelope("getstarred2", |c| Box::pin(library::starred2(c))),
    Endpoint::envelope("getstarred", |c| Box::pin(library::starred(c))),
    Endpoint::envelope("setrating", |c| Box::pin(library::set_rating(c))),
    Endpoint::envelope("scrobble", |c| Box::pin(library::scrobble(c))),
    Endpoint::envelope("getnowplaying", |c| Box::pin(library::now_playing(c))),
    Endpoint::envelope("reportplayback", |c| Box::pin(library::report_playback(c))),
    Endpoint::envelope("getplayqueue", |c| Box::pin(library::get_play_queue(c))),
    Endpoint::envelope("saveplayqueue", |c| Box::pin(library::save_play_queue(c))),
    Endpoint::envelope("getplayqueuebyindex", |c| {
        Box::pin(library::get_play_queue_by_index(c))
    }),
    Endpoint::envelope("saveplayqueuebyindex", |c| {
        Box::pin(library::save_play_queue_by_index(c))
    }),
    Endpoint::envelope("getbookmarks", |c| Box::pin(library::get_bookmarks(c))),
    Endpoint::envelope("createbookmark", |c| Box::pin(library::create_bookmark(c))),
    Endpoint::envelope("deletebookmark", |c| Box::pin(library::delete_bookmark(c))),
    Endpoint::envelope("getartistinfo2", |c| Box::pin(library::artist_info(c))),
    Endpoint::envelope("getartistinfo", |c| Box::pin(library::artist_info(c))),
    Endpoint::envelope("getalbuminfo2", |c| Box::pin(library::album_info(c))),
    Endpoint::envelope("getalbuminfo", |c| Box::pin(library::album_info(c))),
    Endpoint::binary("getavatar", |c| Box::pin(media::avatar(c))),
    Endpoint::envelope("getlyricsbysongid", |c| {
        Box::pin(library::lyrics_by_song_id(c))
    }),
    Endpoint::envelope("getlyrics", |c| Box::pin(library::lyrics(c))),
    Endpoint::envelope("getgenres", |c| Box::pin(library::genres(c))),
    Endpoint::envelope("getsongsbygenre", |c| Box::pin(library::songs_by_genre(c))),
    Endpoint::envelope("getuser", |c| Box::pin(library::user(c))),
    Endpoint::envelope("getscanstatus", |c| Box::pin(library::scan_status(c))),
    Endpoint::envelope("startscan", |c| Box::pin(library::start_scan(c))),
    Endpoint::envelope("gettopsongs", |c| Box::pin(library::top_songs(c))),
    Endpoint::envelope("getsimilarsongs2", |c| Box::pin(library::similar_songs2(c))),
    Endpoint::envelope("getsimilarsongs", |c| Box::pin(library::similar_songs(c))),
];

/// The method with this normalized name, or `None` for an unknown method.
pub fn endpoint(name: &str) -> Option<&'static Endpoint> {
    ENDPOINTS.iter().find(|endpoint| endpoint.name == name)
}

/// Dispatch rule: binary endpoints render codes outside [`AUTH_CODES`]
/// as `text/plain`, except a `stream` failure with code 70, which stays
/// enveloped as Navidrome 0.62.0 answers it. Other endpoints are always
/// enveloped.
pub fn dispatch_uses_envelope(code: u8, normalized_endpoint: &str) -> bool {
    !endpoint(normalized_endpoint).is_some_and(|endpoint| endpoint.binary)
        || AUTH_CODES.contains(&code)
        || (normalized_endpoint == "stream" && code == error::NOT_FOUND)
}

/// Server settings the Subsonic API reads (filled from preferences).
#[derive(Debug, Clone)]
pub struct Settings {
    /// Kill switch; off -> failed envelope code 0 (v2 enablement).
    pub enabled: bool,
    /// Advertised server name (`type` in the envelope).
    pub server_name: String,
    /// Live app version (`serverVersion`).
    pub server_version: String,
    /// Transcoding master switch.
    pub transcoding_enabled: bool,
    /// Default output format (`mp3`/`opus`).
    pub transcode_default_format: String,
    /// Server quality ceiling kbps (never a transcode trigger).
    pub transcode_max_bitrate_kbps: i64,
    /// Whether ffmpeg is available.
    pub ffmpeg_available: bool,
    /// The admin's setting for who may download library files.
    pub download_access: DownloadAccess,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Kill switches default off: serving the
            // API is opt-in, never an accident of `Default`.
            enabled: false,
            server_name: "DroppedNeedle".to_owned(),
            server_version: "dev".to_owned(),
            transcoding_enabled: false,
            transcode_default_format: "mp3".to_owned(),
            transcode_max_bitrate_kbps: 320,
            ffmpeg_available: false,
            download_access: DownloadAccess::Everyone,
        }
    }
}

impl Settings {
    /// Transcode hint for song children: Some iff transcoding is on and
    /// ffmpeg is present (v2 `_transcode_hint`).
    pub fn transcode_hint(&self) -> Option<(String, String)> {
        if self.transcoding_enabled && self.ffmpeg_available {
            let (content_type, suffix) = stream::transcode_hint(&self.transcode_default_format);
            Some((content_type.to_owned(), suffix.to_owned()))
        } else {
            None
        }
    }
}

/// Handler context: everything one endpoint call needs.
pub struct Ctx<'a> {
    /// Normalized endpoint name.
    pub endpoint_name: String,
    /// Decoded params.
    pub params: SubsonicParameters,
    /// Authenticated principal (None only for the public endpoint).
    pub principal: Option<CompatPrincipal>,
    /// Data store, scoped to the caller.
    pub store: &'a SubsonicStore,
    /// Audio over the stream gateway, scoped to the caller.
    pub audio: &'a CompatAudio,
    /// Server settings.
    pub settings: &'a Settings,
    /// Requested format.
    pub format: SubsonicFormat,
    /// JSONP callback (raw; validated at render).
    pub callback: Option<String>,
    /// Uppercase HTTP method.
    pub method: String,
    /// Range header, if any.
    pub range: Option<String>,
    /// Request content type.
    pub content_type: Option<String>,
    /// Raw body.
    pub body: Vec<u8>,
    /// Current time, unix seconds (fixture clock in tests).
    pub now_unix: f64,
    /// The address the app reached this server at, for picture links.
    pub picture_base: Option<String>,
}

impl Ctx<'_> {
    /// Single param.
    pub fn p(&self, key: &str) -> Result<Option<String>, SubsonicError> {
        self.params.string(key, None)
    }

    /// Repeated param.
    pub fn plist(&self, key: &str) -> Result<Vec<String>, SubsonicError> {
        self.params.strings(key)
    }

    /// Integer param.
    pub fn pint(
        &self,
        key: &str,
        default: Option<i64>,
        minimum: Option<i64>,
        maximum: Option<i64>,
    ) -> Result<Option<i64>, SubsonicError> {
        self.params.integer(key, default, minimum, maximum)
    }

    /// Float param.
    pub fn pfloat(
        &self,
        key: &str,
        default: Option<f64>,
        minimum: Option<f64>,
        maximum: Option<f64>,
    ) -> Result<Option<f64>, SubsonicError> {
        self.params.number(key, default, minimum, maximum)
    }

    /// Boolean param.
    pub fn pbool(&self, key: &str, default: bool) -> Result<bool, SubsonicError> {
        self.params.boolean(key, default)
    }

    /// Authenticated principal (handlers other than the public one).
    pub fn user(&self) -> Result<&CompatPrincipal, SubsonicError> {
        self.principal
            .as_ref()
            .ok_or_else(|| SubsonicError::code_only(PARAM_MISSING))
    }

    /// Song child with this request's transcode hint.
    pub fn child(&self, track: &views::ViewTrack) -> models::SChild {
        let hint = self.settings.transcode_hint();
        match &hint {
            Some((content_type, suffix)) => {
                views::to_child(track, Some((content_type.as_str(), suffix.as_str())))
            }
            None => views::to_child(track, None),
        }
    }

    /// The open link to an album or artist picture the catalog just
    /// returned, with an album's cover version, or `None` when the request
    /// named no address: a link is absolute or not sent.
    pub fn picture_link(
        &self,
        kind: picture::PictureKind,
        internal_id: &str,
        size: u32,
        cover_version: Option<&str>,
    ) -> Option<String> {
        let base = self.picture_base.as_deref()?;
        Some(picture::link(
            base,
            self.store.pictures(),
            kind,
            internal_id,
            size,
            cover_version,
        ))
    }

    /// ID3 artist with its picture link.
    pub fn artist_id3(&self, artist: &views::ViewArtist) -> models::SArtistID3 {
        views::to_artist_id3(artist, self.artist_image_url(artist))
    }

    /// File-structure artist with its picture link.
    pub fn artist_file(&self, artist: &views::ViewArtist) -> models::SArtist {
        views::to_artist_file(artist, self.artist_image_url(artist))
    }

    fn artist_image_url(&self, artist: &views::ViewArtist) -> Option<String> {
        self.picture_link(
            picture::PictureKind::Artist,
            &artist.artist_mbid,
            picture::ARTIST_IMAGE_SIZE,
            None,
        )
    }

    /// Storage errors surface as code 0 (generic), never an auth code. The
    /// client sees only the fixed internal message, under 503 with
    /// `Retry-After` when the database was busy and 500 otherwise; the
    /// request boundary logs the cause once.
    pub fn store_err(err: CompatError) -> SubsonicError {
        if let Some((code, message)) = err.client_error() {
            return SubsonicError::new(code, message);
        }
        match err {
            CompatError::Busy(cause) => {
                SubsonicError::server_fault(error::ServerFault::Busy(cause))
            }
            other => store_failure(other),
        }
    }
}

/// Answer a storage failure as code 0 with the fixed message.
fn store_failure(err: impl std::fmt::Display) -> SubsonicError {
    SubsonicError::server_fault(error::ServerFault::Internal(err.to_string()))
}

/// The status, `Retry-After` and request-boundary fault of a server fault,
/// as the kernel's error answers it: busy is the closed-list 503, anything
/// else 500. The boundary logs the fault once, with its cause.
fn fault_answer(
    fault: error::ServerFault,
) -> (
    u16,
    Option<String>,
    Option<droppedneedle_kernel::error::Fault>,
) {
    use axum::response::IntoResponse as _;
    use droppedneedle_kernel::error::{ApiError, Fault, Unavailable};
    let error = match fault {
        error::ServerFault::Busy(cause) => ApiError::unavailable(Unavailable::ServerBusy, cause),
        error::ServerFault::Internal(cause) => ApiError::internal(cause),
    };
    let mut response = error.into_response();
    let retry_after = response
        .headers()
        .get(axum::http::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (
        response.status().as_u16(),
        retry_after,
        response.extensions_mut().remove::<Fault>(),
    )
}

/// Handler outcome: an envelope payload or a binary response.
pub enum Outcome {
    /// `ok` envelope, with the answer under its key unless bare.
    Envelope(Option<(&'static str, Payload)>),
    /// Binary bytes with status, content type, and headers.
    Binary {
        /// HTTP status.
        status: u16,
        /// Content type.
        content_type: String,
        /// Extra headers.
        headers: Vec<(String, String)>,
        /// Body bytes.
        body: Vec<u8>,
    },
    /// Audio streamed as it is read, with status, content type, and
    /// headers.
    Stream {
        /// HTTP status.
        status: u16,
        /// Content type.
        content_type: String,
        /// Extra headers.
        headers: Vec<(String, String)>,
        /// The live body.
        body: crate::body::LiveBody,
    },
}

impl Outcome {
    /// Bare-ok envelope (no response key).
    pub fn ok() -> Self {
        Outcome::Envelope(None)
    }

    /// Envelope with the answer under `key`, serialized now.
    pub fn keyed(key: &'static str, answer: impl Serialize) -> Result<Self, SubsonicError> {
        Ok(Outcome::Envelope(Some((key, value::payload(&answer)?))))
    }
}

/// Run a method and render its outcome in the request's format.
pub async fn run(endpoint: &Endpoint, ctx: &Ctx<'_>) -> Result<Rendered, SubsonicError> {
    match (endpoint.handler)(ctx).await? {
        Outcome::Envelope(answer) => render_ok(
            answer.as_ref().map(|(key, payload)| (*key, &**payload)),
            ctx.format,
            ctx.callback.as_deref(),
            &ctx.settings.server_name,
            &ctx.settings.server_version,
        ),
        Outcome::Binary {
            status,
            content_type,
            headers,
            body,
        } => Ok(Rendered {
            status,
            content_type,
            headers,
            body,
            stream: None,
        }),
        Outcome::Stream {
            status,
            content_type,
            headers,
            body,
        } => Ok(Rendered {
            status,
            content_type,
            headers,
            body: Vec::new(),
            stream: Some(body),
        }),
    }
}

/// Answer a failure through the binary-vs-envelope split. A server fault
/// keeps its 5xx status and `Retry-After` under the failed envelope, and
/// comes back beside it for the request boundary to log.
pub fn failure(
    err: SubsonicError,
    normalized_endpoint: &str,
    format: SubsonicFormat,
    callback: Option<&str>,
    settings: &Settings,
) -> (Rendered, Option<droppedneedle_kernel::error::Fault>) {
    let mut rendered = if dispatch_uses_envelope(err.code, normalized_endpoint) {
        render_error(
            err.code,
            &err.message,
            format,
            callback,
            &settings.server_name,
            &settings.server_version,
        )
    } else {
        render_binary_error(err.code, &err.message)
    };
    let Some(fault) = err.fault else {
        return (rendered, None);
    };
    let (status, retry_after, fault) = fault_answer(fault);
    rendered.status = status;
    if let Some(seconds) = retry_after {
        rendered.headers.push(("Retry-After".to_owned(), seconds));
    }
    (rendered, fault)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_errors_never_reach_the_client() {
        let error = store_failure("disk I/O error at /srv/db/droppedneedle.sqlite");
        assert_eq!(error.code, GENERIC);
        assert_eq!(
            error.message,
            droppedneedle_kernel::error::FIXED_INTERNAL_MESSAGE
        );
    }
}
