//! Skiddle API client (Upcoming Events, the UK/IE depth source).
//!
//! Port of v2's Skiddle repository and models. An actionable-failure client:
//! non-200 answers, undecodable bodies, and Skiddle's own `error != 0`
//! envelope on HTTP 200 all raise [SkiddleError] (wiring maps it to 503);
//! HTTP 429 raises [SkiddleError::RateLimited]; raw transport errors never
//! escape as anything but [SkiddleError::Transport].
//!
//! Wire quirks, verified against the live API on 2026-07-06 and preserved
//! here with the v2 decode fixtures as reference
//! (its `sk_*.json` event fixtures):
//!
//! - `cancelled` is the string `'0'`/`'1'`, never a boolean.
//! - Ids are strings, venue coordinates are floats.
//! - Empty strings stand in for absent values (`ticketUrl`,
//!   `cancellationDate`, `rescheduledDate`).
//! - Key casing is mixed (`eventname` vs `ticketUrl` vs `EventCode`).
//! - `totalcount` is an int on the artists endpoint but a string on events
//!   (`"392"`), so both shapes decode.
//!
//! Pacing: every request waits on the limiter given through
//! [SkiddleClient::with_limiter]. The concerts sweep passes a conservative
//! 1 req/s bucket (Skiddle documents only that unspecified daily and hourly
//! caps exist). Retry and the circuit breaker sit with the caller, which
//! maps failures onto [ProviderError].

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::error::{ProviderError, classify_status};
use super::limiter::RateLimiter;

/// Provider key used in [ProviderError] and logs.
pub const SOURCE: &str = "skiddle";

/// Production endpoint. Tests point elsewhere via [SkiddleClient::with_base_url].
pub const SKIDDLE_API_URL: &str = "https://www.skiddle.com/api/v1";

/// One matching act. `id` is the required identity field: a result without
/// one fails the whole decode rather than materializing as an empty act.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SkiddleArtist {
    /// Skiddle artist id, a string on the wire.
    pub id: String,
    /// Act name.
    #[serde(default)]
    pub name: String,
    /// Spotify URI, shared by duplicate listings of one act.
    #[serde(default)]
    pub spotifyartisturl: Option<String>,
}

/// `totalcount` arrives as an int on the artists endpoint and a string on
/// events; both decode since the value is never consumed.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum TotalCount {
    /// Numeric count, as sent by the artists endpoint.
    Int(i64),
    /// String count such as `"392"`, as sent by the events endpoint.
    Text(String),
}

impl Default for TotalCount {
    fn default() -> Self {
        Self::Int(0)
    }
}

/// Top-level artists payload, including Skiddle's own error envelope.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct SkiddleArtistsResponse {
    #[serde(default)]
    error: i64,
    #[serde(default)]
    errormessage: Option<String>,
    #[serde(default)]
    totalcount: TotalCount,
    #[serde(default)]
    results: Vec<SkiddleArtist>,
}

/// Event venue. Coordinates are floats on this API.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SkiddleVenue {
    /// Venue name.
    #[serde(default)]
    pub name: Option<String>,
    /// Town or city.
    #[serde(default)]
    pub town: Option<String>,
    /// Region such as "Merseyside".
    #[serde(default)]
    pub region: Option<String>,
    /// ISO country code such as "GB".
    #[serde(default)]
    pub country: Option<String>,
    /// Latitude in degrees.
    #[serde(default)]
    pub latitude: Option<f64>,
    /// Longitude in degrees.
    #[serde(default)]
    pub longitude: Option<f64>,
}

/// One act on an event lineup. Small gigs carry no lineup at all.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SkiddleEventArtist {
    /// Skiddle artist id.
    #[serde(default)]
    pub artistid: Option<String>,
    /// Act name.
    #[serde(default)]
    pub name: Option<String>,
}

