//! The one error every download client and indexer maps its HTTP failures
//! into: slskd, SABnzbd, Newznab and Prowlarr.
//!
//! Statuses map the same way for each: 401 and 403 are a refused key, 404
//! and 410 a missing item, 429 a rate limit with the server's
//! `Retry-After`, any other 4xx a refused request, and 5xx or no answer at
//! all an unavailable service. A body that does not decode is `Decode`.
//! Details never hold a request URL, a key or a header.

use std::fmt;
use std::time::Duration;

/// The outside service a [`ClientError`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Slskd,
    Sabnzbd,
    Newznab,
    Prowlarr,
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Slskd => "slskd",
            Self::Sabnzbd => "SABnzbd",
            Self::Newznab => "Newznab indexer",
            Self::Prowlarr => "Prowlarr",
        })
    }
}

/// A download client or indexer call that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// The service refused the API key: HTTP 401 or 403, or an answer
    /// that names the key as the problem. Never retried.
    #[error("{service} rejected the API key{}", status_suffix(*status))]
    Auth {
        service: Service,
        /// The HTTP status, when the refusal came as one.
        status: Option<u16>,
        /// The service's own words, at most a short line.
        detail: String,
    },
    /// HTTP 404 or 410.
    #[error("{service} has no such item")]
    NotFound { service: Service },
    /// HTTP 400 or another 4xx: the service refused the request itself.
    #[error("{service} refused the request{}", detail_suffix(detail))]
    BadRequest { service: Service, detail: String },
    /// HTTP 429, or an answer saying a request limit was reached.
    #[error("{service} is rate limiting requests")]
    RateLimited {
        service: Service,
        /// How long the service asked to wait, when it said.
        retry_after: Option<Duration>,
    },
    /// HTTP 5xx, or no answer at all.
    #[error("{service} is unavailable{}", detail_suffix(detail))]
    Unavailable { service: Service, detail: String },
    /// An answer that is not the documented shape.
    #[error("{service} answered an unexpected body: {detail}")]
    Decode { service: Service, detail: String },
}

fn status_suffix(status: Option<u16>) -> String {
    status.map(|code| format!(" ({code})")).unwrap_or_default()
}

fn detail_suffix(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

impl ClientError {
    /// Classify a non-2xx answer. `body` is cut to one short line; the
    /// caller scrubs it first when the service may echo a secret.
    #[must_use]
    pub fn for_status(
        service: Service,
        status: u16,
        retry_after: Option<Duration>,
        body: &[u8],
    ) -> Self {
        match status {
            401 | 403 => Self::Auth {
                service,
                status: Some(status),
                detail: snippet(body),
            },
            404 | 410 => Self::NotFound { service },
            429 => Self::RateLimited {
                service,
                retry_after,
            },
            400..=499 => Self::BadRequest {
                service,
                detail: snippet(body),
            },
            _ => Self::Unavailable {
                service,
                detail: format!("HTTP {status}"),
            },
        }
    }

    /// No answer: the transport broke before a status line.
    #[must_use]
    pub fn transport(service: Service, detail: impl Into<String>) -> Self {
        Self::Unavailable {
            service,
            detail: detail.into(),
        }
    }

    /// An answer that did not decode.
    #[must_use]
    pub fn decode(service: Service, detail: impl Into<String>) -> Self {
        Self::Decode {
            service,
            detail: detail.into(),
        }
    }

    /// The service this came from.
    #[must_use]
    pub fn service(&self) -> Service {
        match self {
            Self::Auth { service, .. }
            | Self::NotFound { service }
            | Self::BadRequest { service, .. }
            | Self::RateLimited { service, .. }
            | Self::Unavailable { service, .. }
            | Self::Decode { service, .. } => *service,
        }
    }

    /// How long the service asked to wait, for a rate limit that said.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Whether the service refused the key.
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth { .. })
    }
}

/// `Retry-After` as a wait. Only the delta-seconds form is read; the
/// HTTP-date form, which would need the wall clock, reads as `None` and
/// the caller falls back to its own backoff.
#[must_use]
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    let seconds = raw.parse::<f64>().ok()?;
    Duration::try_from_secs_f64(seconds).ok()
}

/// A body cut to one stripped line of at most 200 characters.
fn snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(200).collect()
}
