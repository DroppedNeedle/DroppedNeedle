//! Open-Meteo geocoding client (the events city picker).
//!
//! Port of v2's geocoding repository. A city search is a
//! user-initiated action, not optional enrichment: failures raise a typed
//! error (wiring maps it to 503) so the UI can say "geocoding unavailable".
//! A silent `[]` would read as "no such city", so `[]` only ever means the
//! geocoder knows no such place.
//!
//! Live-verified 2026-07-06: `geocoding-api.open-meteo.com/v1/search?name=…`
//! needs no API key; `?name=Liverpool` returns Liverpool GB first with float
//! coordinates, `country_code`, and `admin1` region. Open-Meteo omits the
//! `results` key entirely for unknown places.
//!
//! Shared infrastructure note: this client sends one request per call. Retry (3
//! attempts over transport and rate-limit failures only), the 2 req/s pacing,
//! and the circuit breaker live in the shared resilience layer, which matches
//! on [GeocodingError].

use serde::Deserialize;
use thiserror::Error;

/// Production endpoint. Tests point elsewhere via [GeocodingClient::with_base_url].
pub const GEOCODING_API_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
/// Default page of city suggestions, kept from v2.
pub const DEFAULT_CITY_COUNT: u32 = 8;

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

/// What can go wrong on a city search. Variants stay distinct so the shared
/// retry layer can match on them: only [GeocodingError::Transport] and
/// [GeocodingError::RateLimited] are retriable.
#[derive(Debug, Error)]
pub enum GeocodingError {
    /// The request never completed (DNS, TLS, connect, timeout).
    #[error("geocoding request failed: {0}")]
    Transport(String),
    /// The provider answered 429.
    #[error("geocoding rate limit exceeded")]
    RateLimited,
    /// The provider answered a non-200 status (v2 matches strict 200).
    #[error("geocoding returned HTTP {status}")]
    Api {
        /// The status the provider sent back.
        status: u16,
    },
    /// The body was not the documented shape.
    #[error("geocoding response decode failed: {0}")]
    Decode(String),
}

/// The city picker client. Clone the shared `reqwest::Client` from the HTTP
/// factory at wiring time; this struct only adds the endpoint and params.
#[derive(Debug, Clone)]
pub struct GeocodingClient {
    http: reqwest::Client,
    base_url: String,
}

impl GeocodingClient {
    /// Client against the production endpoint.
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_base_url(http, GEOCODING_API_URL)
    }

    /// Client against an override endpoint (scripted fakes in tests).
    pub fn with_base_url(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
    }

    /// Cities matching `query`, up to `count` of them. `[]` means the
    /// geocoder knows no such place; a provider failure raises instead.
    pub async fn search_cities(
        &self,
        query: &str,
        count: u32,
    ) -> Result<Vec<GeoCity>, GeocodingError> {
        let params = [
            ("name", query.to_owned()),
            ("count", count.to_string()),
            ("language", "en".to_owned()),
            ("format", "json".to_owned()),
        ];
        let response = self
            .http
            .get(self.base_url.as_str())
            .query(&params)
            .send()
            .await
            .map_err(|error| GeocodingError::Transport(transport_kind(&error).to_owned()))?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(GeocodingError::RateLimited);
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(GeocodingError::Api {
                status: response.status().as_u16(),
            });
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| GeocodingError::Transport(transport_kind(&error).to_owned()))?;
        let decoded: GeocodingResponse = serde_json::from_slice(&body)
            .map_err(|error| GeocodingError::Decode(error.to_string()))?;
        Ok(decoded.results)
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