/// One upcoming event. `id` is the required identity field.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SkiddleEvent {
    /// Skiddle event id, a string on the wire.
    pub id: String,
    /// Event title.
    #[serde(default)]
    pub eventname: String,
    /// Venue-local date, YYYY-MM-DD.
    #[serde(default)]
    pub date: Option<String>,
    /// ISO start datetime.
    #[serde(default)]
    pub startdate: Option<String>,
    /// Cancellation flag: the string `'0'` or `'1'`, never a boolean.
    #[serde(default)]
    pub cancelled: Option<String>,
    /// Reschedule date, or an empty string when unset.
    #[serde(default, rename = "rescheduledDate")]
    pub rescheduled_date: Option<String>,
    /// Skiddle listing page; the fallback when no ticket URL is set.
    #[serde(default)]
    pub link: Option<String>,
    /// Ticket URL, or an empty string when unset.
    #[serde(default, rename = "ticketUrl")]
    pub ticket_url: Option<String>,
    /// Event venue.
    #[serde(default)]
    pub venue: Option<SkiddleVenue>,
    /// Tagged lineup, empty for small gigs.
    #[serde(default)]
    pub artists: Vec<SkiddleEventArtist>,
}

impl SkiddleEvent {
    /// True only when the wire flag is exactly the string `'1'`.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.as_deref() == Some("1")
    }

    /// True when a reschedule date is set (blank and missing count as unset).
    pub fn is_rescheduled(&self) -> bool {
        self.rescheduled_date
            .as_deref()
            .is_some_and(|date| !date.trim().is_empty())
    }
}

/// Top-level events payload, including Skiddle's own error envelope.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct SkiddleEventsResponse {
    #[serde(default)]
    error: i64,
    #[serde(default)]
    errormessage: Option<String>,
    #[serde(default)]
    totalcount: TotalCount,
    #[serde(default)]
    results: Vec<SkiddleEvent>,
}

/// What can go wrong on a Skiddle call. Only [SkiddleError::Transport] and
/// [SkiddleError::RateLimited] are retriable; the envelope and decode
/// failures are deterministic for the payload.
#[derive(Debug, Error)]
pub enum SkiddleError {
    /// The request never completed (DNS, TLS, connect, timeout).
    #[error("skiddle request failed: {0}")]
    Transport(String),
    /// The provider answered 429.
    #[error("skiddle rate limit exceeded")]
    RateLimited,
    /// The provider answered a non-200 status (v2 matches strict 200).
    #[error("skiddle returned HTTP {status}")]
    Api {
        /// The status the provider sent back.
        status: u16,
    },
    /// The body was not the documented shape.
    #[error("skiddle response decode failed: {0}")]
    Decode(String),
    /// HTTP 200 carrying Skiddle's own `error != 0` envelope.
    #[error("skiddle search failed: {0}")]
    Envelope(String),
}

/// The Skiddle client. Clone the shared `reqwest::Client` from the HTTP
/// factory at wiring time; the API key travels as an `api_key` query param.
#[derive(Debug, Clone)]
pub struct SkiddleClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    limiter: Option<Arc<RateLimiter>>,
}

impl SkiddleClient {
    /// Client against the production endpoint.
    pub fn new(http: reqwest::Client, api_key: impl Into<String>) -> Self {
        Self::with_base_url(http, api_key, SKIDDLE_API_URL)
    }

