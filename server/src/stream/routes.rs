//! Stage-6 stream-gateway routes: one source-keyed GET/HEAD surface over the engine.
//!
//! This file owns the HTTP layer only: route shapes, the typed query, range
//! parsing, byte-exact 200/206/416/HEAD headers, and the error envelope. Byte
//! sourcing, leases, and transcode execution live in the engine behind the
//! [`StreamEngine`] seam below.
//!
//! # Routes (relative; the integrator nests these under `/api/v3`)
//!
//! ```text
//! GET  /stream/{source}/{*key}   full or ranged audio bytes
//! HEAD /stream/{source}/{*key}   same headers, no body
//! ```
//!
//! `source` is one of `local`, `jellyfin`, `navidrome`, `plex`. `key` is the
//! source identifier: a local file id, a Jellyfin/Navidrome item id, or a Plex
//! part key (which may itself contain slashes, hence the wildcard). Query
//! keys are `format`, `max_bitrate`, and `estimate_content_length` (see
//! [`StreamQuery`]).
//!
//! # Deep-link note (stage 12)
//!
//! The R2/R1 player call sites migrate in stage 12, so these shapes are kept
//! deliberately small and stable: one handler shape
//! `(State, StreamUser, Path, HeaderMap, ValidatedQuery)`, path params only
//! for identity, query only for transcode hints. Stage 12 should deep-link
//! players at `/api/v3/stream/{source}/{key}` with those three query keys
//! and treat any new gateway capability as a new query key, not a new route.
//!
//! # Integrator seams
//!
//! * Engine: [`StreamEngine`] is a minimal local stand-in. The sibling
//!   `gateway.rs` slice owns the real engine (leases + `Transcoder` +
//!   direct/transcode reads); when it lands, delete this trait and the
//!   [`StreamOpen`]/[`OpenMedia`]/[`StreamFault`] types and re-point the
//!   handlers at the sibling spelling. The range/HEAD/envelope/header logic
//!   in this file stays untouched.
//! * Whole-object reads: the stand-in hands routes the whole object as
//!   `bytes` so range slicing stays byte-exact here. If the real engine
//!   grows a ranged-read method, thread the already-parsed [`ByteRange`]
//!   through instead; the 206/416 decisions must not move.
//! * Mounting: [`stream_routes`] returns a relative-path router. Merge it
//!   into the `/api/v3` nest inside the deny-by-default session gate in
//!   `create_app`, and extend `AppState` with the engine plus ids. Reporting
//!   (`start`/`progress`/`stop`/`scrobble`/`now-playing`/`stopped`) is owned
//!   by the sibling reporting slice, not this router.
//! * [`RETRY_AFTER_SECONDS`] duplicates the sibling transcode constant;
//!   keep one spelling when the slices merge.
//! * [`content_type_for_extension`] ports the v2 `CONTENT_TYPE_MAP` minus
//!   WMA, which never streams per the stage cut. The engine resolves the
//!   final content type (upstream wins for remotes); local reads go through
//!   this table.
//!
//! # v2 parity notes
//!
//! Range parsing ports `LocalFilesService.stream_track` rule for rule:
//! single `bytes=start-end` only (suffix and open forms included), end
//! clamped to size-1, suffix longer than the file clamped to the whole file,
//! anything else 416. The 416 carries `Content-Range: bytes */N` (v2 compat
//! routers), and 429 carries `Retry-After: 1` (v2 subsonic router).

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{FromRequest, FromRequestParts, Path, Query, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{self},
        request::Parts,
    },
    response::{IntoResponse, Response},
    routing::get,
};
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::error::{ErrorBody, ErrorEnvelope};
use droppedneedle::ids::IdGenerator;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

// ---------------------------------------------------------------------------
// Error codes (SCREAMING_SNAKE, shared envelope)
// ---------------------------------------------------------------------------

