//! Stream-gateway routes: one source-keyed GET/HEAD surface over the engine.
//!
//! This file owns the HTTP layer only: route shapes, the typed query, range
//! parsing, byte-exact 200/206/416/HEAD framing for every stream route, and
//! the error mapping. Byte sourcing, leases, and transcode execution live in
//! the engine behind the [`StreamEngine`] seam below.
//!
//! # Routes (relative; `MediaSetup` nests these under `/api/v3`)
//!
//! ```text
//! GET  /stream/{source}/{*key}   full or ranged audio bytes
//! HEAD /stream/{source}/{*key}   same headers, no body
//! ```
//!
//! `source` is one of `local`, `jellyfin`, `navidrome`, `plex`. `key` is the
//! source identifier: a local file id, a Jellyfin/Navidrome item id, or a Plex
//! part key (which may itself contain slashes, hence the wildcard). Query
//! keys are `format`, `max_bitrate`, `start_seconds` and
//! `estimate_content_length` (see [`StreamQuery`]).
//!
//! # Deep links
//!
//! Players link to `/api/v3/stream/{source}/{key}` with those query
//! keys. The shapes stay small and stable: one handler shape
//! `(State, User, Path, HeaderMap, ValidQuery)`, path params only
//! for identity, query only for transcode hints. A new gateway capability
//! should be a new query key, not a new route.
//!
//! # Seams
//!
//! * Engine: [`StreamEngine`] is the engine seam; [`super::gateway::Gateway`]
//!   implements it (leases + `Transcoder` + direct/transcode reads).
//! * Streaming reads: routes call [`StreamEngine::open_stream`], which
//!   hands back a [`MediaBody`]: a local file read by range on demand, a
//!   transcode read chunk by chunk, or in-memory bytes. HEAD never reads a
//!   body and never starts ffmpeg.
//! * Framing: [`frame`] turns one opened object, the `Range` header and
//!   HEAD into status, headers and body. The native, Subsonic and Jellyfin
//!   stream routes and the track download all frame through it or through
//!   [`DirectSpan`], so ranges, lengths and HEAD answer the same everywhere;
//!   each protocol keeps only its own error body.
//! * Mounting: [`routes`] mounts [`stream_get_route`] and
//!   [`stream_head_route`], the production forms of the generic handlers,
//!   for any signed-in user. Playback
//!   reporting (`start`/`progress`/`stop`/`scrobble`/`now-playing`/`stopped`)
//!   lives in `playback`, not this router.
//! * [`content_type_for_extension`] is the one audio content-type table
//!   (v2's minus WMA, which v3 does not support). The engine resolves the
//!   final content type (upstream wins for remotes); local reads go through
//!   this table.
//!
//! # Ranges
//!
//! One `bytes=start-end` range only (suffix and open forms included), end
//! clamped to size-1, suffix longer than the file clamped to the whole
//! file. Anything else, a multi-range request included, is 416 with
//! `Content-Range: bytes */N`. A 429 carries `Retry-After: 1`.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;

use axum::{
    body::Body,
    extract::{FromRef, Path, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{self},
    },
    response::{IntoResponse, Response},
};
use droppedneedle_auth::{Principal, User};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_kernel::mount::{Routes, routes};

use super::leases::RETRY_AFTER_SECONDS;
use crate::MediaEngine;
use serde::Deserialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// Error codes (SCREAMING_SNAKE, shared envelope)
// ---------------------------------------------------------------------------

/// Unknown file/item id. Message follows the v2 404 detail per source.
pub const STREAM_NOT_FOUND: &str = "STREAM_NOT_FOUND";
/// Playback refused, or a local path outside the music directory.
pub const STREAM_FORBIDDEN: &str = "STREAM_FORBIDDEN";
/// Range header present but unsatisfiable. Carries `Content-Range: bytes */N`.
pub const RANGE_NOT_SATISFIABLE: &str = "RANGE_NOT_SATISFIABLE";
/// Direct or transcode leases exhausted. Carries `Retry-After: 1`.
pub const STREAM_CAPACITY_EXHAUSTED: &str = "STREAM_CAPACITY_EXHAUSTED";

// ---------------------------------------------------------------------------
// Audio sources
// ---------------------------------------------------------------------------

/// The stream sources behind one gateway. Display names feed the logged
/// cause of a failed read only; wire identity is the lowercase route
/// segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioSource {
    /// Local library file bytes.
    Local,
    /// Jellyfin proxied audio.
    Jellyfin,
    /// Navidrome proxied audio.
    Navidrome,
    /// Plex proxied audio (part keys may contain slashes).
    Plex,
    /// A `streaming_source` plugin; the key is a recording MBID.
    Plugin,
}

