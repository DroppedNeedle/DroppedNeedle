//! Skiddle API client (Upcoming Events, the UK/IE depth source).
//!
//! Port of v2's Skiddle repository and models. Every request goes through
//! the executor at the `skiddle` rate row, which retries, rests on a rate
//! limit and types each failure as a [`ProviderError`]; Skiddle's own
//! `error != 0` envelope on HTTP 200 is a payload failure that carries
//! only the code. The API key travels as the `api_key` query pair, so it is
//! never in an error or a log line.
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

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RequestPriority;
use crate::client::{decode, parse_base, payload, unusable_base};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

/// Provider key used in [ProviderError] and logs, and its rate row.
pub const SOURCE: &str = "skiddle";

/// Production endpoint.
pub const SKIDDLE_API_URL: &str = "https://www.skiddle.com/api/v1";
/// Largest Skiddle answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Skiddle on the wire: the standard rules.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

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
    totalcount: TotalCount,
    #[serde(default)]
    results: Vec<SkiddleEvent>,
}

/// The Skiddle client, built once with the providers. The key is an
/// argument of each call, read from settings by the caller.
#[derive(Debug, Clone)]
pub struct SkiddleClient {
    executor: Executor,
    base: Option<Url>,
}

impl SkiddleClient {
    /// The client sending through `executor` to the API root at
    /// `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        Self {
            executor,
            base: parse_base(base_url, SOURCE),
        }
    }

    /// How much longer the bucket rests, for a caller that will not wait
    /// that long.
    #[must_use]
    pub fn cooling_for(&self) -> Duration {
        self.executor.limiter().cooling_for()
    }

    /// Whether the bucket rests after a run of failed calls (an outage).
    #[must_use]
    pub fn resting_after_failures(&self) -> bool {
        self.executor.resting_after_failures()
    }

    /// Acts matching `name`; `[]` means Skiddle knows no such act.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_artists(
        &self,
        key: &str,
        name: &str,
        priority: RequestPriority,
    ) -> Result<Vec<SkiddleArtist>, ProviderError> {
        let reply = self
            .get(key, &["artists"], &[("name", name)], priority)
            .await?;
        let decoded: SkiddleArtistsResponse = decode(&reply)?;
        envelope(decoded.error)?;
        Ok(decoded.results)
    }

    /// Upcoming events tagged with one Skiddle artist id (`a=` filter).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn events_for_artist(
        &self,
        key: &str,
        artist_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<SkiddleEvent>, ProviderError> {
        let reply = self
            .get(
                key,
                &["events", "search"],
                &[("a", artist_id), ("description", "1")],
                priority,
            )
            .await?;
        let decoded: SkiddleEventsResponse = decode(&reply)?;
        envelope(decoded.error)?;
        Ok(decoded.results)
    }

    /// Check that `key` is accepted: one small events search, sent once,
    /// whose answer must carry Skiddle's `error` code 0. An answer without
    /// the code is not proof the key works.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn check_key(
        &self,
        key: &str,
        priority: RequestPriority,
    ) -> Result<(), ProviderError> {
        #[derive(Deserialize)]
        struct KeyCheck {
            error: Option<i64>,
        }
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(
            base,
            &["events", "search"],
            &[("limit", "1"), ("api_key", key)],
        )
        .map_err(|_| unusable_base(SOURCE))?
        .trailing_slash()
        .idempotent(false);
        let reply = self.executor.send(request, priority).await?;
        match decode::<KeyCheck>(&reply)?.error {
            Some(code) => envelope(code),
            None => Err(payload(
                SOURCE,
                PayloadKind::Contract,
                "skiddle answered no error code",
            )),
        }
    }

    /// One GET of `segments` under the root, with the key as a query pair.
    /// Skiddle serves these routes with a trailing slash.
    async fn get(
        &self,
        key: &str,
        segments: &[&str],
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let mut query = params.to_vec();
        query.push(("api_key", key));
        let request = Request::get(base, segments, &query)
            .map_err(|_| unusable_base(SOURCE))?
            .trailing_slash();
        self.executor.send(request, priority).await
    }
}

/// Skiddle's own failure envelope on HTTP 200. Only the code is kept: the
/// message is the provider's text and is not ours to log.
fn envelope(code: i64) -> Result<(), ProviderError> {
    if code == 0 {
        return Ok(());
    }
    Err(payload(
        SOURCE,
        PayloadKind::Contract,
        &format!("skiddle answered error {code}"),
    ))
}