/// Missing session. Carries the `WWW-Authenticate` challenge.
pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
/// Unknown file/item id. Message follows the v2 404 detail per source.
pub const STREAM_NOT_FOUND: &str = "STREAM_NOT_FOUND";
/// Playback refused, or a local path outside the music directory.
pub const STREAM_FORBIDDEN: &str = "STREAM_FORBIDDEN";
/// Bad query string, unknown source, or unsupported audio format.
pub const INVALID_INPUT: &str = "INVALID_INPUT";
/// Range header present but unsatisfiable. Carries `Content-Range: bytes */N`.
pub const RANGE_NOT_SATISFIABLE: &str = "RANGE_NOT_SATISFIABLE";
/// Direct or transcode leases exhausted. Carries `Retry-After: 1`.
pub const STREAM_CAPACITY_EXHAUSTED: &str = "STREAM_CAPACITY_EXHAUSTED";
/// The remote source failed the read.
pub const UPSTREAM_UNAVAILABLE: &str = "UPSTREAM_UNAVAILABLE";

/// Challenge sent on every 401, mirroring the sibling slices.
pub const WWW_AUTHENTICATE_BEARER: &str = "Bearer";
/// Retry hint on every 429 (v2 sends 1; duplicates the transcode constant).
pub const RETRY_AFTER_SECONDS: u64 = 1;

// ---------------------------------------------------------------------------
// Audio sources
// ---------------------------------------------------------------------------

/// The four stream sources behind one gateway. Display names feed the 502
/// message only; wire identity is the lowercase route segment.
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
}

impl AudioSource {
    /// Parse the `{source}` path segment. Case-insensitive; `None` rejects.
    pub fn parse(segment: &str) -> Option<Self> {
        match segment.to_lowercase().as_str() {
            "local" => Some(Self::Local),
            "jellyfin" => Some(Self::Jellyfin),
            "navidrome" => Some(Self::Navidrome),
            "plex" => Some(Self::Plex),
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
        }
    }

    /// Human name for the 502 message (safe: fixed strings only).
    fn display_name(self) -> &'static str {
        match self {
            Self::Local => "local file",
            Self::Jellyfin => "Jellyfin",
            Self::Navidrome => "Navidrome",
            Self::Plex => "Plex",
        }
    }
}

// ---------------------------------------------------------------------------
// Engine seam (stand-in; the sibling gateway.rs owns the real engine)
// ---------------------------------------------------------------------------

/// Transcode hints from the query string, mapped 1:1 for the engine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamParams {
    /// Requested output codec (`mp3`, `opus`, `raw`, or unset). The engine's
    /// `decide()` interprets it, including the `raw` backstop.
    pub format: Option<String>,
    /// Client bitrate cap in kbps. 0 or unset means uncapped.
    pub max_bitrate_kbps: Option<i64>,
    /// Ask a transcode landing for its estimated `Content-Length`.
    pub estimate_content_length: bool,
}

/// One engine read: identity, lease principal, and transcode hints.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Whole media object plus its response metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenMedia {
    /// Engine-resolved content type (upstream wins; local via
    /// [`content_type_for_extension`]).
    pub content_type: String,
    /// Exact object length in bytes.
    pub total_len: u64,
    /// Transcode landing: ranges are refused, never honored.
    pub transcoded: bool,
    /// Estimated length for the optional transcode `Content-Length`.
    pub estimated_len: Option<u64>,
    /// Whole object bytes (direct) or whole transcode output (fake-sized).
    pub bytes: Vec<u8>,
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
    /// The remote source failed the read.
    Upstream {
        /// Which source failed, for the 502 message.
        source: AudioSource,
    },
    /// Server fault. The cause reaches the log only, never the wire.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

/// Minimal engine stand-in: open one media object or fail with a [`StreamFault`].
/// The sibling `gateway.rs` slice replaces this trait; handlers stay the same.
pub trait StreamEngine: Send + Sync {
    /// Resolve and read one media object for `request`.
    fn open(
        &self,
        request: StreamOpen,
    ) -> impl Future<Output = Result<OpenMedia, StreamFault>> + Send;
}

