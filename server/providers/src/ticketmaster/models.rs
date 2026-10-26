//! Ticketmaster Discovery wire shapes, read by the client in `ticketmaster.rs`.
//!
//! Tolerant of unknown fields. Identity fields fail the decode when missing:
//! an attraction or event `id`, and an attraction `name` (the exact-name match
//! keys on it). A MusicBrainz link with no id is supporting evidence, so it is
//! dropped and the act still reads. Everything else is optional, and a missing
//! `_embedded` is an answer with no results.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

/// The answer to `attractions.json`.
///
/// Verified against app.ticketmaster.com/discovery/v2/attractions.json on 2026-10-26: tests/fixtures/ticketmaster/attractions_search_radiohead.json.
/// Zero results omit `_embedded` (attractions_no_results.json, 2026-10-26).
/// A made-up key answers 401 (attractions_invalid_key.json, 2026-10-26).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TmAttractionsResponse {
    #[serde(default, rename = "_embedded")]
    pub(super) embedded: Option<TmAttractionsEmbedded>,
}

/// The `_embedded` block of an attractions answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmAttractionsEmbedded {
    /// Matching acts.
    #[serde(default)]
    pub attractions: Vec<TmAttraction>,
}

/// One act. `id` and `name` are required.
#[derive(Debug, Clone, PartialEq)]
pub struct TmAttraction {
    /// Ticketmaster attraction id.
    pub id: String,
    /// Act name.
    pub name: String,
    /// The `musicbrainz` links that carry an id, when the act carries a link
    /// map. Other link kinds are not read.
    pub external_links: Option<HashMap<String, Vec<TmExternalId>>>,
}

impl TmAttraction {
    /// MusicBrainz artist ids for this act, trimmed, lowercased, blanks
    /// skipped. Empty when the act has no `musicbrainz` links, as several
    /// acts in `attractions_search_radiohead.json` do.
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

    fn from_wire(wire: AttractionWire) -> Self {
        let external_links = wire.external_links.map(|links| {
            let musicbrainz = links
                .musicbrainz
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .filter_map(|link| link.id)
                .map(|id| TmExternalId { id: Some(id) })
                .collect();
            HashMap::from([("musicbrainz".to_owned(), musicbrainz)])
        });
        Self {
            id: wire.id,
            name: wire.name,
            external_links,
        }
    }
}

impl<'de> Deserialize<'de> for TmAttraction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        AttractionWire::deserialize(deserializer).map(Self::from_wire)
    }
}

/// An act as the wire sends it. Only the `musicbrainz` links are read. A null
/// list, a null entry or a link with no id is dropped rather than failing the
/// answer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttractionWire {
    id: String,
    name: String,
    external_links: Option<LinksWire>,
}

#[derive(Deserialize)]
struct LinksWire {
    #[serde(default)]
    musicbrainz: Option<Vec<Option<LinkWire>>>,
}

#[derive(Deserialize)]
struct LinkWire {
    #[serde(default)]
    id: Option<String>,
}

/// One `musicbrainz` link of an act that carries an id. Decoding always sets
/// it; `None` is only for values built by hand.
#[derive(Debug, Clone, PartialEq)]
pub struct TmExternalId {
    /// The MusicBrainz artist id as sent.
    pub id: Option<String>,
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

/// The paging block of an events answer. The client reads only the page
/// count; when it is absent, the client reads one page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmPage {
    /// Total pages available.
    #[serde(default)]
    pub total_pages: Option<u32>,
}

/// The `_embedded` block of an events answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmEventsEmbedded {
    /// Events on this page.
    #[serde(default)]
    pub events: Vec<TmEvent>,
}

/// The answer to `events.json`. Zero results carry a page with `totalPages`
/// 0 and no `_embedded`.
///
/// Verified against app.ticketmaster.com/discovery/v2/events.json on 2026-10-26: tests/fixtures/ticketmaster/events_fontaines_dc.json.
/// Zero results: tests/fixtures/ticketmaster/events_radiohead_none.json (2026-10-26).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TmEventsResponse {
    #[serde(default, rename = "_embedded")]
    pub(super) embedded: Option<TmEventsEmbedded>,
    #[serde(default)]
    pub(super) page: Option<TmPage>,
}
