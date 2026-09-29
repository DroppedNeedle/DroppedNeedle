//! Ticketmaster Discovery v2 client (Upcoming Events).
//!
//! Port of `backend/repositories/ticketmaster_repository.py` and
//! `backend/repositories/ticketmaster_models.py`. An actionable-failure
//! client: non-200 answers and undecodable bodies raise [TicketmasterError]
//! (wiring maps it to 503), HTTP 429 raises
//! [TicketmasterError::RateLimited] carrying the server's retry hint, and raw
//! transport errors never escape as anything but
//! [TicketmasterError::Transport].
//!
//! Wire quirks, verified against the live API on 2026-07-06 and preserved
//! here with the v2 decode fixtures as reference
//! (`backend/tests/fixtures/events/tm_*.json`):
//!
//! - Ticketmaster omits `_embedded` entirely when there are zero results,
//!   so a missing block decodes as "no results", never as an error.
//! - Everything is tolerant-by-default: attractions without
//!   `externalLinks`, venues without `location`, and events without
//!   `_embedded` must all decode cleanly.
//! - Venue coordinates arrive as STRINGS (`"51.46368200"`) and stay that
//!   way; parsing them is the caller's choice.
//! - `externalLinks` is a dynamic key/value map; the key this client
//!   consumes is `musicbrainz`, whose ids arrive padded or cased and are
//!   trimmed and lowercased on read, blanks skipped.
//!
//! Seam for s5-core: this client sends one request per call (plus pagination
//! follow-ups). Retry (3 attempts over transport and rate-limit failures
//! only), the 2 req/s pacing (Ticketmaster's docs state 5 req/s in one place
//! and 2 req/s in another; v2 encodes the documented floor), and the circuit
//! breaker live in the shared resilience layer, which matches on
//! [TicketmasterError]. The 5,000/day quota stays enforced by sweep sizing,
//! not by this client.

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

/// Production endpoint. Tests point elsewhere via [TicketmasterClient::with_base_url].
pub const TICKETMASTER_API_URL: &str = "https://app.ticketmaster.com/discovery/v2";
/// Events requested per page, kept from v2.
pub const PAGE_SIZE: u32 = 200;
/// Pagination follow cap: 600 events per attraction covers the largest tours.
/// A deeper result set is truncated, loudly (see the warning log).
pub const MAX_PAGES: u32 = 3;

/// One external id link inside an attraction's `externalLinks` map.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmExternalId {
    /// The external id, such as a MusicBrainz artist MBID.
    #[serde(default)]
    pub id: Option<String>,
}

/// One matching act. `id` is the required identity field: a result without
/// one fails the whole decode rather than materializing as an empty act.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmAttraction {
    /// Ticketmaster attraction id.
    pub id: String,
    /// Act name.
    #[serde(default)]
    pub name: String,
    /// Dynamic link map; the consumed key is `musicbrainz`.
    #[serde(default)]
    pub external_links: Option<HashMap<String, Vec<TmExternalId>>>,
}

impl TmAttraction {
    /// MusicBrainz artist ids for this act, trimmed, lowercased, blanks
    /// skipped. Empty when the act carries no `musicbrainz` links (the
    /// DJ-set sibling trap in the live fixture).
    pub fn musicbrainz_ids(&self) -> Vec<String> {
        self.external_links
            .as_ref()
            .and_then(|links| links.get("musicbrainz"))
            .map(|links| {
                links
                    .iter()
                    .filter_map(|link| link.id.as_deref())
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(|id| id.to_lowercase())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The `_embedded` block of an attractions answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmAttractionsEmbedded {
    /// Matching acts.
    #[serde(default)]
    pub attractions: Vec<TmAttraction>,
}

/// Top-level attractions payload. `_embedded` is simply absent on zero
/// results, which decodes as an empty search.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TmAttractionsResponse {
    #[serde(default, rename = "_embedded")]
    embedded: Option<TmAttractionsEmbedded>,
}

/// An event's start date, venue-local and absolute side by side.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmDateStart {
    /// Venue-local date, YYYY-MM-DD.
    #[serde(default)]
    pub local_date: Option<String>,
    /// Absolute start datetime.
    #[serde(default)]
    pub date_time: Option<String>,
}

/// Ticket-sale status of an event, such as `onsale`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmDateStatus {
    /// Status code.
    #[serde(default)]
    pub code: Option<String>,
}

/// An event's dates block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmDates {
    /// Start dates.
    #[serde(default)]
    pub start: Option<TmDateStart>,
    /// Sale status.
    #[serde(default)]
    pub status: Option<TmDateStatus>,
}