// ---------------------------------------------------------------------------
// Content types (v2 table minus WMA, which never streams)
// ---------------------------------------------------------------------------

/// Content type for a local file extension, e.g. `.flac` or `flac`.
/// Case-insensitive. `None` (including `.wma`) means the format never
/// streams and the engine answers [`StreamFault::InvalidInput`].
pub fn content_type_for_extension(extension: &str) -> Option<&'static str> {
    match extension.trim_start_matches('.').to_lowercase().as_str() {
        "flac" => Some("audio/flac"),
        "mp3" => Some("audio/mpeg"),
        "ogg" => Some("audio/ogg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "wav" => Some("audio/wav"),
        "opus" => Some("audio/opus"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Range parsing (v2 `stream_track` rules, byte-exact)
// ---------------------------------------------------------------------------

/// Inclusive resolved byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte offset.
    pub start: u64,
    /// Last byte offset, inclusive.
    pub end: u64,
}

impl ByteRange {
    /// Length of the slice in bytes. Ranges are never empty by
    /// construction (`parse_range` only returns `start <= end`), so there
    /// is deliberately no `is_empty`.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

/// Parse a single `Range` header against `total_len`, v2 rule for rule:
/// `bytes=start-end` (end clamped), `bytes=start-` (open), `bytes=-suffix`
/// (clamped to the whole file when longer). `None` means 416: no match,
/// both bounds empty, suffix of 0, start past the end, or an empty object.
/// Multi-range and unit forms other than `bytes=` never match.
pub fn parse_range(header: &str, total_len: u64) -> Option<ByteRange> {
    if total_len == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?;
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

/// Everything the stream handlers need: the engine plus id generation.
pub struct StreamState<E> {
    /// Byte sourcing, leases, and transcode execution.
    pub engine: Arc<E>,
    /// Fresh error ids for the 5xx envelope.
    pub ids: Arc<dyn IdGenerator>,
}

impl<E> Clone for StreamState<E> {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            ids: Arc::clone(&self.ids),
        }
    }
}

/// Any authenticated session. The session gate already ran, so presence of
/// the stashed session is the whole check; the user id feeds per-principal
/// leases. Missing session reads as 401, mirroring the sibling extractors.
pub struct StreamUser {
    /// Lease principal.
    pub user_id: String,
}

impl<E: Send + Sync> FromRequestParts<StreamState<E>> for StreamUser {
    type Rejection = StreamError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &StreamState<E>,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentSession>()
            .map(|session| Self {
                user_id: session.user_id.clone(),
            })
            .ok_or(StreamError::Unauthorized {
                message: "Authentication required".to_owned(),
            })
    }
}

/// Transcode hints. All optional; `format`/`max_bitrate` pass straight to
/// the engine's `decide()`, and `estimate_content_length=true` asks a
/// transcode landing for its estimated length. Stage-12 deep links freeze
/// these three keys.
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
}

