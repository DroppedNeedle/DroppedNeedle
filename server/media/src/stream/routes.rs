//! Stream-gateway routes: one source-keyed GET/HEAD surface over the engine.
//!
//! This file owns the route shapes, the typed query, the handlers and the
//! mapping from an engine fault to the wire. The rest of the stream's HTTP
//! layer and what it reads sit beside it:
//!
//! * [`engine`](super::engine): the sources, one read's request and answer,
//!   the [`StreamEngine`] trait the routes read through and the audio
//!   content-type table. [`super::gateway::Gateway`] implements the trait
//!   (byte sourcing, leases and transcode execution). HEAD never reads a
//!   body and never starts ffmpeg.
//! * [`framing`](super::framing): range parsing and the byte-exact
//!   200/206/HEAD framing every stream route shares.
//! * [`error`](super::error): the failures these routes answer, including
//!   the 416 with `Content-Range: bytes */N` and the 429 with
//!   `Retry-After: 1`.
//!
//! # Routes (relative; `MediaSetup` nests these under `/api/v3`)
//!
//! ```text
//! GET  /stream/{source}/{*key}   full or ranged audio bytes
//! HEAD /stream/{source}/{*key}   same headers, no body
//! GET  /playback/conversion      whether a stream that asks for a format is converted
//! ```
//!
//! `source` is one of `local`, `jellyfin`, `navidrome`, `plex`. `key` is the
//! source identifier: a local file id, a Jellyfin/Navidrome item id, or a Plex
//! part key (which may itself contain slashes, hence the wildcard). Query
//! keys are `format`, `max_bitrate`, `start_seconds` and
//! `estimate_content_length` (see [`StreamQuery`]).
//!
//! A `format` the bytes would not be in is refused here with a 409
//! `TRANSCODE_UNAVAILABLE` instead of answered with the original, so the web
//! player can tell a conversion from a pass-through. The compat routes keep
//! the original-bytes fallback their clients expect.
//!
//! # Deep links
//!
//! Players link to `/api/v3/stream/{source}/{key}` with those query
//! keys. The shapes stay small and stable: one handler shape
//! `(State, User, Path, HeaderMap, ValidQuery)`, path params only
//! for identity, query only for transcode hints. A new gateway capability
//! should be a new query key, not a new route.
//!
//! # Mounting
//!
//! [`routes`] mounts [`stream_get_route`] and [`stream_head_route`], the
//! production forms of the generic handlers, and [`conversion_route`], for
//! any signed-in user. Playback
//! reporting (`start`/`progress`/`stop`/`scrobble`/`now-playing`/`stopped`)
//! lives in `playback`, not this router.

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{FromRef, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use droppedneedle_auth::{Principal, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_kernel::mount::{Routes, routes};

use super::engine::{
    AudioSource, Conversion, StreamEngine, StreamFault, StreamMedia, StreamOpen, StreamParams,
    format_for_content_type,
};
use super::error::StreamError;
use super::framing::{FramedBody, frame, range_header};
use crate::MediaEngine;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Whether the web player's format asks are converted.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlaybackConversion {
    /// On, turned off by an admin, or impossible without ffmpeg.
    pub conversion: Conversion,
}

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
    /// Requested output codec (`mp3`, `opus`, `flac`, `raw`, or unset).
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

/// The stream route, GET and HEAD, for any signed-in user. The
/// Plex-compatible wildcard key also matches single-segment ids; the
/// document spells the wildcard the same way, so it names the route as
/// served.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    StreamState<MediaEngine>: FromRef<S>,
{
    Routes::new()
        .user(routes!(stream_get_route, stream_head_route))
        .user(routes!(conversion_route))
}

/// Whether a stream that asks for a format is converted, so the web player
/// knows before it asks.
#[utoipa::path(
    get,
    tag = "stream",
    path = "/api/v3/playback/conversion",
    responses(
        (status = 200, description = "Whether converting is on", body = PlaybackConversion),
        (status = 401, description = "Not authenticated"),
    ),
    operation_id = "playback_conversion"
)]
pub async fn conversion_route(
    State(state): State<StreamState<MediaEngine>>,
    _user: User,
) -> Result<Json<PlaybackConversion>, ApiError> {
    let conversion = state
        .engine
        .conversion()
        .ok_or_else(|| ApiError::internal("connect_apps settings cannot be read"))?;
    Ok(Json(PlaybackConversion { conversion }))
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
        ("format" = Option<String>, Query, description = "Requested output codec: mp3, opus, flac, or raw"),
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
        (status = 409, description = "A format was asked for and the server is not converting"),
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
        ("format" = Option<String>, Query, description = "Requested output codec: mp3, opus, flac, or raw"),
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
        (status = 409, description = "A format was asked for and the server is not converting"),
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
    if unconverted(query.format.as_deref(), &media) {
        return Err(StreamError::Unconverted);
    }

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

/// True when the request named a format and the gateway landed direct on
/// bytes in another one: converting is off or there is no ffmpeg. `raw`
/// asks for the original, and a direct landing already in the asked format
/// is what was asked for.
fn unconverted(asked: Option<&str>, media: &StreamMedia) -> bool {
    let Some(asked) = asked.filter(|format| !format.is_empty()) else {
        return false;
    };
    if asked.eq_ignore_ascii_case("raw") || media.transcoded {
        return false;
    }
    !format_for_content_type(&media.content_type).eq_ignore_ascii_case(asked)
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
