//! Every failure the stream routes answer, the codes they carry and how
//! they render on the wire.

use axum::http::{HeaderValue, StatusCode, header};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use serde_json::json;

use super::engine::AudioSource;
use super::leases::RETRY_AFTER_SECONDS;

/// Unknown file/item id. Message follows the v2 404 detail per source.
pub const STREAM_NOT_FOUND: &str = "STREAM_NOT_FOUND";
/// Playback refused, or a local path outside the music directory.
pub const STREAM_FORBIDDEN: &str = "STREAM_FORBIDDEN";
/// Range header present but unsatisfiable. Carries `Content-Range: bytes */N`.
pub const RANGE_NOT_SATISFIABLE: &str = "RANGE_NOT_SATISFIABLE";
/// Direct or transcode leases exhausted. Carries `Retry-After: 1`.
pub const STREAM_CAPACITY_EXHAUSTED: &str = "STREAM_CAPACITY_EXHAUSTED";

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