impl AudioSource {
    /// Parse the `{source}` path segment. Case-insensitive; `None` rejects.
    pub fn parse(segment: &str) -> Option<Self> {
        match segment.to_lowercase().as_str() {
            "local" => Some(Self::Local),
            "jellyfin" => Some(Self::Jellyfin),
            "navidrome" => Some(Self::Navidrome),
            "plex" => Some(Self::Plex),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }

    /// Route segment spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Jellyfin => "jellyfin",
            Self::Navidrome => "navidrome",
            Self::Plex => "plex",
            Self::Plugin => "plugin",
        }
    }

    /// Human name for the logged cause of a failed read.
    fn display_name(self) -> &'static str {
        match self {
            Self::Local => "local file",
            Self::Jellyfin => "Jellyfin",
            Self::Navidrome => "Navidrome",
            Self::Plex => "Plex",
            Self::Plugin => "plugin",
        }
    }
}

// ---------------------------------------------------------------------------
// Engine seam (implemented by `super::gateway::Gateway`)
// ---------------------------------------------------------------------------

/// What a client asked of one stream, in every protocol's terms: the
/// native query maps 1:1, and the Subsonic and Jellyfin routes translate
/// their own parameter names into it. The engine's `decide()` reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamParams {
    /// Requested output codec (`mp3`, `opus`, `raw`, or unset). `raw`
    /// always serves the original bytes.
    pub format: Option<String>,
    /// Client bitrate cap in kbps. 0 or unset means uncapped.
    pub max_bitrate_kbps: Option<i64>,
    /// Ask a transcode landing for its estimated `Content-Length`.
    pub estimate_content_length: bool,
    /// Seek offset in seconds (`-ss`, transcode only).
    pub start_seconds: f64,
}

impl StreamParams {
    /// The original bytes, whatever the transcode policy says.
    pub fn original() -> Self {
        Self {
            format: Some("raw".to_owned()),
            ..Self::default()
        }
    }
}

/// One engine read: identity, lease principal, and transcode hints.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamOpen {
    /// Which source backend to read from.
    pub source: AudioSource,
    /// File id, item id, or Plex part key.
    pub key: String,
    /// Lease principal (the session user id).
    pub user_id: String,
    /// Transcode hints from the query string.
    pub params: StreamParams,
}

/// Body chunks of a streamed response.
pub type ChunkStream = BoxStream<'static, Result<Vec<u8>, std::io::Error>>;

/// Opens `len` bytes from `start` of a remote object. The stream is lazy:
/// nothing goes upstream until the response body is first polled.
pub type SpanOpener = Box<dyn FnOnce(u64, u64) -> ChunkStream + Send>;

/// Where the response body comes from.
pub enum MediaBody {
    /// The whole object in memory.
    Bytes(Vec<u8>),
    /// A local file, read by range when the response streams.
    File(PathBuf),
    /// Live transcode output, chunk by chunk.
    Chunks(ChunkStream),
    /// A sized remote object opened by span on demand (plugin URLs), so a
    /// seek asks the upstream for just that range.
    Spans(SpanOpener),
    /// Nothing to send: HEAD of a transcode, answered without ffmpeg.
    Empty,
}

impl std::fmt::Debug for MediaBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::File(path) => write!(f, "File({})", path.display()),
            Self::Chunks(_) => f.write_str("Chunks"),
            Self::Spans(_) => f.write_str("Spans"),
            Self::Empty => f.write_str("Empty"),
        }
    }
}

/// One opened media object for the stream routes.
#[derive(Debug)]
pub struct StreamMedia {
    /// Engine-resolved content type.
    pub content_type: String,
    /// Exact object length for direct reads; unknown (0) for transcodes.
    pub total_len: u64,
    /// Transcode landing: ranges are refused, never honored.
    pub transcoded: bool,
    /// Estimated length for the optional transcode `Content-Length`.
    pub estimated_len: Option<u64>,
    /// The body source.
    pub body: MediaBody,
    /// The direct lease the open took, when the engine hands it back.
    /// Holding it until the body ends bounds response concurrency.
    pub lease: Option<super::leases::OwnedDirectLease>,
}

/// Engine failures, mapped to the wire in [`serve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFault {
    /// Unknown id.
    NotFound,
    /// Playback refused or path outside the music directory. The message is
    /// user-facing v2 detail, never a path.
    Forbidden {
        /// What was refused, e.g. "Playback not allowed".
        message: String,
    },
    /// Unsupported audio format or bad key. User-facing.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Direct or transcode leases exhausted. Routes answer 429 + Retry-After.
    Capacity,
    /// The source failed the read.
    Upstream {
        /// Which source failed.
        source: AudioSource,
        /// Log-only cause.
        cause: String,
    },
    /// Server fault. The cause reaches the log only, never the wire.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

