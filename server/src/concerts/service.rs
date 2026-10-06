//! The read side: a user's concerts, cities, unseen count and city search.
//!
//! Concerts are feed rows for the user's follows (or the whole feed in
//! library sweep scope, where the shared library's gigs belong to
//! everyone), narrowed to the user's saved cities. One user's candidates
//! number in the hundreds at most, so the city filter runs here rather
//! than in SQL. A user with no cities sees nothing; the page walks them
//! into adding one.

use std::sync::Arc;

use super::matching::{MatchedConcert, filter_to_cities};
use super::models::{
    CitySearchResponse, CitySearchResult, Concert, ConcertsResponse, EventCitiesResponse,
    EventCity, EventCityInput,
};
use super::sources::Sources;
use super::store::{ConcertsStore, StoreError};
use super::{ActiveSources, now_unix, today};
use crate::providers::error::ProviderError;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::EventsSweepScope;

/// Cities kept per user, from v2.
pub const MAX_CITIES: usize = 50;
/// Radius clamp in km, from v2.
pub const MIN_RADIUS_KM: f64 = 1.0;
/// Radius clamp in km, from v2.
pub const MAX_RADIUS_KM: f64 = 500.0;
/// City search length bounds in characters, from v2.
const SEARCH_CHARS: std::ops::RangeInclusive<usize> = 2..=100;

/// Why a concerts call failed.
#[derive(Debug, thiserror::Error)]
pub enum ConcertsError {
    /// The request itself is wrong. The message is user-facing.
    #[error("{0}")]
    InvalidInput(String),
    /// The geocoder failed; an empty list would read as "no such city".
    #[error("city search unavailable: {0}")]
    SearchUnavailable(ProviderError),
    /// The database stayed locked past its busy timeout.
    #[error("database busy during {0}")]
    Busy(String),
    /// Anything else. The text goes to the log only.
    #[error("{0}")]
    Internal(String),
}

impl From<StoreError> for ConcertsError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(operation) => Self::Busy(operation),
            StoreError::Internal(cause) => Self::Internal(cause),
        }
    }
}

/// The concerts read service.
#[derive(Clone)]
pub struct ConcertsService {
    store: ConcertsStore,
    config: Arc<ConfigStore>,
    sources: Arc<Sources>,
}

impl ConcertsService {
    /// Service over the shared store, settings and sources.
    pub fn new(store: ConcertsStore, config: Arc<ConfigStore>, sources: Arc<Sources>) -> Self {
        Self {
            store,
            config,
            sources,
        }
    }

    /// The events settings for this call; `None` when no source is ready.
    fn active(&self) -> Option<ActiveSources> {
        ActiveSources::read(&self.config)
    }

    /// Candidate rows for the user, optionally only those first seen after
    /// `discovered_after`.
    async fn candidates(
        &self,
        user_id: &str,
        discovered_after: Option<f64>,
    ) -> Result<Vec<super::matching::StoredConcert>, ConcertsError> {
        let min_date = today().to_string();
        let library_scope = ActiveSources::scope(&self.config) == EventsSweepScope::Library;
        let user = (!library_scope).then_some(user_id);
        Ok(self
            .store
            .concerts(user, &min_date, discovered_after)
            .await?)
    }

    /// Upcoming gigs in the user's cities, date ascending.
    pub async fn list(&self, user_id: &str) -> Result<ConcertsResponse, ConcertsError> {
        let configured = self.active().is_some();
        let cities = self.store.cities(user_id).await?;
        let items: Vec<Concert> = if cities.is_empty() {
            Vec::new()
        } else {
            filter_to_cities(self.candidates(user_id, None).await?, &cities)
                .into_iter()
                .map(concert_view)
                .collect()
        };
        Ok(ConcertsResponse {
            configured,
            total: items.len(),
            items,
        })
    }

