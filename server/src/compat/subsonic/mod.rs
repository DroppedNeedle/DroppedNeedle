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
//! code 0. Only `getOpenSubsonicExtensions` is public; everything else,
//! binary endpoints included, needs app-password auth. Formats: `f` in
//! `xml` (default), `json`, `jsonp` (unsafe callbacks fall back to JSON).
//!
//! | Endpoint | Key | Auth | Notes |
//! |---|---|---|---|
//! | ping | ok | yes | auth probe |
//! | getLicense | license | yes | static valid:true |
//! | getOpenSubsonicExtensions | openSubsonicExtensions | PUBLIC | 3 exts x v1 |
//! | getMusicFolders | musicFolders | yes | only folder 1; others 70 |
//! | getArtists/getIndexes | artists/indexes | yes | article-stripped A-Z buckets |
//! | getArtist/getAlbum/getSong | artist/album/song | yes | artist embeds albums, album embeds songs |
//! | getAlbumList2/getAlbumList | albumList2/albumList | yes | `highest` -> 0; byYear needs from+to |
//! | getRandomSongs | randomSongs | yes | filters |
//! | getMusicDirectory | directory | yes | 1 -> artists; artist -> albums; album -> songs |
//! | search3/search2 | searchResult3/2 | yes | missing/empty/`""` query = match-all |
//! | getCoverArt | bytes | yes | RG art/artist image/playlist file; miss -> SVG; size buckets |
//! | stream/download | bytes | yes | decide() policy; ranges; 416; HEAD |
//! | getTranscodeDecision | transcodeDecision | yes | POST-only, JSON-only |
//! | getTranscodeStream | bytes | yes | signed params |
//! | getPlaylists/getPlaylist | playlists/playlist | yes | streamable-only counts (#181) |
//! | createPlaylist | playlist | yes | with playlistId = replace |
//! | updatePlaylist/deletePlaylist | ok | yes | rename/public/add/remove-by-index |
//! | star/unstar | ok | yes | prefix-routed, deduped; empty -> 10 |
//! | getStarred2/getStarred | starred2/starred | yes | ID3 vs file shapes |
//! | setRating | ok | yes | validated no-op |
//! | scrobble | ok | yes | parallel time[]; submission=false -> now-playing |
//! | getNowPlaying | nowPlaying | yes | presence |
//! | reportPlayback | ok | yes | GET/form/JSON; extension playbackReport:1 |
//! | getPlayQueue/savePlayQueue | playQueue/ok | yes | <=500; current consistency |
//! | getPlayQueueByIndex/savePlayQueueByIndex | playQueueByIndex/ok | yes | extension indexBasedQueue:1 |
//! | getBookmarks/createBookmark/deleteBookmark | bookmarks/ok | yes | pinned bookmark fields |
//! | getArtistInfo[2]/getAlbumInfo[2] | artistInfo[2]/albumInfo[2] | yes | projected MBIDs + sized cover URLs |
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
//! - Streamable-only playlist counts (#181).
//! - getAvatar 403-as-text for non-self usernames: the only 403-as-text;
//!   code 50 anywhere in the dispatch path stays enveloped.
//! - Binary-vs-envelope split: binary dispatch errors outside
//!   {10,40,41,42,43,44,50} render `text/plain` (70 -> 404, else 404).
//! - Transcode hints on song children only when transcoding is enabled
//!   and ffmpeg is present.
//! - Served-but-unadvertised extensions (songLyrics, playbackReport,
//!   indexBasedQueue, transcoding, all v1) stay out of
//!   getOpenSubsonicExtensions (the matrix wins over the router: 3
//!   advertised, `transcoding` served but unadvertised).
//!
//! # Seams (implemented elsewhere, bound in `compat::setup`)
//!
//! - Auth: [`auth::Credentials`] classification here; secret verification
//!   lives in `auth::compat_auth::subsonic::authenticate`, and [`Verifier`]
//!   delegates to it.
//! - Data: [`store::Store`] (library, playlists, favorites, scrobble,
//!   queues, bookmarks, lyrics, avatars, scan, cover art, advanced
//!   transcode).
//! - Audio bytes: [`stream::AudioBackend`] on the stream engine
//!   (ranges, HEAD, leases per caller).
//! - Edge (`compat::shared`): enablement kill-switch default, rate limits,
//!   CORS, case-insensitive paths, access-log redaction. The enablement
//!   gate itself (`enabled` in [`Settings`]) runs here before lookup.

