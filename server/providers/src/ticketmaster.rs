//! Ticketmaster Discovery v2 client (Upcoming Events).
//!
//! Port of v2's Ticketmaster repository and models. Every request goes
//! through the executor at the `ticketmaster` rate row, which retries,
//! rests on a rate limit and types each failure as a [`ProviderError`].
//! The API key travels as the `apikey` query pair, so it is never in an
//! error or a log line: the executor prints neither the URL nor the body.
//!
//! Wire quirks, verified against the live API on 2026-07-06 and preserved
//! here with the v2 decode fixtures as reference
//! (its `tm_*.json` event fixtures):
//!
//! - Ticketmaster omits `_embedded` entirely when there are zero results,
//!   so a missing block decodes as "no results", never as an error.
//! - Everything is tolerant-by-default: attractions without
//!   `externalLinks`, venues without `location`, and events without
//!   `_embedded` must all decode cleanly.
//! - Venue coordinates arrive as strings (`"51.46368200"`) and stay that
//!   way; parsing them is the caller's choice.
//! - `externalLinks` is a dynamic key/value map; the key this client
//!   consumes is `musicbrainz`, whose ids arrive padded or cased and are
//!   trimmed and lowercased on read, blanks skipped.
//!
//! - A spent daily allowance is a 429 whose body names
//!   `policies.ratelimit.QuotaViolation`; the bucket then rests until the
//!   `Rate-Limit-Reset` time, at most an hour. The docs' example value
//!   (`1453180594367`) is unix milliseconds. Read on the getting-started
//!   page on 2026-10-20. That body's fault string echoes the key, so it is
//!   never logged or kept.
//!
//! Every page of a paged answer is its own request, retried on its own.
//! The 5,000/day allowance is kept by the sweep's size, not by this client.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{Answer, Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

/// Provider key used in [ProviderError] and logs, and its rate row.
pub const SOURCE: &str = "ticketmaster";

/// Production endpoint.
pub const TICKETMASTER_API_URL: &str = "https://app.ticketmaster.com/discovery/v2";
/// Events requested per page, kept from v2.
pub const PAGE_SIZE: u32 = 200;
/// Pagination follow cap: 600 events per attraction covers the largest tours.
/// A deeper result set is truncated, loudly (see the warning log).
pub const MAX_PAGES: u32 = 3;
/// Largest Ticketmaster answer read. A page holds up to [`PAGE_SIZE`]
/// events, each with its own image list, venue and attraction blocks, so
/// a full page can pass 4 MiB.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// How long a spent daily allowance rests the bucket when the answer
/// names no reset time, and the longest rest it may ask for.
const QUOTA_REST: Duration = Duration::from_secs(3600);

/// Ticketmaster on the wire: the standard rules plus the daily allowance
/// hook.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(30),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: QUOTA_REST,
    classify,
};

/// A 429 that names the spent daily allowance rests until its reset;
/// everything else follows the standard rules.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let reply = answer.reply;
    if reply.status() == 429
        && String::from_utf8_lossy(reply.bytes()).contains("policies.ratelimit.QuotaViolation")
    {
        return Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: Some(quota_reset(reply, answer.now_unix).unwrap_or(QUOTA_REST)),
        });
    }
    standard_classify(answer)
}

/// The wait until `Rate-Limit-Reset`: unix milliseconds as documented, or
/// unix seconds. `None` when missing, unreadable or past.
fn quota_reset(reply: &Reply, now_unix: i64) -> Option<Duration> {
    let stamp: u64 = reply.header("rate-limit-reset")?.trim().parse().ok()?;
    let reset = if stamp > 100_000_000_000 {
        stamp / 1000
    } else {
        stamp
    };
    let now = u64::try_from(now_unix).ok()?;
    (reset > now).then(|| Duration::from_secs(reset - now))
}

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

/// Venue coordinates. The wire carries these as strings (`"51.46368200"`),
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