/// Query extractor that renders failures in the shared envelope instead of
/// Axum's default plain-text 400.
pub struct ValidatedQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidatedQuery<T> {
    type Rejection = StreamError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| StreamError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every failure these routes can return.
#[derive(Debug)]
pub enum StreamError {
    /// No valid session. Carries the `WWW-Authenticate` challenge.
    Unauthorized {
        /// User-safe reason.
        message: String,
    },
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
    /// The remote source failed the read.
    Upstream {
        /// "Failed to stream local file" or "Failed to stream from {Name}".
        message: String,
    },
    /// Server fault. Fixed body plus the request-tied id.
    Internal {
        /// Ties the wire response to the server log line.
        error_id: String,
    },
}

impl StreamError {
    /// Build a 500, logging the real cause with its id.
    pub fn internal(cause: &dyn std::fmt::Display, ids: &dyn IdGenerator) -> Self {
        let error_id = ids.new_id();
        tracing::error!(error_id, %cause, "stream gateway failed");
        Self::Internal { error_id }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::InvalidInput { .. } => StatusCode::BAD_REQUEST,
            Self::Unsatisfiable { .. } => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::Capacity => StatusCode::TOO_MANY_REQUESTS,
            Self::Upstream { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn envelope(&self) -> ErrorEnvelope {
        let (code, message, details) = match self {
            Self::Unauthorized { message } => (UNAUTHORIZED.to_owned(), message.clone(), None),
            Self::NotFound { message } => (STREAM_NOT_FOUND.to_owned(), message.clone(), None),
            Self::Forbidden { message } => (STREAM_FORBIDDEN.to_owned(), message.clone(), None),
            Self::InvalidInput { message } => (INVALID_INPUT.to_owned(), message.clone(), None),
            Self::Unsatisfiable { .. } => (
                RANGE_NOT_SATISFIABLE.to_owned(),
                "Range not satisfiable".to_owned(),
                None,
            ),
            Self::Capacity => (
                STREAM_CAPACITY_EXHAUSTED.to_owned(),
                "Stream capacity exhausted".to_owned(),
                Some(json!({ "retry_after": RETRY_AFTER_SECONDS })),
            ),
            Self::Upstream { message } => (UPSTREAM_UNAVAILABLE.to_owned(), message.clone(), None),
            Self::Internal { error_id } => (
                droppedneedle::error::INTERNAL_ERROR.to_owned(),
                droppedneedle::error::FIXED_INTERNAL_MESSAGE.to_owned(),
                Some(json!({ "error_id": error_id })),
            ),
        };
        ErrorEnvelope {
            error: ErrorBody {
                code,
                message,
                details,
            },
        }
    }
}

impl IntoResponse for StreamError {
    fn into_response(self) -> Response {
        let status = self.status();
        let mut response = (status, axum::Json(self.envelope())).into_response();
        let headers = response.headers_mut();
        match &self {
            Self::Unsatisfiable { total_len } => {
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total_len}")) {
                    headers.insert(header::CONTENT_RANGE, value);
                }
            }
            Self::Capacity => {
                if let Ok(value) = HeaderValue::from_str(&RETRY_AFTER_SECONDS.to_string()) {
                    headers.insert(header::RETRY_AFTER, value);
                }
            }
            Self::Unauthorized { .. } => {
                if let Ok(challenge) = HeaderValue::from_str(WWW_AUTHENTICATE_BEARER) {
                    headers.insert(header::WWW_AUTHENTICATE, challenge);
                }
            }
            Self::NotFound { .. }
            | Self::Forbidden { .. }
            | Self::InvalidInput { .. }
            | Self::Upstream { .. }
            | Self::Internal { .. } => {}
        }
        response
    }
}

// ---------------------------------------------------------------------------
// Router and handlers
// ---------------------------------------------------------------------------

/// Relative-path stream router for nesting under `/api/v3` inside the
/// session gate. One route serves both GET and HEAD; the Plex-compatible
/// wildcard key also matches single-segment ids.
pub fn stream_routes<E>(state: StreamState<E>) -> Router
where
    E: StreamEngine + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/stream/{source}/{*key}",
            get(stream_get::<E>).head(stream_head::<E>),
        )
        .with_state(state)
}

/// Full or ranged audio bytes.
async fn stream_get<E: StreamEngine>(
    State(state): State<StreamState<E>>,
    user: StreamUser,
    Path((source, key)): Path<(String, String)>,
    headers: HeaderMap,
    ValidatedQuery(query): ValidatedQuery<StreamQuery>,
) -> Result<Response, StreamError> {
    serve(&state, &user, &source, &key, &headers, &query, false).await
}

/// Same headers as GET, no body.
async fn stream_head<E: StreamEngine>(
    State(state): State<StreamState<E>>,
    user: StreamUser,
    Path((source, key)): Path<(String, String)>,
    headers: HeaderMap,
    ValidatedQuery(query): ValidatedQuery<StreamQuery>,
) -> Result<Response, StreamError> {
    serve(&state, &user, &source, &key, &headers, &query, true).await
}