pub mod advanced;
pub mod auth;
pub mod browse;
pub mod error;
pub mod ids;
pub mod library;
pub mod media;
pub mod models;
pub mod params;
pub mod store;
pub mod stream;
pub mod value;
pub mod views;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

use std::collections::HashMap;

// Seam surface: public for the HTTP adapter.
pub use auth::{Credentials, Principal, classify};
pub use error::{GENERIC, PARAM_MISSING, SubsonicError};
pub use params::SubsonicParameters;
pub use store::Store;
pub use stream::AudioBackend;
pub use value::{
    Rendered, SubsonicFormat, parse_format, render_binary_error, render_error, render_ok,
};

/// Binary endpoints (normalized names): dispatch-path errors outside
/// the auth codes render `text/plain` (v2 `_BINARY`).
pub const BINARY_ENDPOINTS: &[&str] = &[
    "stream",
    "download",
    "getcoverart",
    "getavatar",
    "gettranscodestream",
];

/// Codes that stay enveloped on the binary dispatch path (v2 `_AUTH_CODES`).
pub const AUTH_CODES: &[u8] = &[10, 40, 41, 42, 43, 44, 50];

/// The one public endpoint (normalized name, v2 `_PUBLIC`).
pub const PUBLIC_ENDPOINT: &str = "getopensubsonicextensions";

/// getAvatar refusal for a non-self username, v2 message verbatim.
pub const AVATAR_FORBIDDEN_MESSAGE: &str = "Avatar access is limited to the authenticated user";

/// Normalize an endpoint name: casefold, strip one `.view` suffix
/// (v2 `_dispatch`).
pub fn normalize_endpoint(raw: &str) -> String {
    let folded = raw.to_lowercase();
    folded.strip_suffix(".view").unwrap_or(&folded).to_owned()
}

/// True for the five binary endpoints (takes a normalized name).
pub fn is_binary_endpoint(normalized: &str) -> bool {
    BINARY_ENDPOINTS.contains(&normalized)
}

/// Dispatch rule: binary endpoints render codes outside [`AUTH_CODES`]
/// as `text/plain`; everything else stays enveloped.
pub fn dispatch_uses_envelope(code: u8, normalized_endpoint: &str) -> bool {
    !is_binary_endpoint(normalized_endpoint) || AUTH_CODES.contains(&code)
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
    /// Public base URL (for credential-free cover URLs).
    pub base_url: String,
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
            base_url: "http://localhost".to_owned(),
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

/// One inbound request (the HTTP adapter builds this).
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Uppercase HTTP method (`GET`/`POST`/`HEAD`).
    pub method: String,
    /// Raw endpoint path segment (may carry `.view`, any case).
    pub endpoint: String,
    /// Decoded query+form params (JSON bodies pre-flattened for reportPlayback).
    pub params: SubsonicParameters,
    /// Request headers (only `Range` and `Content-Type` are read).
    pub headers: HashMap<String, String>,
    /// Raw body (getTranscodeDecision JSON).
    pub body: Vec<u8>,
    /// Request content type.
    pub content_type: Option<String>,
    /// Clock override, unix seconds. `None` reads the system clock;
    /// tests stamp the fixture clock for deterministic time fields.
    pub now_unix: Option<f64>,
}

impl Request {
    /// Header lookup, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Handler context: everything one endpoint call needs.
pub struct Ctx<'a, P, S, B> {
    /// Normalized endpoint name.
    pub endpoint_name: String,
    /// Decoded params.
    pub params: SubsonicParameters,
    /// Authenticated principal (None only for the public endpoint).
    pub principal: Option<P>,
    /// Data store.
    pub store: &'a S,
    /// Audio backend.
    pub audio: &'a B,
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
}