/// The engine the routes read from: open one media object or fail with a
/// [`StreamFault`]. [`super::gateway::Gateway`] is the production
/// implementation.
pub trait StreamEngine: Send + Sync {
    /// Resolve one media object for streaming. `head_only` asks for the
    /// metadata alone.
    fn open_stream(
        &self,
        request: StreamOpen,
        head_only: bool,
    ) -> impl Future<Output = Result<StreamMedia, StreamFault>> + Send;
}

// ---------------------------------------------------------------------------
// Content types (v2 table minus WMA, which never streams)
// ---------------------------------------------------------------------------

/// Every audio format that streams, by extension, with its content type.
const AUDIO_TYPES: [(&str, &str); 7] = [
    ("flac", "audio/flac"),
    ("mp3", "audio/mpeg"),
    ("ogg", "audio/ogg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("wav", "audio/wav"),
    ("opus", "audio/opus"),
];

/// Content type for an audio file extension, e.g. `.flac` or `flac`.
/// Case-insensitive. `None` means the format never streams and the engine
/// answers [`StreamFault::InvalidInput`].
pub fn content_type_for_extension(extension: &str) -> Option<&'static str> {
    let extension = extension.trim_start_matches('.');
    AUDIO_TYPES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map(|(_, content_type)| *content_type)
}

/// The extension of a content type from [`content_type_for_extension`].
pub fn extension_for_content_type(content_type: &str) -> Option<&'static str> {
    AUDIO_TYPES
        .iter()
        .find(|(_, known)| *known == content_type)
        .map(|(extension, _)| *extension)
}

// ---------------------------------------------------------------------------
// Range parsing (v2 `stream_track` rules, byte-exact)
// ---------------------------------------------------------------------------

/// Inclusive resolved byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    /// First byte offset.
    start: u64,
    /// Last byte offset, inclusive.
    end: u64,
}

/// Parse a single `Range` header against `total_len`, v2 rule for rule:
/// `bytes=start-end` (end clamped), `bytes=start-` (open), `bytes=-suffix`
/// (clamped to the whole file when longer). `None` means 416: no match,
/// both bounds empty, suffix of 0, start past the end, or an empty object.
/// Multi-range and unit forms other than `bytes=` never match.
fn parse_range(header: &str, total_len: u64) -> Option<ByteRange> {
    if total_len == 0 {
        return None;
    }
    // Surrounding whitespace is insignificant: `"bytes=0-1 "` seeks.
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start_str, end_str) = spec.split_once('-')?;
    if start_str.is_empty() && end_str.is_empty() {
        return None;
    }
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(start_str) || !digits(end_str) {
        return None;
    }
    if start_str.is_empty() {
        let suffix_len: u64 = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = total_len.saturating_sub(suffix_len);
        return Some(ByteRange {
            start,
            end: total_len - 1,
        });
    }
    let start: u64 = start_str.parse().ok()?;
    let end = if end_str.is_empty() {
        total_len - 1
    } else {
        let asked: u64 = end_str.parse().ok()?;
        asked.min(total_len - 1)
    };
    if start > end || start >= total_len {
        return None;
    }
    Some(ByteRange { start, end })
}

// ---------------------------------------------------------------------------
// Handler state, principal, and typed query (no bare Query)
// ---------------------------------------------------------------------------

/// Everything the stream handlers need.
pub struct StreamState<E> {
    /// Byte sourcing, leases, and transcode execution.
    pub engine: Arc<E>,
}

impl<E> Clone for StreamState<E> {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
        }
    }
}

/// Transcode hints. All optional; `format`, `max_bitrate` and
/// `start_seconds` pass straight to the engine's `decide()`, and
/// `estimate_content_length=true` asks a transcode landing for its
/// estimated length. Player deep links rely on these keys.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamQuery {
    /// Requested output codec (`mp3`, `opus`, `raw`, or unset).
    #[serde(default)]
    pub format: Option<String>,
    /// Client bitrate cap in kbps (0 or unset means uncapped).
    #[serde(default)]
    pub max_bitrate: Option<i64>,
    /// Ask a transcode landing for its estimated `Content-Length`.
    #[serde(default)]
    pub estimate_content_length: bool,
    /// Seek offset in seconds (`-ss`, transcode only).
    #[serde(default)]
    pub start_seconds: f64,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure these routes can return.