/// Shared GET/HEAD implementation: open the object, then apply the HTTP
/// semantics (transcode headers, range slicing, HEAD body strip).
async fn serve<E: StreamEngine>(
    state: &StreamState<E>,
    user: &StreamUser,
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
        .open(StreamOpen {
            source,
            key: key.to_owned(),
            user_id: user.user_id.clone(),
            params: StreamParams {
                format: query.format.clone(),
                max_bitrate_kbps: query.max_bitrate,
                estimate_content_length: query.estimate_content_length,
            },
        })
        .await
        .map_err(|fault| map_fault(fault, source, state.ids.as_ref()))?;

    if media.transcoded {
        return transcode_response(
            &media,
            query.estimate_content_length,
            head_only,
            state.ids.as_ref(),
        );
    }

    let range = match headers.get(header::RANGE) {
        None => None,
        Some(value) => {
            let text = value.to_str().unwrap_or("");
            match parse_range(text, media.total_len) {
                Some(resolved) => Some(resolved),
                None => {
                    return Err(StreamError::Unsatisfiable {
                        total_len: media.total_len,
                    });
                }
            }
        }
    };

    let content_type: HeaderValue = upstream_content_type(&media.content_type);
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, content_type);
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );

    let (status, body) = match range {
        None => {
            response_headers.insert(
                header::CONTENT_LENGTH,
                header_value(&media.total_len.to_string(), state.ids.as_ref())?,
            );
            let body = if head_only {
                Body::empty()
            } else {
                Body::from(media.bytes)
            };
            (StatusCode::OK, body)
        }
        Some(resolved) => {
            let start = usize::try_from(resolved.start).unwrap_or(usize::MAX);
            let end = usize::try_from(resolved.end).unwrap_or(0);
            let slice = media
                .bytes
                .get(start..=end)
                .filter(|slice| slice.len() as u64 == resolved.len())
                .ok_or_else(|| {
                    StreamError::internal(
                        &"engine short-read the ranged object",
                        state.ids.as_ref(),
                    )
                })?;
            response_headers.insert(
                header::CONTENT_LENGTH,
                header_value(&slice.len().to_string(), state.ids.as_ref())?,
            );
            response_headers.insert(
                header::CONTENT_RANGE,
                header_value(
                    &format!(
                        "bytes {}-{}/{}",
                        resolved.start, resolved.end, media.total_len
                    ),
                    state.ids.as_ref(),
                )?,
            );
            let body = if head_only {
                Body::empty()
            } else {
                Body::from(slice.to_vec())
            };
            (StatusCode::PARTIAL_CONTENT, body)
        }
    };

    let mut response = (status, body).into_response();
    response.headers_mut().extend(response_headers);
    Ok(response)
}

/// Transcode landing: 200, no ranges, no caching, identity encoding, and the
/// estimated length only when the client asked for it (v2 headers).
fn transcode_response(
    media: &OpenMedia,
    want_estimate: bool,
    head_only: bool,
    ids: &dyn IdGenerator,
) -> Result<Response, StreamError> {
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        upstream_content_type(&media.content_type),
    );
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response_headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    let estimated = if want_estimate {
        media.estimated_len
    } else {
        None
    };
    if let Some(len) = estimated {
        response_headers.insert(header::CONTENT_LENGTH, header_value(&len.to_string(), ids)?);
    } else if head_only {
        // Without an estimate GET's length is the router's automatic stamp
        // of the buffered body; HEAD's body is empty (auto-stamp 0), so pin
        // the same actual length explicitly for GET/HEAD parity.
        response_headers.insert(
            header::CONTENT_LENGTH,
            header_value(&media.bytes.len().to_string(), ids)?,
        );
    }
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(media.bytes.clone())
    };
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().extend(response_headers);
    // Without an estimate no Content-Length is set here for GET; the router
    // stamps one for buffered bodies with an exact size, while the
    // production transcode stream (unknown size) stays lengthless at serve
    // time.
    Ok(response)
}