impl<P: Principal, S: Store, B: AudioBackend> Ctx<'_, P, S, B> {
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
    pub fn user(&self) -> Result<&P, SubsonicError> {
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

    /// Advertised getCoverArt URL: base URL plus the fixed route, sized,
    /// never carrying a credential (v2 `cover_art_url`).
    pub fn cover_art_url(&self, cover_id: &str, size: i64) -> String {
        format!(
            "{}/subsonic/rest/getCoverArt?id={cover_id}&size={size}",
            self.settings.base_url.trim_end_matches('/')
        )
    }

    /// Storage errors surface as code 0 (generic), never an auth code. The
    /// cause is logged; the client sees only the fixed internal message.
    pub fn store_err<E: std::fmt::Display>(err: E) -> SubsonicError {
        store_failure(err)
    }
}

/// Log a storage failure and answer code 0 with the fixed message.
fn store_failure<E: std::fmt::Display>(err: E) -> SubsonicError {
    tracing::error!(cause = %err, "subsonic store call failed");
    SubsonicError::new(GENERIC, crate::error::FIXED_INTERNAL_MESSAGE)
}

/// Handler outcome: an envelope payload or a binary response.
pub enum Outcome {
    /// `ok` envelope with an optional response key + payload.
    Envelope {
        /// Response key (`None` for bare-ok endpoints).
        key: Option<&'static str>,
        /// Payload value.
        payload: Option<value::Val>,
    },
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
}

impl Outcome {
    /// Bare-ok envelope (no response key).
    pub fn ok() -> Self {
        Outcome::Envelope {
            key: None,
            payload: None,
        }
    }

    /// Envelope with a response key + payload.
    pub fn keyed(key: &'static str, payload: value::Val) -> Self {
        Outcome::Envelope {
            key: Some(key),
            payload: Some(payload),
        }
    }
}

/// Secret verifier. Implemented by delegating to
/// `auth::compat_auth::subsonic::authenticate` and adapting the result.
pub trait Verifier: Clone + Send + Sync {
    /// Principal type the handlers see.
    type Principal: Principal;

    /// Verify classified credentials against the app-password store.
    fn verify(
        &self,
        credentials: &Credentials,
    ) -> impl Future<Output = Result<Self::Principal, SubsonicError>> + Send;
}

/// Dispatch one request: normalize, parse `f`, gate enablement, resolve
/// the handler, authenticate (except the public endpoint), run, and
/// render. Failures map through the binary-vs-envelope split (v2
/// `_dispatch` boundary: nothing escapes this function as a panic).
pub async fn dispatch<V: Verifier, S: Store, B: AudioBackend>(
    verifier: &V,
    store: &S,
    audio: &B,
    settings: &Settings,
    request: &Request,
) -> Rendered {
    dispatch_with(verifier, store, audio, settings, request, None).await
}

