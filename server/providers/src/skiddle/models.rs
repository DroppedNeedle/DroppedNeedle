//! Skiddle wire shapes.
//!
//! Tolerant of unknown fields: serde skips what is not declared. Identity is
//! required, so an act without its id or name, and an event without its id,
//! fail decoding. Every other field is `Option`, or a default that cannot
//! pass for a real value.
//!
//! Quirks seen in the recordings of 2026-10-26 (`tests/fixtures/skiddle/`):
//!
//! - `error` is `0` on success. A bad key is HTTP 400 with `error` 1 and
//!   `errorcode` 997, and the status decides that failure before the body is
//!   read.
//! - `totalcount` is not declared: an int on the artists answer (`3`) and a
//!   string on the events answer (`"15000"`). Nothing reads it.
//! - `cancelled` is a string, never a boolean: `'0'` in every recording. The
//!   v2 port read `'1'` as cancelled; no cancelled gig was recorded.
//! - Ids are strings. Venue coordinates are floats.
//! - Empty strings stand in for absent `ticketUrl`, `cancellationDate` and
//!   `rescheduledDate`.
//! - Key casing is mixed (`eventname`, `ticketUrl`, `EventCode`).

use serde::Deserialize;

/// The envelope of a search answer: Skiddle's error code, then the results.
/// A non-zero code on an HTTP 200 answer is a failure too, as the v2 port
/// had it. No recording on 2026-10-26 shows one.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Answer<T> {
    /// Skiddle's error code, `0` on success. Absent means the answer does
    /// not say, which is not proof of success.
    pub error: Option<i64>,
    /// The matches. Skiddle sends `[]` when it knows none.
    pub results: Option<Vec<T>>,
}

/// `GET /artists/?name=`, the act search.
/// Verified against www.skiddle.com/api/v1/artists/ on 2026-10-26: tests/fixtures/skiddle/artists_search.json.
pub type ArtistsAnswer = Answer<SkiddleArtist>;

/// One act. The id is what events are fetched by, and the name is what the
/// sweep matches on, so both are required.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SkiddleArtist {
    /// Skiddle artist id, a string on the wire.
    pub id: String,
    /// Act name.
    pub name: String,
    /// Spotify artist URI, as sent.
    #[serde(default)]
    pub spotifyartisturl: Option<String>,
}

/// `GET /events/search/` filtered by artist (`a=`), with `description=1`.
/// Verified against www.skiddle.com/api/v1/events/search/ on 2026-10-26: tests/fixtures/skiddle/events_for_artist.json.
pub type EventsAnswer = Answer<SkiddleEvent>;

/// The key check's answer. Only the error code is read.
/// Verified against www.skiddle.com/api/v1/events/search/ on 2026-10-26: tests/fixtures/skiddle/events_key_check.json.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct KeyCheck {
    /// Skiddle's error code.
    pub error: Option<i64>,
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

/// One upcoming event. `id` is required: an event without one fails decoding.
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