/// Map an engine fault to the wire. Messages stay fixed or user-safe; only
/// the internal cause reaches the log.
fn map_fault(fault: StreamFault, source: AudioSource, ids: &dyn IdGenerator) -> StreamError {
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
        StreamFault::Upstream { source } => StreamError::Upstream {
            message: match source {
                AudioSource::Local => "Failed to stream local file".to_owned(),
                _ => format!("Failed to stream from {}", source.display_name()),
            },
        },
        StreamFault::Internal { cause } => StreamError::internal(&cause, ids),
    }
}

/// Fallible header construction without panics: only ASCII digits and fixed
/// tokens reach here, so failure is an internal error by construction.
fn header_value(text: &str, ids: &dyn IdGenerator) -> Result<HeaderValue, StreamError> {
    HeaderValue::from_str(text)
        .map_err(|_| StreamError::internal(&"stream header failed to render", ids))
}

/// Upstream content type, infallible: remotes resolve their own type, and an
/// unparseable one must not 500 the stream, so it falls back to `audio/mpeg`.
fn upstream_content_type(content_type: &str) -> HeaderValue {
    HeaderValue::from_str(content_type).unwrap_or_else(|_| HeaderValue::from_static("audio/mpeg"))
}

// ---------------------------------------------------------------------------
// OpenAPI stubs (never mounted; the handlers are generic over the engine)
// ---------------------------------------------------------------------------

/// Full or ranged audio bytes.
#[utoipa::path(
    get,
    path = "/api/v3/stream/{source}/{key}",
    params(
        ("source" = String, Path, description = "Audio source: local, jellyfin, navidrome, or plex"),
        ("key" = String, Path, description = "Local file id, remote item id, or Plex part key"),
        ("format" = Option<String>, Query, description = "Requested output codec: mp3, opus, or raw"),
        ("max_bitrate" = Option<i64>, Query, description = "Client bitrate cap in kbps (0 or unset means uncapped)"),
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
    )
)]
#[allow(dead_code)]
pub(crate) fn doc_stream_get() {}

/// Same headers as GET, no body.
#[utoipa::path(
    head,
    path = "/api/v3/stream/{source}/{key}",
    params(
        ("source" = String, Path, description = "Audio source: local, jellyfin, navidrome, or plex"),
        ("key" = String, Path, description = "Local file id, remote item id, or Plex part key"),
    ),
    responses(
        (status = 200, description = "Stream headers, no body"),
        (status = 400, description = "Unknown source or unsupported format"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Playback refused"),
        (status = 404, description = "Unknown audio id"),
        (status = 429, description = "Stream capacity exhausted"),
        (status = 502, description = "Remote source failed the read"),
    )
)]
#[allow(dead_code)]
pub(crate) fn doc_stream_head() {}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestIds;

    impl IdGenerator for TestIds {
        fn new_id(&self) -> String {
            "test-id".to_owned()
        }
    }

    fn transcode_media() -> OpenMedia {
        let bytes = b"fake-transcoded-mp3-bytes".to_vec();
        OpenMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: bytes.len() as u64,
            transcoded: true,
            estimated_len: Some(48_000),
            bytes,
        }
    }

    #[test]
    fn transcode_response_sets_length_only_with_estimate() {
        let media = transcode_media();
        let ids = TestIds;

        let estimated = transcode_response(&media, true, false, &ids).expect("renders");
        assert_eq!(
            estimated.headers().get(header::CONTENT_LENGTH),
            Some(&HeaderValue::from_static("48000"))
        );

        let plain = transcode_response(&media, false, false, &ids).expect("renders");
        assert_eq!(plain.headers().get(header::CONTENT_LENGTH), None);
        assert_eq!(
            plain.headers().get(header::ACCEPT_RANGES),
            Some(&HeaderValue::from_static("none"))
        );
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
    fn content_types_cover_audio_minus_wma() {
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
        }
        assert_eq!(content_type_for_extension(".wma"), None);
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