#[derive(Debug)]
pub enum StreamError {
    /// Unknown id. Message follows the v2 detail per source.
    NotFound {
        /// "Track file not found" or "Audio item not found".
        message: String,
    },
    /// Playback refused or path outside the music directory.
    Forbidden {
        /// User-facing reason, never a path.
        message: String,
    },
    /// Bad query, unknown source, or unsupported format.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Unsatisfiable range. Renders 416 plus `Content-Range: bytes */N`.
    Unsatisfiable {
        /// Object length for the `bytes */N` header.
        total_len: u64,
    },
    /// Leases exhausted. Renders 429 plus `Retry-After: 1`.
    Capacity,
    /// The source failed the read. A remote media server answers the
    /// closed-list `REMOTE_UNAVAILABLE`; a local file or a plugin a plain 502.
    Upstream {
        /// Which source failed.
        source: AudioSource,
        /// Log-only cause.
        cause: String,
    },
    /// Server fault. The cause reaches the log only.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

impl StreamError {
    /// A 500 whose cause the request boundary logs.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

/// 416 with `Content-Range: bytes */N`, shared with the download routes.
pub(crate) fn unsatisfiable(total_len: u64) -> ApiError {
    let error = ApiError::new(
        StatusCode::RANGE_NOT_SATISFIABLE,
        RANGE_NOT_SATISFIABLE,
        "Range not satisfiable",
    );
    match HeaderValue::from_str(&format!("bytes */{total_len}")) {
        Ok(value) => error.with_header(header::CONTENT_RANGE, value),
        Err(_) => error,
    }
}

impl From<StreamError> for ApiError {
    fn from(error: StreamError) -> Self {
        match error {
            StreamError::NotFound { message } => {
                ApiError::new(StatusCode::NOT_FOUND, STREAM_NOT_FOUND, message)
            }
            StreamError::Forbidden { message } => {
                ApiError::new(StatusCode::FORBIDDEN, STREAM_FORBIDDEN, message)
            }
            StreamError::InvalidInput { message } => ApiError::invalid_input(message),
            StreamError::Unsatisfiable { total_len } => unsatisfiable(total_len),
            StreamError::Capacity => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                STREAM_CAPACITY_EXHAUSTED,
                "Stream capacity exhausted",
            )
            .with_details(json!({ "retry_after": RETRY_AFTER_SECONDS }))
            .with_header(header::RETRY_AFTER, RETRY_AFTER_SECONDS.into()),
            StreamError::Upstream { source, cause } => {
                let cause = format!("failed to stream from {}: {cause}", source.display_name());
                match source {
                    AudioSource::Jellyfin | AudioSource::Navidrome | AudioSource::Plex => {
                        ApiError::unavailable(Unavailable::MediaServer, cause)
                    }
                    AudioSource::Local | AudioSource::Plugin => {
                        ApiError::fault(StatusCode::BAD_GATEWAY, cause)
                    }
                }
            }
            StreamError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

// ---------------------------------------------------------------------------
// Router and handlers
// ---------------------------------------------------------------------------

/// The stream route, GET and HEAD, for any signed-in user. The
/// Plex-compatible wildcard key also matches single-segment ids; the
/// document spells the wildcard the same way, so it names the route as
/// served.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    StreamState<MediaEngine>: FromRef<S>,
{
    Routes::new().user(routes!(stream_get_route, stream_head_route))
}

/// The stream route at a relative path over any engine, for tests that
/// drive it with fakes.
#[cfg(any(test, feature = "test-support"))]
pub fn stream_routes<E>(state: StreamState<E>) -> axum::Router
where
    E: StreamEngine + Send + Sync + 'static,
{
    axum::Router::new()
        .route(
            "/stream/{source}/{*key}",
            axum::routing::get(stream_get::<E>).head(stream_head::<E>),
        )
        .with_state(state)
}

/// Full or ranged audio bytes.
// The mounted form of `stream_get` over the production engine.
#[utoipa::path(
    get,
    tag = "stream",
    path = "/api/v3/stream/{source}/{*key}",
    params(
        ("source" = String, Path, description = "Audio source: local, jellyfin, navidrome, plex, or plugin"),
        ("*key" = String, Path, description = "Local file id, remote item id, Plex part key, or recording MBID for plugin. A wildcard: Plex part keys keep their slashes"),
        ("format" = Option<String>, Query, description = "Requested output codec: mp3, opus, or raw"),
        ("max_bitrate" = Option<i64>, Query, description = "Client bitrate cap in kbps (0 or unset means uncapped)"),
        ("start_seconds" = Option<f64>, Query, description = "Seek offset in seconds for a transcode"),
        ("estimate_content_length" = Option<bool>, Query, description = "Ask a transcode landing for its estimated Content-Length"),
    ),
    responses(
        (status = 200, description = "Full audio bytes or transcode output"),
        (status = 206, description = "Ranged audio bytes"),
        (status = 400, description = "Unknown source or unsupported format"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Playback refused"),
        (status = 404, description = "Unknown audio id"),
        (status = 416, description = "Range not satisfiable"),
        (status = 429, description = "Stream capacity exhausted"),
        (status = 502, description = "Remote source failed the read"),
    ),
    operation_id = "stream_get"
)]
pub async fn stream_get_route(
    state: State<StreamState<MediaEngine>>,
    user: User,
    path: Path<(String, String)>,
    headers: HeaderMap,
    query: ValidQuery<StreamQuery>,
) -> Result<Response, ApiError> {
    stream_get(state, user, path, headers, query).await
}

/// Same headers as GET, no body.
// The mounted form of `stream_head` over the production engine.
#[utoipa::path(
    head,
    tag = "stream",
    path = "/api/v3/stream/{source}/{*key}",
    params(
        ("source" = String, Path, description = "Audio source: local, jellyfin, navidrome, plex, or plugin"),
        ("*key" = String, Path, description = "Local file id, remote item id, Plex part key, or recording MBID for plugin. A wildcard: Plex part keys keep their slashes"),
        ("format" = Option<String>, Query, description = "Requested output codec: mp3, opus, or raw"),
        ("max_bitrate" = Option<i64>, Query, description = "Client bitrate cap in kbps (0 or unset means uncapped)"),
        ("start_seconds" = Option<f64>, Query, description = "Seek offset in seconds for a transcode"),
        ("estimate_content_length" = Option<bool>, Query, description = "Ask a transcode landing for its estimated Content-Length"),
    ),
    responses(
        (status = 200, description = "Stream headers, no body"),
        (status = 400, description = "Unknown source or unsupported format"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Playback refused"),
        (status = 404, description = "Unknown audio id"),
        (status = 429, description = "Stream capacity exhausted"),
        (status = 502, description = "Remote source failed the read"),
    ),
    operation_id = "stream_head"
)]
pub async fn stream_head_route(
    state: State<StreamState<MediaEngine>>,
    user: User,
    path: Path<(String, String)>,
    headers: HeaderMap,
    query: ValidQuery<StreamQuery>,
) -> Result<Response, ApiError> {
    stream_head(state, user, path, headers, query).await
}

/// Full or ranged audio bytes from any engine.
pub(crate) async fn stream_get<E: StreamEngine>(
    State(state): State<StreamState<E>>,
    User(user): User,
    Path((source, key)): Path<(String, String)>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<StreamQuery>,
) -> Result<Response, ApiError> {
    Ok(serve(&state, &user, &source, &key, &headers, &query, false).await?)
}

/// Same headers as GET, no body, from any engine.
pub(crate) async fn stream_head<E: StreamEngine>(
    State(state): State<StreamState<E>>,
    User(user): User,
    Path((source, key)): Path<(String, String)>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<StreamQuery>,
) -> Result<Response, ApiError> {
    Ok(serve(&state, &user, &source, &key, &headers, &query, true).await?)
}

/// Shared GET/HEAD implementation: open the object, then apply the HTTP
/// semantics (transcode headers, range slicing, HEAD body strip).
async fn serve<E: StreamEngine>(
    state: &StreamState<E>,
    user: &Principal,
    source_raw: &str,
    key: &str,
    headers: &HeaderMap,
    query: &StreamQuery,
    head_only: bool,
) -> Result<Response, StreamError> {
    let source = AudioSource::parse(source_raw).ok_or(StreamError::InvalidInput {
        message: "Unknown stream source".to_owned(),
    })?;
    let media = state
        .engine
        .open_stream(
            StreamOpen {
                source,
                key: key.to_owned(),
                user_id: user.user_id().to_owned(),
                params: StreamParams {
                    format: query.format.clone(),
                    max_bitrate_kbps: query.max_bitrate,
                    estimate_content_length: query.estimate_content_length,
                    start_seconds: query.start_seconds,
                },
            },
            head_only,
        )
        .await
        .map_err(|fault| map_fault(fault, source))?;

    let framed = frame(
        media,
        range_header(headers),
        head_only,
        query.estimate_content_length,
    )?;
    let body = match framed.body {
        FramedBody::Empty => Body::empty(),
        FramedBody::Bytes(bytes) => Body::from(bytes),
        FramedBody::Stream(chunks) => Body::from_stream(chunks),
    };
    let mut response = (framed.status, body).into_response();
    response.headers_mut().extend(framed.headers);
    Ok(response)
}

/// The `Range` header as text. One that is not visible ASCII reads as an
/// empty value, which no range matches (416).
pub fn range_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::RANGE)
        .map(|value| value.to_str().unwrap_or(""))
}