/// Any named sub-object (city, state).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmNamed {
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// A venue's country block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmVenueCountry {
    /// ISO country code such as "GB".
    #[serde(default)]
    pub country_code: Option<String>,
}

/// Venue coordinates. The wire carries these as STRINGS (`"51.46368200"`),
/// so they stay strings here too.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmLocation {
    /// Latitude as sent on the wire.
    #[serde(default)]
    pub latitude: Option<String>,
    /// Longitude as sent on the wire.
    #[serde(default)]
    pub longitude: Option<String>,
}

/// One event venue. Venues without `location` decode cleanly.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmVenue {
    /// Venue name.
    #[serde(default)]
    pub name: Option<String>,
    /// Venue city.
    #[serde(default)]
    pub city: Option<TmNamed>,
    /// Venue state.
    #[serde(default)]
    pub state: Option<TmNamed>,
    /// Venue country.
    #[serde(default)]
    pub country: Option<TmVenueCountry>,
    /// Venue coordinates, when given.
    #[serde(default)]
    pub location: Option<TmLocation>,
}

/// The `_embedded` block of an event: its venues and lineup.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmEventEmbedded {
    /// Event venues.
    #[serde(default)]
    pub venues: Vec<TmVenue>,
    /// Lineup attractions.
    #[serde(default)]
    pub attractions: Vec<TmAttraction>,
}

/// One upcoming event. `id` is the required identity field.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmEvent {
    /// Ticketmaster event id.
    pub id: String,
    /// Event title.
    #[serde(default)]
    pub name: String,
    /// Ticketmaster listing URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Dates block.
    #[serde(default)]
    pub dates: Option<TmDates>,
    /// Venues and lineup, when the event carries any.
    #[serde(default, rename = "_embedded")]
    pub embedded: Option<TmEventEmbedded>,
}

/// Pagination cursor of an events answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmPage {
    /// Page size the server used.
    #[serde(default)]
    pub size: u32,
    /// Total matching events.
    #[serde(default)]
    pub total_elements: u32,
    /// Total pages available.
    #[serde(default)]
    pub total_pages: u32,
    /// Zero-based number of this page.
    #[serde(default)]
    pub number: u32,
}

/// The `_embedded` block of an events answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmEventsEmbedded {
    /// Events on this page.
    #[serde(default)]
    pub events: Vec<TmEvent>,
}

/// Top-level events payload.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TmEventsResponse {
    #[serde(default, rename = "_embedded")]
    embedded: Option<TmEventsEmbedded>,
    #[serde(default)]
    page: Option<TmPage>,
}

/// What can go wrong on a Ticketmaster call. Only
/// [TicketmasterError::Transport] and [TicketmasterError::RateLimited] are
/// retriable; API and decode failures are deterministic for the call.
#[derive(Debug, Error)]
pub enum TicketmasterError {
    /// The request never completed (DNS, TLS, connect, timeout).
    #[error("ticketmaster request failed: {0}")]
    Transport(String),
    /// The provider answered 429, with the server's retry hint when it sent
    /// a usable one.
    #[error("ticketmaster rate limit exceeded")]
    RateLimited {
        /// Parsed `Retry-After` seconds, when positive and numeric.
        retry_after_secs: Option<f64>,
    },
    /// The provider answered a non-200 status (v2 matches strict 200).
    #[error("ticketmaster returned HTTP {status}")]
    Api {
        /// The status the provider sent back.
        status: u16,
    },
    /// The body was not the documented shape.
    #[error("ticketmaster response decode failed: {0}")]
    Decode(String),
}