    /// Gigs in the user's cities first seen since they last looked.
    pub async fn unseen_count(&self, user_id: &str) -> Result<usize, ConcertsError> {
        let cities = self.store.cities(user_id).await?;
        if cities.is_empty() {
            return Ok(0);
        }
        let seen_at = self.store.seen_at(user_id).await?;
        let fresh = self.candidates(user_id, Some(seen_at)).await?;
        Ok(filter_to_cities(fresh, &cities).len())
    }

    /// Stamp the user's seen marker.
    pub async fn mark_seen(&self, user_id: &str) -> Result<(), ConcertsError> {
        Ok(self.store.mark_seen(user_id, now_unix()).await?)
    }

    /// The user's cities in picker order.
    pub async fn cities(&self, user_id: &str) -> Result<EventCitiesResponse, ConcertsError> {
        Ok(EventCitiesResponse {
            items: self.store.cities(user_id).await?,
        })
    }

    /// Replace the user's cities with the picker's list. Blank names are
    /// dropped, the list is capped at [`MAX_CITIES`] and radii clamp to
    /// 1-500 km; coordinates out of range reject the whole save.
    pub async fn replace_cities(
        &self,
        user_id: &str,
        items: Vec<EventCityInput>,
    ) -> Result<EventCitiesResponse, ConcertsError> {
        for item in &items {
            if !(-90.0..=90.0).contains(&item.latitude) {
                return Err(ConcertsError::InvalidInput(
                    "latitude must be between -90 and 90".to_owned(),
                ));
            }
            if !(-180.0..=180.0).contains(&item.longitude) {
                return Err(ConcertsError::InvalidInput(
                    "longitude must be between -180 and 180".to_owned(),
                ));
            }
            if !item.radius_km.is_finite() {
                return Err(ConcertsError::InvalidInput(
                    "radius_km must be a number".to_owned(),
                ));
            }
        }
        let cities: Vec<EventCity> = items
            .into_iter()
            .filter_map(|item| {
                let city_name = item.city_name.trim().to_owned();
                (!city_name.is_empty()).then(|| EventCity {
                    city_name,
                    latitude: item.latitude,
                    longitude: item.longitude,
                    radius_km: item.radius_km.clamp(MIN_RADIUS_KM, MAX_RADIUS_KM),
                    country_code: item.country_code,
                })
            })
            .take(MAX_CITIES)
            .collect();
        self.store.replace_cities(user_id, cities).await?;
        self.cities(user_id).await
    }

    /// City suggestions from the geocoder.
    pub async fn search_cities(&self, query: &str) -> Result<CitySearchResponse, ConcertsError> {
        if !SEARCH_CHARS.contains(&query.chars().count()) {
            return Err(ConcertsError::InvalidInput(
                "q must be 2 to 100 characters".to_owned(),
            ));
        }
        let found = self
            .sources
            .cities(query)
            .await
            .map_err(ConcertsError::SearchUnavailable)?;
        Ok(CitySearchResponse {
            items: found
                .into_iter()
                .filter(|city| !city.name.is_empty())
                .map(|city| CitySearchResult {
                    name: city.name,
                    latitude: city.latitude,
                    longitude: city.longitude,
                    country_code: city.country_code,
                    country: city.country,
                    region: city.admin1,
                })
                .collect(),
        })
    }
}

fn concert_view(matched: MatchedConcert) -> Concert {
    let MatchedConcert {
        concert,
        matched_city,
        distance_km,
    } = matched;
    let event = concert.event;
    Concert {
        artist_mbid: concert.artist_mbid,
        artist_name: event.artist_name,
        event_name: event.event_name,
        local_date: event.local_date,
        status: event.status,
        source: event.source,
        source_event_id: event.source_event_id,
        matched_city,
        venue_name: event.venue_name,
        city: event.city,
        region: event.region,
        country_code: event.country_code,
        starts_at: event.starts_at,
        ticket_url: event.ticket_url,
        distance_km,
    }
}