/// A framed stream answer: status, framing headers and body.
#[derive(Debug)]
pub struct Framed {
    /// 200 or 206.
    pub status: StatusCode,
    /// `Content-Type`, `Content-Encoding`, `Accept-Ranges`, and
    /// `Content-Length` and `Content-Range` where they apply; a transcode
    /// adds `Cache-Control`.
    pub headers: HeaderMap,
    /// What to send.
    pub body: FramedBody,
}

/// The body of a [`Framed`] answer. A stream carries the open's direct
/// lease inside, so the slot stays held until the last byte is sent or
/// the client goes away.
pub enum FramedBody {
    /// Nothing (HEAD, or a transcode HEAD answered without ffmpeg).
    Empty,
    /// Bytes already in memory, sent whole.
    Bytes(Vec<u8>),
    /// Bytes read as the response goes out.
    Stream(ChunkStream),
}

impl std::fmt::Debug for FramedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::Stream(_) => f.write_str("Stream"),
        }
    }
}

/// Frame one opened object for the wire, the same way on every stream
/// route. A direct object honours one `Range` (206) or answers whole (200);
/// any other range, multi-range included, is
/// [`StreamError::Unsatisfiable`], which each protocol renders as its own
/// 416. A transcode is 200 with no ranges and no caching, sized only when
/// `want_estimate` and the engine estimated it. HEAD gets GET's status and
/// headers with no body.
pub fn frame(
    media: StreamMedia,
    range: Option<&str>,
    head_only: bool,
    want_estimate: bool,
) -> Result<Framed, StreamError> {
    let StreamMedia {
        content_type,
        total_len,
        transcoded,
        estimated_len,
        body,
        lease,
    } = media;
    let hold = move |chunks: ChunkStream| match lease {
        Some(lease) => lease.hold(chunks),
        None => chunks,
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, upstream_content_type(&content_type));
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    if transcoded {
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let estimated = estimated_len.filter(|_| want_estimate);
        if let Some(len) = estimated {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        } else if let (true, MediaBody::Bytes(bytes)) = (head_only, &body) {
            // An in-memory body knows its length: pin it on HEAD for GET/HEAD
            // parity (GET's buffered body gets it stamped).
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        }
        // A live transcode has no known size, so GET without an estimate
        // carries no Content-Length and streams chunked.
        let body = if head_only {
            FramedBody::Empty
        } else {
            match body {
                MediaBody::Bytes(bytes) => FramedBody::Bytes(bytes),
                MediaBody::Chunks(chunks) => FramedBody::Stream(hold(chunks)),
                MediaBody::File(path) => FramedBody::Stream(hold(file_range(path, 0, total_len))),
                MediaBody::Spans(open) => FramedBody::Stream(hold(open(0, total_len))),
                MediaBody::Empty => FramedBody::Empty,
            }
        };
        return Ok(Framed {
            status: StatusCode::OK,
            headers,
            body,
        });
    }
    let span = DirectSpan::for_request(range, total_len)
        .ok_or(StreamError::Unsatisfiable { total_len })?;
    span.write_headers(&mut headers);
    let body = if head_only {
        FramedBody::Empty
    } else {
        direct_body(body, span.start, span.len, hold)?
    };
    Ok(Framed {
        status: span.status(),
        headers,
        body,
    })
}

