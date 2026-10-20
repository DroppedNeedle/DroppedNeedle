//! Open-Meteo geocoding client (the events city picker).
//!
//! Port of v2's geocoding repository. A city search is a person waiting,
//! not optional enrichment: failures are typed [`ProviderError`]s (the
//! route answers 503) so the UI can say "geocoding unavailable". A silent
//! `[]` would read as "no such city", so `[]` only ever means the geocoder
//! knows no such place. Every request goes through the executor at the
//! `open-meteo` rate row.
//!
//! Live-verified 2026-07-06: `geocoding-api.open-meteo.com/v1/search?name=…`
//! needs no API key; `?name=Liverpool` returns Liverpool GB first with float
//! coordinates, `country_code`, and `admin1` region. Open-Meteo omits the
//! `results` key entirely for unknown places.

use std::time::Duration;

use serde::Deserialize;

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{Backoff, Executor, Request, Spec, Url, standard_classify};

/// Provider key used in [ProviderError] and logs.
pub const SOURCE: &str = "geocoding";
/// The rate row the geocoder paces through.
pub const RATE_ROW: &str = "open-meteo";

/// Production endpoint.
pub const GEOCODING_API_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
/// Default page of city suggestions, kept from v2.
pub const DEFAULT_CITY_COUNT: u32 = 8;
/// Largest geocoding answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Open-Meteo on the wire: the standard rules.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(10),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

/// One matching city. Every field past the name tolerates absence, mirroring
/// the v2 struct's all-defaulted shape.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GeoCity {
    /// City name, empty when the wire omits it.
    #[serde(default)]
    pub name: String,
    /// Latitude in degrees, 0.0 when absent.
    #[serde(default)]
    pub latitude: f64,
    /// Longitude in degrees, 0.0 when absent.
    #[serde(default)]
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

/// Top-level geocoding payload. `results` is missing (not empty) for unknown
/// places, hence the default.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct GeocodingResponse {
    #[serde(default)]
    results: Vec<GeoCity>,
}

/// The city picker client, built once with the providers.
#[derive(Debug, Clone)]
pub struct GeocodingClient {
    executor: Executor,
    base: Option<Url>,
}

impl GeocodingClient {
    /// The client sending through `executor` to the search URL `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        Self {
            executor,
            base: parse_base(base_url, SOURCE),
        }
    }

    /// Cities matching `query`, up to `count` of them. `[]` means the
    /// geocoder knows no such place.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_cities(
        &self,
        query: &str,
        count: u32,
        priority: RequestPriority,
    ) -> Result<Vec<GeoCity>, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let count = count.to_string();
        let request = Request::get(
            base,
            &[],
            &[
                ("name", query),
                ("count", &count),
                ("language", "en"),
                ("format", "json"),
            ],
        )
        .map_err(|_| unusable_base(SOURCE))?;
        let reply = self.executor.send(request, priority).await?;
        let decoded: GeocodingResponse = decode(&reply)?;
        Ok(decoded.results)
    }
}