/// [`dispatch`] with a pre-verified principal. The HTTP adapter verifies
/// before dispatching so it can enforce the per-principal rate buckets and
/// record auth failures without verifying twice; `None` verifies inline
/// exactly like [`dispatch`]. A pre-verified principal is used only for
/// non-public endpoints and never skips the enablement gate.
pub async fn dispatch_with<V: Verifier, S: Store, B: AudioBackend>(
    verifier: &V,
    store: &S,
    audio: &B,
    settings: &Settings,
    request: &Request,
    preverified: Option<V::Principal>,
) -> Rendered {
    let name = normalize_endpoint(&request.endpoint);
    let is_binary = is_binary_endpoint(&name);
    // Early format sniffing preserves the requested envelope even when
    // strict decoding fails (v2 reads a unique valid query value only).
    let mut format = SubsonicFormat::Xml;
    let mut callback: Option<String> = None;
    if request.params.all("f").len() == 1
        && let Some(sniffed) = request.params.all("f").into_iter().next()
        && matches!(sniffed, "xml" | "json" | "jsonp")
    {
        format = parse_format(Some(sniffed)).unwrap_or(SubsonicFormat::Xml);
    }
    if request.params.all("callback").len() == 1
        && let Some(sniffed) = request.params.all("callback").into_iter().next()
        && sniffed.len() <= 128
    {
        callback = Some(sniffed.to_owned());
    }
    let failure =
        |code: u8, message: &str, format: SubsonicFormat, callback: Option<&str>| -> Rendered {
            if is_binary && !AUTH_CODES.contains(&code) {
                render_binary_error(code, message)
            } else {
                render_error(
                    code,
                    message,
                    format,
                    callback,
                    &settings.server_name,
                    &settings.server_version,
                )
            }
        };
    let outcome: Result<Rendered, SubsonicError> = async {
        let requested = request.params.string_max("f", Some("xml"), 16)?;
        let parsed = parse_format(requested.as_deref())?;
        format = parsed;
        callback = request.params.string_max("callback", None, 128)?;
        // Gate before handler lookup: a disabled API must not leak
        // method existence (v2 `ensure_subsonic_enabled`).
        if !settings.enabled {
            return Err(SubsonicError::new(
                0,
                "The Subsonic API is disabled on this server.",
            ));
        }
        if !is_known_endpoint(&name) {
            return Err(SubsonicError::new(0, format!("Unknown method {name}")));
        }
        let principal = if name == PUBLIC_ENDPOINT {
            None
        } else if let Some(preverified) = preverified {
            Some(preverified)
        } else {
            let credentials = classify(&request.params)?
                .ok_or_else(|| SubsonicError::code_only(PARAM_MISSING))?;
            Some(verifier.verify(&credentials).await?)
        };
        let scoped = principal
            .as_ref()
            .map(|principal| store.for_caller(principal.user_id()));
        let scoped_audio = principal
            .as_ref()
            .map(|principal| audio.for_caller(principal.user_id()));
        let ctx = Ctx {
            endpoint_name: name.clone(),
            params: request.params.clone(),
            principal,
            store: scoped.as_ref().unwrap_or(store),
            audio: scoped_audio.as_ref().unwrap_or(audio),
            settings,
            format,
            callback: callback.clone(),
            method: request.method.clone(),
            range: request.header("Range").map(str::to_owned),
            content_type: request.content_type.clone(),
            body: request.body.clone(),
            now_unix: request.now_unix.unwrap_or_else(live_now_unix),
        };
        run_handler(&ctx).await
    }
    .await;
    match outcome {
        Ok(rendered) => rendered,
        Err(err) => failure(err.code, &err.message, format, callback.as_deref()),
    }
}

/// Render an outcome in the request's format.
pub fn render_outcome(
    outcome: Outcome,
    ctx: &Ctx<impl Principal, impl Store, impl AudioBackend>,
) -> Rendered {
    match outcome {
        Outcome::Envelope { key, payload } => render_ok(
            key,
            payload,
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
        } => Rendered {
            status,
            content_type,
            headers,
            body,
        },
    }
}

/// System clock, unix seconds (0 when the clock reads before the epoch).
pub fn live_now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// True when the normalized name has a handler.
pub fn is_known_endpoint(name: &str) -> bool {
    matches!(
        name,
        "ping"
            | "getlicense"
            | "getopensubsonicextensions"
            | "getmusicfolders"
            | "getartists"
            | "getindexes"
            | "getartist"
            | "getalbum"
            | "getsong"
            | "getalbumlist2"
            | "getalbumlist"
            | "getrandomsongs"
            | "getmusicdirectory"
            | "search3"
            | "search2"
            | "getcoverart"
            | "stream"
            | "download"
            | "gettranscodedecision"
            | "gettranscodestream"
            | "getplaylists"
            | "getplaylist"
            | "createplaylist"
            | "updateplaylist"
            | "deleteplaylist"
            | "star"
            | "unstar"
            | "getstarred2"
            | "getstarred"
            | "setrating"
            | "scrobble"
            | "getnowplaying"
            | "reportplayback"
            | "getplayqueue"
            | "saveplayqueue"
            | "getplayqueuebyindex"
            | "saveplayqueuebyindex"
            | "getbookmarks"
            | "createbookmark"
            | "deletebookmark"
            | "getartistinfo2"
            | "getartistinfo"
            | "getalbuminfo2"
            | "getalbuminfo"
            | "getavatar"
            | "getlyricsbysongid"
            | "getlyrics"
            | "getgenres"
            | "getsongsbygenre"
            | "getuser"
            | "getscanstatus"
            | "startscan"
            | "gettopsongs"
            | "getsimilarsongs2"
            | "getsimilarsongs"
    )
}