/// The bytes a direct (untranscoded) response sends: the whole object, or
/// the one span a `Range` header asked for. [`frame`], the Subsonic stream
/// and download routes and local file downloads all frame their bodies
/// with this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSpan {
    /// First byte sent.
    pub start: u64,
    /// Bytes sent.
    pub len: u64,
    /// Length of the whole object.
    pub total_len: u64,
    /// True when answering a satisfiable range (206).
    pub partial: bool,
}

impl DirectSpan {
    /// Apply the [`parse_range`] rules to an optional `Range` header. No
    /// header sends everything. `None` means 416, a multi-range ask
    /// included.
    pub fn for_request(range: Option<&str>, total_len: u64) -> Option<Self> {
        let Some(text) = range else {
            return Some(Self {
                start: 0,
                len: total_len,
                total_len,
                partial: false,
            });
        };
        parse_range(text, total_len).map(|resolved| Self {
            start: resolved.start,
            len: resolved.end - resolved.start + 1,
            total_len,
            partial: true,
        })
    }

    /// 206 for a range, else 200.
    pub fn status(&self) -> StatusCode {
        if self.partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        }
    }

    /// `Accept-Ranges`, `Content-Length`, and `Content-Range` on a 206.
    pub fn write_headers(&self, headers: &mut HeaderMap) {
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(self.len));
        if self.partial {
            let last = self.start + self.len.saturating_sub(1);
            if let Ok(value) =
                HeaderValue::from_str(&format!("bytes {}-{last}/{}", self.start, self.total_len))
            {
                headers.insert(header::CONTENT_RANGE, value);
            }
        }
    }
}

