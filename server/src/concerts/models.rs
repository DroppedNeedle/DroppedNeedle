//! Wire shapes for the concerts routes, plus the two enums the feed stores.
//!
//! Field names and meanings follow v2's `api/v1/schemas/following.py`, so
//! the city picker and the events page read the same fields they always did.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Where a feed row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// Ticketmaster Discovery.
    Ticketmaster,
    /// Skiddle (UK and Ireland).
    Skiddle,
}

impl EventSource {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ticketmaster => "ticketmaster",
            Self::Skiddle => "skiddle",
        }
    }

    /// Parse the stored spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ticketmaster" => Some(Self::Ticketmaster),
            "skiddle" => Some(Self::Skiddle),
            _ => None,
        }
    }
}

/// Whether the gig is still on. Ticketmaster's `postponed` reads as
/// rescheduled: the gig is not happening on its listed date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConcertStatus {
    /// On as listed.
    Scheduled,
    /// Called off.
    Cancelled,
    /// Moved or postponed.
    Rescheduled,
}

impl ConcertStatus {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Cancelled => "cancelled",
            Self::Rescheduled => "rescheduled",
        }
    }

    /// Parse the stored spelling; anything unknown reads as scheduled.
    pub fn parse(value: &str) -> Self {
        match value {
            "cancelled" => Self::Cancelled,
            "rescheduled" => Self::Rescheduled,
            _ => Self::Scheduled,
        }
    }
}

/// One upcoming gig in one of the caller's cities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Concert {
    /// Followed artist's MBID (lowercase in library sweep scope).
    pub artist_mbid: String,
    /// Artist name as followed.
    pub artist_name: String,
    /// Event title from the source.
    pub event_name: String,
    /// Venue-local date, `YYYY-MM-DD`.
    pub local_date: String,
    /// Whether the gig is still on.
    pub status: ConcertStatus,
    /// Source of the listing.
    pub source: EventSource,
    /// Listing id, unique per source.
    pub source_event_id: String,
    /// The saved city this gig matched.
    pub matched_city: String,
    /// Venue name.
    pub venue_name: Option<String>,
    /// Venue town or city.
    pub city: Option<String>,
    /// Venue region.
    pub region: Option<String>,
    /// Venue country code.
    pub country_code: Option<String>,
    /// Start time, ISO, when the source gives one.
    pub starts_at: Option<String>,
    /// Ticket or listing link.
    pub ticket_url: Option<String>,
    /// Distance from the matched city in km, one decimal; null when the
    /// venue had no coordinates and matched by city name.
    pub distance_km: Option<f64>,
}

/// The caller's concerts list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ConcertsResponse {
    /// False when the admin has no events source switched on with a key.
    pub configured: bool,
    /// Gigs, date ascending.
    pub items: Vec<Concert>,
    /// Item count.
    pub total: usize,
}

/// One saved city.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventCity {
    /// Display name.
    pub city_name: String,
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// Match radius in km.
    pub radius_km: f64,
    /// Country code, when known.
    pub country_code: Option<String>,
}

/// One city as the picker submits it.
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
pub struct EventCityInput {
    /// Display name; blank entries are dropped.
    pub city_name: String,
    /// Latitude, -90 to 90.
    pub latitude: f64,
    /// Longitude, -180 to 180.
    pub longitude: f64,
    /// Match radius in km, clamped to 1-500.
    #[serde(default = "default_radius_km")]
    #[schema(default = 30.0)]
    pub radius_km: f64,
    /// Country code, when known.
    #[serde(default)]
    pub country_code: Option<String>,
}

fn default_radius_km() -> f64 {
    30.0
}

/// The caller's saved cities, in picker order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventCitiesResponse {
    /// Cities in order.
    pub items: Vec<EventCity>,
}

/// Replace-all body: the picker sends its whole list in order.
#[derive(Debug, Clone, PartialEq, Deserialize, ToSchema)]
pub struct EventCitiesUpdate {
    /// Cities in order. At most 50 are kept.
    pub items: Vec<EventCityInput>,
}

/// City search query.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CitySearchQuery {
    /// Search text, 2-100 characters.
    pub q: String,
}

/// One geocoder suggestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CitySearchResult {
    /// City name.
    pub name: String,
    /// Latitude in degrees.
    pub latitude: f64,
    /// Longitude in degrees.
    pub longitude: f64,
    /// Country code.
    pub country_code: Option<String>,
    /// Country name.
    pub country: Option<String>,
    /// First-level region.
    pub region: Option<String>,
}

/// City suggestions. Empty means the geocoder knows no such place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CitySearchResponse {
    /// Suggestions, best first.
    pub items: Vec<CitySearchResult>,
}