/// Route to the endpoint handler (v2 `_HANDLERS`).
async fn run_handler<P: Principal, S: Store, B: AudioBackend>(
    ctx: &Ctx<'_, P, S, B>,
) -> Result<Rendered, SubsonicError> {
    let outcome = match ctx.endpoint_name.as_str() {
        "ping" => browse::ping(ctx).await?,
        "getlicense" => browse::license(ctx).await?,
        "getopensubsonicextensions" => browse::extensions(ctx).await?,
        "getmusicfolders" => browse::music_folders(ctx).await?,
        "getartists" => browse::artists(ctx).await?,
        "getindexes" => browse::indexes(ctx).await?,
        "getartist" => browse::artist(ctx).await?,
        "getalbum" => browse::album(ctx).await?,
        "getsong" => browse::song(ctx).await?,
        "getalbumlist2" => browse::album_list2(ctx).await?,
        "getalbumlist" => browse::album_list(ctx).await?,
        "getrandomsongs" => browse::random_songs(ctx).await?,
        "getmusicdirectory" => browse::music_directory(ctx).await?,
        "search3" => browse::search3(ctx).await?,
        "search2" => browse::search2(ctx).await?,
        "getcoverart" => media::cover_art(ctx).await?,
        "stream" => media::stream(ctx).await?,
        "download" => media::download(ctx).await?,
        "gettranscodedecision" => media::transcode_decision(ctx).await?,
        "gettranscodestream" => media::transcode_stream(ctx).await?,
        "getplaylists" => library::playlists(ctx).await?,
        "getplaylist" => library::playlist(ctx).await?,
        "createplaylist" => library::create_playlist(ctx).await?,
        "updateplaylist" => library::update_playlist(ctx).await?,
        "deleteplaylist" => library::delete_playlist(ctx).await?,
        "star" => library::star(ctx, true).await?,
        "unstar" => library::star(ctx, false).await?,
        "getstarred2" => library::starred2(ctx).await?,
        "getstarred" => library::starred(ctx).await?,
        "setrating" => library::set_rating(ctx).await?,
        "scrobble" => library::scrobble(ctx).await?,
        "getnowplaying" => library::now_playing(ctx).await?,
        "reportplayback" => library::report_playback(ctx).await?,
        "getplayqueue" => library::get_play_queue(ctx).await?,
        "saveplayqueue" => library::save_play_queue(ctx).await?,
        "getplayqueuebyindex" => library::get_play_queue_by_index(ctx).await?,
        "saveplayqueuebyindex" => library::save_play_queue_by_index(ctx).await?,
        "getbookmarks" => library::get_bookmarks(ctx).await?,
        "createbookmark" => library::create_bookmark(ctx).await?,
        "deletebookmark" => library::delete_bookmark(ctx).await?,
        "getartistinfo2" | "getartistinfo" => library::artist_info(ctx).await?,
        "getalbuminfo2" | "getalbuminfo" => library::album_info(ctx).await?,
        "getavatar" => media::avatar(ctx).await?,
        "getlyricsbysongid" => library::lyrics_by_song_id(ctx).await?,
        "getlyrics" => library::lyrics(ctx).await?,
        "getgenres" => library::genres(ctx).await?,
        "getsongsbygenre" => library::songs_by_genre(ctx).await?,
        "getuser" => library::user(ctx).await?,
        "getscanstatus" => library::scan_status(ctx).await?,
        "startscan" => library::start_scan(ctx).await?,
        "gettopsongs" => library::top_songs(ctx).await?,
        "getsimilarsongs2" => library::similar_songs2(ctx).await?,
        "getsimilarsongs" => library::similar_songs(ctx).await?,
        _ => {
            return Err(SubsonicError::new(
                0,
                format!("Unknown method {}", ctx.endpoint_name),
            ));
        }
    };
    Ok(render_outcome(outcome, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_errors_never_reach_the_client() {
        let error = store_failure("disk I/O error at /srv/db/droppedneedle.sqlite");
        assert_eq!(error.code, GENERIC);
        assert_eq!(error.message, crate::error::FIXED_INTERNAL_MESSAGE);
    }
}