/// `len` bytes from `start` of a direct object's body, holding `lease`
/// until a streamed body ends. For a caller that framed the span itself
/// with [`DirectSpan`].
pub fn read_span(
    body: MediaBody,
    lease: Option<super::leases::OwnedDirectLease>,
    start: u64,
    len: u64,
) -> Result<FramedBody, StreamError> {
    direct_body(body, start, len, move |chunks| match lease {
        Some(lease) => lease.hold(chunks),
        None => chunks,
    })
}

/// Body for `len` bytes of a direct object starting at `start`.
fn direct_body(
    body: MediaBody,
    start: u64,
    len: u64,
    hold: impl FnOnce(ChunkStream) -> ChunkStream,
) -> Result<FramedBody, StreamError> {
    match body {
        MediaBody::Bytes(bytes) => {
            let from = usize::try_from(start).unwrap_or(usize::MAX);
            let to = usize::try_from(len)
                .ok()
                .and_then(|len| from.checked_add(len))
                .unwrap_or(usize::MAX);
            if from == 0 && to == bytes.len() {
                return Ok(FramedBody::Bytes(bytes));
            }
            let slice = bytes
                .get(from..to)
                .ok_or_else(|| StreamError::internal("engine short-read the ranged object"))?;
            Ok(FramedBody::Bytes(slice.to_vec()))
        }
        MediaBody::File(path) => Ok(FramedBody::Stream(hold(file_range(path, start, len)))),
        MediaBody::Spans(open) => Ok(FramedBody::Stream(hold(open(start, len)))),
        MediaBody::Chunks(_) | MediaBody::Empty => {
            Err(StreamError::internal("engine returned no direct body"))
        }
    }
}

/// Read size of one file chunk.
const FILE_CHUNK: u64 = 64 * 1024;

/// Stream `len` bytes of `path` from `start`. The file is opened when the
/// stream is first polled. A file that ends early fails the stream instead
/// of sending short.
pub fn file_range(path: PathBuf, start: u64, len: u64) -> ChunkStream {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

    struct Cursor {
        path: PathBuf,
        file: Option<tokio::fs::File>,
        start: u64,
        remaining: u64,
    }
    async fn next(mut cursor: Cursor) -> std::io::Result<(Vec<u8>, Cursor)> {
        let mut file = match cursor.file.take() {
            Some(file) => file,
            None => {
                let mut file = tokio::fs::File::open(&cursor.path).await?;
                file.seek(std::io::SeekFrom::Start(cursor.start)).await?;
                file
            }
        };
        let want = usize::try_from(cursor.remaining.min(FILE_CHUNK)).unwrap_or(0);
        let mut buf = vec![0u8; want];
        let read = file.read(&mut buf).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "file ended before the advertised length",
            ));
        }
        buf.truncate(read);
        cursor.remaining = cursor.remaining.saturating_sub(read as u64);
        cursor.file = Some(file);
        Ok((buf, cursor))
    }
    let cursor = Cursor {
        path,
        file: None,
        start,
        remaining: len,
    };
    futures_util::stream::unfold(Some(cursor), |state| async move {
        let cursor = state.filter(|cursor| cursor.remaining > 0)?;
        Some(match next(cursor).await {
            Ok((buf, cursor)) => (Ok(buf), Some(cursor)),
            Err(error) => (Err(error), None),
        })
    })
    .boxed()
}

/// Map an engine fault to the wire. Messages stay fixed or user-safe; only
/// the internal cause reaches the log.
fn map_fault(fault: StreamFault, source: AudioSource) -> StreamError {
    match fault {
        StreamFault::NotFound => StreamError::NotFound {
            message: match source {
                AudioSource::Local => "Track file not found".to_owned(),
                _ => "Audio item not found".to_owned(),
            },
        },
        StreamFault::Forbidden { message } => StreamError::Forbidden { message },
        StreamFault::InvalidInput { message } => StreamError::InvalidInput { message },
        StreamFault::Capacity => StreamError::Capacity,
        StreamFault::Upstream { source, cause } => StreamError::Upstream { source, cause },
        StreamFault::Internal { cause } => StreamError::Internal { cause },
    }
}