/// The Ticketmaster client. Clone the shared `reqwest::Client` from the HTTP
/// factory at wiring time; the API key travels as an `apikey` query param.
#[derive(Debug, Clone)]
pub struct TicketmasterClient {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl TicketmasterClient {
    /// Client against the production endpoint.
    pub fn new(http: reqwest::Client, api_key: impl Into<String>) -> Self {
        Self::with_base_url(http, api_key, TICKETMASTER_API_URL)
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
        }
    }

    /// Music attractions matching `keyword`; `[]` means Ticketmaster knows
    /// no such act.
    pub async fn search_attractions(
        &self,
        keyword: &str,
    ) -> Result<Vec<TmAttraction>, TicketmasterError> {
        let body = self
            .fetch(
                "/attractions.json",
                &[
                    ("keyword", keyword.to_owned()),
                    ("classificationName", "Music".to_owned()),
                    ("size", "50".to_owned()),
                ],
            )
            .await?;
        let decoded: TmAttractionsResponse = serde_json::from_slice(&body)
            .map_err(|error| TicketmasterError::Decode(error.to_string()))?;
        Ok(decoded
            .embedded
            .map(|inner| inner.attractions)
            .unwrap_or_default())
    }

    /// All upcoming events for one attraction, worldwide, oldest first.
    ///
    /// Follows pagination up to [MAX_PAGES]; a deeper result set is
    /// truncated with a warning, never silently.
    pub async fn events_for_attraction(
        &self,
        attraction_id: &str,
    ) -> Result<Vec<TmEvent>, TicketmasterError> {
        let mut events = Vec::new();
        for page_number in 0..MAX_PAGES {
            let body = self
                .fetch(
                    "/events.json",
                    &[
                        ("attractionId", attraction_id.to_owned()),
                        ("sort", "date,asc".to_owned()),
                        ("size", PAGE_SIZE.to_string()),
                        ("page", page_number.to_string()),
                    ],
                )
                .await?;
            let decoded: TmEventsResponse = serde_json::from_slice(&body)
                .map_err(|error| TicketmasterError::Decode(error.to_string()))?;
            if let Some(inner) = decoded.embedded {
                events.extend(inner.events);
            }
            let total_pages = decoded
                .page
                .as_ref()
                .map(|page| page.total_pages)
                .unwrap_or(1);
            if page_number + 1 >= total_pages {
                return Ok(events);
            }
        }
        tracing::warn!(
            attraction_id,
            MAX_PAGES,
            event_count = events.len(),
            "ticketmaster events truncated at page cap",
        );
        Ok(events)
    }

    /// True iff the configured key can reach the Discovery API. Like v2,
    /// this checks reachability only and never decodes the body.
    pub async fn test_connection(&self) -> bool {
        self.fetch(
            "/attractions.json",
            &[("keyword", "test".to_owned()), ("size", "1".to_owned())],
        )
        .await
        .is_ok()
    }

    /// One GET with the key merged into the params. Ticketmaster answers
    /// strict 200 on success; anything else (besides 429) is an API error.
    async fn fetch(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Vec<u8>, TicketmasterError> {
        let mut query: Vec<(&str, String)> = params.to_vec();
        query.push(("apikey", self.api_key.clone()));
        let url = join(&self.base_url, path);
        let response = self
            .http
            .get(url)
            .query(&query)
            .send()
            .await
            .map_err(|error| TicketmasterError::Transport(transport_kind(&error).to_owned()))?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(TicketmasterError::RateLimited {
                retry_after_secs: parse_retry_after(
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok()),
                ),
            });
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(TicketmasterError::Api {
                status: response.status().as_u16(),
            });
        }
        response
            .bytes()
            .await
            .map(|body| body.to_vec())
            .map_err(|error| TicketmasterError::Transport(transport_kind(&error).to_owned()))
    }
}

/// Parse a `Retry-After` value into seconds. Only positive numbers count;
/// missing, unparsable, zero, and negative values all mean "no hint", kept
/// from v2's `_parse_retry_after`.
pub fn parse_retry_after(value: Option<&str>) -> Option<f64> {
    let seconds: f64 = value?.trim().parse().ok()?;
    if seconds > 0.0 { Some(seconds) } else { None }
}

/// Join a base URL and a path without doubling the slash.
fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
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
