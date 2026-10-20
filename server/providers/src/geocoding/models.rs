//! Open-Meteo geocoding wire shapes.
//!
//! Tolerant of unknown fields. A place's coordinates are its identity and are
//! required: a place without them fails decoding. An answer with no `results`
//! key is how the geocoder says it knows no such place, and reads as an empty
//! list. A refused request (HTTP 400, with `error` and `reason`) is answered
//! as a typed failure before this decode runs.

use serde::Deserialize;

/// The search answer. `results` is missing, not empty, for an unknown place.
///
/// Verified against geocoding-api.open-meteo.com/v1/search on 2026-10-20: tests/fixtures/geocoding/search_liverpool.json.
#[derive(Debug, Deserialize)]
pub struct SearchAnswer {
    /// The matching places, best first.
    #[serde(default)]
    pub results: Vec<GeoCity>,
}

/// One matching place. Its coordinates are its identity: a place without
/// them fails decoding. The name and the region are display text, empty or
/// absent when the wire omits them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GeoCity {
    /// City name, empty when the wire omits it.
    #[serde(default)]
    pub name: String,
    /// Latitude in degrees. Required.
    pub latitude: f64,
    /// Longitude in degrees. Required.
    pub longitude: f64,
    /// ISO country code such as "GB".
    #[serde(default)]
    pub country_code: Option<String>,
    /// Full country name.
    #[serde(default)]
    pub country: Option<String>,
    /// First-level region such as "England".
    #[serde(default)]
    pub admin1: Option<String>,
}
