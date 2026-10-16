//! slskd error taxonomy, mirroring the v2 client contract.
//!
//! Ported from v2's slskd client `_check`: 429 maps to
//! [`SlskdError::RateLimited`] ("only one concurrent operation is
//! permitted"), 401/403 to [`SlskdError::Auth`], any
//! other 4xx/5xx to [`SlskdError::Api`]. Like v2, auth failures carry a
//! stripped single-line body snippet at most: never the URL, host, key, or
//! headers (v2 repository `health_check`).

use std::fmt;

/// Failures from the slskd HTTP surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlskdError {
    /// 429: slskd permits only one concurrent operation. Retriable with
    /// backoff; v2 retries it via `with_retry` because
    /// `RateLimitedError` is an `ExternalServiceError`.
    RateLimited,
    /// 401/403: wrong key or key-CIDR deny; slskd returns 401 for both
    /// (v2 mock, issue #193). Never retried, never breaks the circuit
    /// (v2 client `_NON_BREAKING` / `_NON_RETRIABLE`).
    Auth {
        /// The status slskd answered with (401 or 403).
        status: u16,
        /// Stripped single-line body snippet (v2 truncates at 200 chars).
        detail: String,
    },
    /// Any other non-2xx status.
    Api { status: u16, detail: String },
    /// The transport broke down before producing a status line (bad URL,
    /// DNS, connect, TLS, timeout, reset, truncated body).
    Transport(String),
    /// A 2xx body that did not decode into the expected shape.
    Decode(String),
}

impl SlskdError {
    /// Whether the call is worth retrying with backoff (v2 `_RETRIABLE`).
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Transport(_) | Self::Api { .. }
        )
    }

    /// Classify one answered status, truncating the body to a stripped
    /// single-line snippet. The API key, URL, and headers never appear here.
    #[must_use]
    pub fn for_status(status: u16, body: &[u8]) -> Self {
        if status == 429 {
            return Self::RateLimited;
        }
        let detail = snippet(body);
        if status == 401 || status == 403 {
            return Self::Auth { status, detail };
        }
        Self::Api { status, detail }
    }
}

/// Collapse a body to the stripped single-line snippet v2 appends to error
/// messages (at most 200 chars).
fn snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(200).collect()
}

impl fmt::Display for SlskdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited => {
                write!(f, "slskd: only one concurrent operation is permitted")
            }
            Self::Auth { status, detail } => {
                write!(f, "slskd authentication rejected ({status})")?;
                if !detail.is_empty() {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Api { status, detail } => {
                write!(f, "slskd returned HTTP {status}")?;
                if !detail.is_empty() {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            Self::Transport(detail) => write!(f, "slskd request failed: {detail}"),
            Self::Decode(detail) => write!(f, "slskd answered an unexpected body: {detail}"),
        }
    }
}

impl std::error::Error for SlskdError {}