/// One decoded page of events plus the server's page count.
#[derive(Debug, Clone, PartialEq)]
pub struct TmEventsPage {
    /// Events on this page.
    pub events: Vec<TmEvent>,
    /// Total pages available (1 when the answer carries no page block).
    pub total_pages: u32,
}

/// Follow an attraction's pages through `fetch_page`, up to [MAX_PAGES].
/// A deeper result set is truncated with a warning, never silently.
pub async fn collect_event_pages<F, Fut, E>(
    attraction_id: &str,
    mut fetch_page: F,
) -> Result<Vec<TmEvent>, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<TmEventsPage, E>>,
{
    let mut events = Vec::new();
    for page_number in 0..MAX_PAGES {
        let page = fetch_page(page_number).await?;
        events.extend(page.events);
        if page_number + 1 >= page.total_pages {
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

/// The Ticketmaster client, built once with the providers. The key is an
/// argument of each call, read from settings by the caller.
#[derive(Debug, Clone)]
pub struct TicketmasterClient {
    executor: Executor,
    base: Option<Url>,
}

impl TicketmasterClient {
    /// The client sending through `executor` to the Discovery root at
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

    /// Music attractions matching `keyword`; `[]` means Ticketmaster knows
    /// no such act.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_attractions(
        &self,
        key: &str,
        keyword: &str,
        priority: RequestPriority,
    ) -> Result<Vec<TmAttraction>, ProviderError> {
        let reply = self
            .get(
                key,
                "attractions.json",
                &[
                    ("keyword", keyword),
                    ("classificationName", "Music"),
                    ("size", "50"),
                ],
                priority,
            )
            .await?;
        let decoded: TmAttractionsResponse = decode(&reply)?;
        Ok(decoded
            .embedded
            .map(|inner| inner.attractions)
            .unwrap_or_default())
    }

    /// All upcoming events for one attraction, worldwide, oldest first,
    /// through [collect_event_pages].
    ///
    /// # Errors
    ///
    /// The typed failure of the first page that failed.
    pub async fn events_for_attraction(
        &self,
        key: &str,
        attraction_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<TmEvent>, ProviderError> {
        collect_event_pages(attraction_id, |page| {
            self.events_page(key, attraction_id, page, priority)
        })
        .await
    }

    /// One page (zero-based) of an attraction's upcoming events.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn events_page(
        &self,
        key: &str,
        attraction_id: &str,
        page: u32,
        priority: RequestPriority,
    ) -> Result<TmEventsPage, ProviderError> {
        let size = PAGE_SIZE.to_string();
        let page = page.to_string();
        let reply = self
            .get(
                key,
                "events.json",
                &[
                    ("attractionId", attraction_id),
                    ("sort", "date,asc"),
                    ("size", &size),
                    ("page", &page),
                ],
                priority,
            )
            .await?;
        let decoded: TmEventsResponse = decode(&reply)?;
        Ok(TmEventsPage {
            events: decoded
                .embedded
                .map(|inner| inner.events)
                .unwrap_or_default(),
            total_pages: decoded.page.map(|page| page.total_pages).unwrap_or(1),
        })
    }

    /// Check that `key` is accepted: one small attractions search, sent
    /// once.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn check_key(
        &self,
        key: &str,
        priority: RequestPriority,
    ) -> Result<(), ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(
            base,
            &["attractions.json"],
            &[("keyword", "test"), ("size", "1"), ("apikey", key)],
        )
        .map_err(|_| unusable_base(SOURCE))?
        .idempotent(false);
        self.executor.send(request, priority).await.map(drop)
    }

    /// One GET of `endpoint` under the root, with the key as a query pair.
    async fn get(
        &self,
        key: &str,
        endpoint: &str,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let mut query = params.to_vec();
        query.push(("apikey", key));
        let request = Request::get(base, &[endpoint], &query).map_err(|_| unusable_base(SOURCE))?;
        self.executor.send(request, priority).await
    }
}