/// Upstream content type, infallible: remotes resolve their own type, and an
/// unparseable one must not 500 the stream, so it falls back to `audio/mpeg`.
fn upstream_content_type(content_type: &str) -> HeaderValue {
    HeaderValue::from_str(content_type).unwrap_or_else(|_| HeaderValue::from_static("audio/mpeg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcode_media() -> StreamMedia {
        let bytes = b"fake-transcoded-mp3-bytes".to_vec();
        StreamMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: 0,
            transcoded: true,
            estimated_len: Some(bytes.len() as u64),
            body: MediaBody::Bytes(bytes),
            lease: None,
        }
    }

    #[test]
    fn transcode_sets_length_only_with_estimate() {
        let estimated = frame(transcode_media(), None, false, true).expect("frames");
        assert_eq!(
            estimated.headers.get(header::CONTENT_LENGTH),
            Some(&HeaderValue::from_static("25"))
        );

        let plain = frame(transcode_media(), None, false, false).expect("frames");
        assert_eq!(plain.headers.get(header::CONTENT_LENGTH), None);
        assert_eq!(
            plain.headers.get(header::ACCEPT_RANGES),
            Some(&HeaderValue::from_static("none"))
        );
    }

    #[tokio::test]
    async fn the_lease_lasts_until_the_framed_stream_is_gone() {
        let gate = Arc::new(super::super::leases::DirectGate::new());
        let lease = gate.acquire_owned("u1").await.expect("slot");
        let media = StreamMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: 3,
            transcoded: false,
            estimated_len: None,
            body: MediaBody::Spans(Box::new(|_, _| {
                futures_util::stream::iter(vec![Ok(b"ab".to_vec()), Ok(b"c".to_vec())]).boxed()
            })),
            lease: Some(lease),
        };
        let framed = frame(media, None, false, false).expect("frames");
        assert_eq!(gate.active(), 1, "held while the body waits");
        let FramedBody::Stream(mut chunks) = framed.body else {
            panic!("a span streams");
        };
        let mut read = Vec::new();
        while let Some(chunk) = chunks.next().await {
            read.extend(chunk.expect("reads"));
        }
        assert_eq!(read, b"abc");
        drop(chunks);
        assert_eq!(gate.active(), 0, "released once the stream is done");
    }

    #[test]
    fn range_matrix_matches_v2() {
        let total = 100;
        assert_eq!(
            parse_range("bytes=0-9", total),
            Some(ByteRange { start: 0, end: 9 })
        );
        assert_eq!(
            parse_range("bytes=90-", total),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=-10", total),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=-1000", total),
            Some(ByteRange { start: 0, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=0-9999", total),
            Some(ByteRange { start: 0, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=99-99", total),
            Some(ByteRange { start: 99, end: 99 })
        );
        for bad in [
            "bytes=100-",
            "bytes=50-40",
            "bytes=-0",
            "bytes=-",
            "bytes=",
            "bytes=abc",
            "bytes=0-1,2-3",
            "items=0-9",
            "bytes=0 - 9",
            "",
        ] {
            assert_eq!(parse_range(bad, total), None, "{bad:?}");
        }
        assert_eq!(parse_range("bytes=0-", 0), None);
        assert_eq!(parse_range("bytes=-5", 0), None);
    }

    #[test]
    fn content_types_cover_streamable_audio_only() {
        for (extension, media_type) in [
            (".flac", "audio/flac"),
            ("mp3", "audio/mpeg"),
            (".ogg", "audio/ogg"),
            (".m4a", "audio/mp4"),
            (".aac", "audio/aac"),
            (".wav", "audio/wav"),
            (".opus", "audio/opus"),
            (".MP3", "audio/mpeg"),
        ] {
            assert_eq!(content_type_for_extension(extension), Some(media_type));
            let bare = extension.trim_start_matches('.').to_lowercase();
            assert_eq!(extension_for_content_type(media_type), Some(bare.as_str()));
        }
        assert_eq!(content_type_for_extension(".WMA"), None);
        assert_eq!(content_type_for_extension(".zip"), None);
        assert_eq!(content_type_for_extension(""), None);
    }

    #[test]
    fn sources_parse_case_insensitively() {
        assert_eq!(AudioSource::parse("local"), Some(AudioSource::Local));
        assert_eq!(AudioSource::parse("Plex"), Some(AudioSource::Plex));
        assert_eq!(AudioSource::parse("ftp"), None);
        assert_eq!(AudioSource::parse(""), None);
    }
}