    /// Client against an override endpoint (scripted fakes in tests).
    pub fn with_base_url(
        http: reqwest::Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            http,
            api_key: api_key.into(),
            base_url: base_url.into(),
            limiter: None,
        }
    }

    /// Pace every request through `limiter`.
    #[must_use]
    pub fn with_limiter(mut self, limiter: Arc<RateLimiter>) -> Self {
        self.limiter = Some(limiter);
        self
    }

    /// Acts matching `name`; `[]` means Skiddle knows no such act.
    pub async fn search_artists(&self, name: &str) -> Result<Vec<SkiddleArtist>, SkiddleError> {
        let body = self
            .fetch("/artists/", &[("name", name.to_owned())])
            .await?;
        let decoded: SkiddleArtistsResponse = serde_json::from_slice(&body)
            .map_err(|error| SkiddleError::Decode(error.to_string()))?;
        if decoded.error != 0 {
            return Err(SkiddleError::Envelope(envelope_detail(
                decoded.error,
                decoded.errormessage.as_deref(),
            )));
        }
        Ok(decoded.results)
    }

    /// Upcoming events tagged with one Skiddle artist id (`a=` filter).
    pub async fn events_for_artist(
        &self,
        artist_id: &str,
    ) -> Result<Vec<SkiddleEvent>, SkiddleError> {
        let body = self
            .fetch(
                "/events/search/",
                &[("a", artist_id.to_owned()), ("description", "1".to_owned())],
            )
            .await?;
        let decoded: SkiddleEventsResponse = serde_json::from_slice(&body)
            .map_err(|error| SkiddleError::Decode(error.to_string()))?;
        if decoded.error != 0 {
            return Err(SkiddleError::Envelope(envelope_detail(
                decoded.error,
                decoded.errormessage.as_deref(),
            )));
        }
        Ok(decoded.results)
    }

    /// True iff the configured key can reach the events API.
    pub async fn test_connection(&self) -> bool {
        let body = match self
            .fetch("/events/search/", &[("limit", "1".to_owned())])
            .await
        {
            Ok(body) => body,
            Err(_) => return false,
        };
        serde_json::from_slice::<SkiddleEventsResponse>(&body)
            .is_ok_and(|decoded| decoded.error == 0)
    }

    /// One GET with the key merged into the params. Skiddle answers strict
    /// 200 on success; anything else (besides 429) is an API error.
    async fn fetch(&self, path: &str, params: &[(&str, String)]) -> Result<Vec<u8>, SkiddleError> {
        if let Some(limiter) = &self.limiter {
            // One token never exceeds a bucket's burst, so this cannot fail.
            let _ = limiter.acquire().await;
        }
        let mut query: Vec<(&str, String)> = params.to_vec();
        query.push(("api_key", self.api_key.clone()));
        let url = join(&self.base_url, path);
        let response = self
            .http
            .get(url)
            .query(&query)
            .send()
            .await
            .map_err(|error| SkiddleError::Transport(transport_kind(&error).to_owned()))?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(SkiddleError::RateLimited);
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(SkiddleError::Api {
                status: response.status().as_u16(),
            });
        }
        response
            .bytes()
            .await
            .map(|body| body.to_vec())
            .map_err(|error| SkiddleError::Transport(transport_kind(&error).to_owned()))
    }
}

impl From<SkiddleError> for ProviderError {
    fn from(error: SkiddleError) -> Self {
        match error {
            SkiddleError::Transport(message) => Self::Transport {
                provider: SOURCE,
                message,
            },
            SkiddleError::RateLimited => Self::RateLimited {
                provider: SOURCE,
                retry_after: None,
            },
            SkiddleError::Api { status } => {
                classify_status(SOURCE, status, None).unwrap_or(Self::Server {
                    provider: SOURCE,
                    status,
                })
            }
            // Both are deterministic for the payload: a retry gets the same answer.
            SkiddleError::Decode(message) | SkiddleError::Envelope(message) => Self::Payload {
                provider: SOURCE,
                message,
            },
        }
    }
}

/// Join a base URL and a path without doubling the slash.
fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// Envelope detail: the message when set and non-blank, else the numeric
/// code, mirroring v2's `errormessage or error` fallback.
fn envelope_detail(code: i64, message: Option<&str>) -> String {
    match message {
        Some(text) if !text.is_empty() => text.to_owned(),
        _ => code.to_string(),
    }
}

/// Short transport failure kind. Only the kind is kept, never the URL, the
/// same shape as v2's exception-type-only message.
fn transport_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    }
}
