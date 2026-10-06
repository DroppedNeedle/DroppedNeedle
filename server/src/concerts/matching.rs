//! The pure rules behind the concerts feed, ported from v2's events watcher
//! and events service.
//!
//! Matching happens when an artist is resolved to a source's own entity,
//! never per event:
//!
//! - Ticketmaster: an attraction whose external links carry our MBID wins.
//!   Otherwise an exact-ish name match with no MBID at all; a name match
//!   carrying a different MBID is vetoed. Phrase containment is never
//!   enough ("Fontaines D.C. DJ Set" is not the band).
//! - Skiddle: exact-ish name equality only, keeping every matching id
//!   (Skiddle lists some acts more than once). "Fontaines CD" is a tribute.
//!
//! When both sources list the same gig, the Ticketmaster row wins.

use crate::providers::skiddle::{SkiddleArtist, SkiddleEvent};
use crate::providers::ticketmaster::{TmAttraction, TmEvent};

use super::models::{ConcertStatus, EventCity, EventSource};

/// Two listings within this distance on the same date are one gig.
const DEDUPE_KM: f64 = 1.0;

/// One artist the sweep walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepArtist {
    /// MBID as followed (original case).
    pub mbid: String,
    /// Lowercase MBID, the feed key.
    pub mbid_lower: String,
    /// Name used for source searches.
    pub name: String,
}

/// How a Ticketmaster attraction was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmBasis {
    /// The attraction carried our MBID.
    Mbid,
    /// Exact-ish name match with no conflicting MBID.
    ExactName,
    /// No Ticketmaster presence.
    None,
}

impl TmBasis {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mbid => "mbid",
            Self::ExactName => "exact_name",
            Self::None => "none",
        }
    }

    /// Parse the stored spelling; anything unknown reads as no presence.
    pub fn parse(value: &str) -> Self {
        match value {
            "mbid" => Self::Mbid,
            "exact_name" => Self::ExactName,
            _ => Self::None,
        }
    }
}

/// How sure the feed is that a row belongs to the artist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Matched through the MBID.
    Mbid,
    /// Matched by name.
    Name,
}

impl Confidence {
    /// The stored spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mbid => "mbid",
            Self::Name => "name",
        }
    }
}

/// One feed row as a sweep produces it.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    /// Source of the listing.
    pub source: EventSource,
    /// Listing id, unique per source.
    pub source_event_id: String,
    /// Lowercase artist MBID.
    pub artist_mbid_lower: String,
    /// Artist name.
    pub artist_name: String,
    /// Event title.
    pub event_name: String,
    /// Venue-local date, `YYYY-MM-DD`.
    pub local_date: String,
    /// Normalized status.
    pub status: ConcertStatus,
    /// Resolution basis.
    pub confidence: Confidence,
    /// Venue name.
    pub venue_name: Option<String>,
    /// Venue town or city.
    pub city: Option<String>,
    /// Venue region.
    pub region: Option<String>,
    /// Venue country code.
    pub country_code: Option<String>,
    /// Venue latitude.
    pub latitude: Option<f64>,
    /// Venue longitude.
    pub longitude: Option<f64>,
    /// ISO start time.
    pub starts_at: Option<String>,
    /// Ticket or listing link.
    pub ticket_url: Option<String>,
}

impl EventRow {
    /// The feed key within one artist.
    pub fn key(&self) -> (EventSource, String) {
        (self.source, self.source_event_id.clone())
    }
}

/// A stored feed row joined to the artist MBID the caller sees.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredConcert {
    /// The row.
    pub event: EventRow,
    /// MBID for artist links: the follow's spelling, or lowercase in
    /// library scope.
    pub artist_mbid: String,
}

/// A stored concert inside one of the user's cities.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchedConcert {
    /// The row and its MBID.
    pub concert: StoredConcert,
    /// The saved city it matched.
    pub matched_city: String,
    /// Distance in km rounded to one decimal; `None` for a city-name match.
    pub distance_km: Option<f64>,
}

/// Exact-ish name key: lowercase ASCII letters and digits only, so
/// "Fontaines D.C." equals "Fontaines DC" but not "Fontaines CD".
pub fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// Great-circle distance in km.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let radius = 6371.0;
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let d_phi = (lat2 - lat1).to_radians();
    let d_lambda = (lon2 - lon1).to_radians();
    let a = (d_phi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (d_lambda / 2.0).sin().powi(2);
    // Float error can push `a` past 1 for near-antipodal pairs.
    2.0 * radius * a.clamp(0.0, 1.0).sqrt().asin()
}

/// Resolve the Ticketmaster attraction for an artist.
pub fn pick_tm_attraction(
    attractions: &[TmAttraction],
    artist_name: &str,
    artist_mbid_lower: &str,
) -> (Option<String>, TmBasis) {
    if let Some(found) = attractions.iter().find(|attraction| {
        attraction
            .musicbrainz_ids()
            .iter()
            .any(|id| id == artist_mbid_lower)
    }) {
        return (Some(found.id.clone()), TmBasis::Mbid);
    }
    let target = normalize_name(artist_name);
    if !target.is_empty()
        && let Some(found) = attractions.iter().find(|attraction| {
            normalize_name(&attraction.name) == target && attraction.musicbrainz_ids().is_empty()
        })
    {
        return (Some(found.id.clone()), TmBasis::ExactName);
    }
    (None, TmBasis::None)
}

/// Every Skiddle id whose name matches exact-ish.
pub fn pick_skiddle_ids(artists: &[SkiddleArtist], artist_name: &str) -> Vec<String> {
    let target = normalize_name(artist_name);
    if target.is_empty() {
        return Vec::new();
    }
    artists
        .iter()
        .filter(|artist| normalize_name(&artist.name) == target)
        .map(|artist| artist.id.clone())
        .collect()
}

/// A coordinate string as a finite float.
fn parse_coord(value: Option<&str>) -> Option<f64> {
    value?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Map one Ticketmaster event. Dateless (TBA) events are dropped: a city
/// and date feed cannot place them.
pub fn map_tm_event(
    event: &TmEvent,
    artist: &SweepArtist,
    confidence: Confidence,
) -> Option<EventRow> {
    let dates = event.dates.as_ref();
    let start = dates.and_then(|dates| dates.start.as_ref());
    let local_date = start
        .and_then(|start| start.local_date.clone())
        .filter(|date| !date.is_empty())?;
    let status_code = dates
        .and_then(|dates| dates.status.as_ref())
        .and_then(|status| status.code.as_deref())
        .unwrap_or("")
        .to_lowercase();
    let status = match status_code.as_str() {
        "cancelled" | "canceled" => ConcertStatus::Cancelled,
        "rescheduled" | "postponed" => ConcertStatus::Rescheduled,
        _ => ConcertStatus::Scheduled,
    };
    let venue = event
        .embedded
        .as_ref()
        .and_then(|embedded| embedded.venues.first());
    let location = venue.and_then(|venue| venue.location.as_ref());
    Some(EventRow {
        source: EventSource::Ticketmaster,
        source_event_id: event.id.clone(),
        artist_mbid_lower: artist.mbid_lower.clone(),
        artist_name: artist.name.clone(),
        event_name: event.name.clone(),
        local_date,
        status,
        confidence,
        venue_name: venue.and_then(|venue| venue.name.clone()),
        city: venue
            .and_then(|venue| venue.city.as_ref())
            .and_then(|city| city.name.clone()),
        region: venue
            .and_then(|venue| venue.state.as_ref())
            .and_then(|state| state.name.clone()),
        country_code: venue
            .and_then(|venue| venue.country.as_ref())
            .and_then(|country| country.country_code.clone()),
        latitude: parse_coord(location.and_then(|loc| loc.latitude.as_deref())),
        longitude: parse_coord(location.and_then(|loc| loc.longitude.as_deref())),
        starts_at: start.and_then(|start| start.date_time.clone()),
        ticket_url: event.url.clone(),
    })
}

/// Map one Skiddle event. Skiddle carries no MBIDs, so every row is a
/// name match.
pub fn map_skiddle_event(event: &SkiddleEvent, artist: &SweepArtist) -> Option<EventRow> {
    let local_date = event.date.clone().filter(|date| !date.is_empty())?;
    let status = if event.is_cancelled() {
        ConcertStatus::Cancelled
    } else if event.is_rescheduled() {
        ConcertStatus::Rescheduled
    } else {
        ConcertStatus::Scheduled
    };
    let venue = event.venue.as_ref();
    let ticket_url = event
        .ticket_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .or_else(|| event.link.clone());
    Some(EventRow {
        source: EventSource::Skiddle,
        source_event_id: event.id.clone(),
        artist_mbid_lower: artist.mbid_lower.clone(),
        artist_name: artist.name.clone(),
        event_name: event.eventname.clone(),
        local_date,
        status,
        confidence: Confidence::Name,
        venue_name: venue.and_then(|venue| venue.name.clone()),
        city: venue.and_then(|venue| venue.town.clone()),
        region: venue.and_then(|venue| venue.region.clone()),
        country_code: venue.and_then(|venue| venue.country.clone()),
        latitude: venue
            .and_then(|venue| venue.latitude)
            .filter(|v| v.is_finite()),
        longitude: venue
            .and_then(|venue| venue.longitude)
            .filter(|v| v.is_finite()),
        starts_at: event.startdate.clone(),
        ticket_url,
    })
}

/// Same local date and either the same folded venue name or venues within
/// [`DEDUPE_KM`].
fn same_gig(a: &EventRow, b: &EventRow) -> bool {
    if a.local_date != b.local_date {
        return false;
    }
    let name_a = normalize_name(a.venue_name.as_deref().unwrap_or(""));
    if !name_a.is_empty() && name_a == normalize_name(b.venue_name.as_deref().unwrap_or("")) {
        return true;
    }
    match (a.latitude, a.longitude, b.latitude, b.longitude) {
        (Some(lat_a), Some(lon_a), Some(lat_b), Some(lon_b)) => {
            haversine_km(lat_a, lon_a, lat_b, lon_b) <= DEDUPE_KM
        }
        _ => false,
    }
}

/// Drop Skiddle rows that duplicate a Ticketmaster row: Ticketmaster's
/// matching is MBID-grade and its rows are richer.
pub fn dedupe_across_sources(events: Vec<EventRow>) -> Vec<EventRow> {
    let (tm_rows, others): (Vec<EventRow>, Vec<EventRow>) = events
        .into_iter()
        .partition(|row| row.source == EventSource::Ticketmaster);
    let kept_others: Vec<EventRow> = others
        .into_iter()
        .filter(|row| !tm_rows.iter().any(|tm| same_gig(row, tm)))
        .collect();
    tm_rows.into_iter().chain(kept_others).collect()
}

/// Distance when the event falls in the city's radius, `Some(None)` for a
/// coordinate-less city-name match, `None` for no match.
fn match_city(event: &EventRow, city: &EventCity) -> Option<Option<f64>> {
    if let (Some(lat), Some(lon)) = (event.latitude, event.longitude) {
        let distance = haversine_km(city.latitude, city.longitude, lat, lon);
        return (distance <= city.radius_km).then_some(Some(distance));
    }
    let event_city = event.city.as_deref()?.trim().to_lowercase();
    (event_city == city.city_name.trim().to_lowercase()).then_some(None)
}

/// Keep the concerts inside any saved city, each tagged with its nearest
/// city. A name match counts as nearer than any distance, as in v2.
pub fn filter_to_cities(concerts: Vec<StoredConcert>, cities: &[EventCity]) -> Vec<MatchedConcert> {
    concerts
        .into_iter()
        .filter_map(|concert| {
            let mut best: Option<(Option<f64>, &EventCity)> = None;
            for city in cities {
                let Some(distance) = match_city(&concert.event, city) else {
                    continue;
                };
                let closer = match (best, distance) {
                    (None, _) => true,
                    (Some((Some(current), _)), Some(candidate)) => candidate < current,
                    (Some((Some(_), _)), None) => true,
                    (Some((None, _)), _) => false,
                };
                if closer {
                    best = Some((distance, city));
                }
            }
            let (distance, city) = best?;
            Some(MatchedConcert {
                concert,
                matched_city: city.city_name.clone(),
                distance_km: distance.map(|km| (km * 10.0).round() / 10.0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::providers::ticketmaster::TmExternalId;

    fn attraction(id: &str, name: &str, mbids: &[&str]) -> TmAttraction {
        let links = (!mbids.is_empty()).then(|| {
            HashMap::from([(
                "musicbrainz".to_owned(),
                mbids
                    .iter()
                    .map(|mbid| TmExternalId {
                        id: Some((*mbid).to_owned()),
                    })
                    .collect(),
            )])
        });
        TmAttraction {
            id: id.to_owned(),
            name: name.to_owned(),
            external_links: links,
        }
    }

    // The resolution rules decide whose gigs a user sees; a regression here
    // silently shows tribute acts or hides the real band.
    #[test]
    fn tm_resolution_prefers_mbid_and_vetoes_conflicting_names() {
        let ours = "aaaaaaaa-0000-0000-0000-000000000001";
        let found = pick_tm_attraction(
            &[
                attraction("dj", "Fontaines D.C. DJ Set", &[]),
                attraction("real", "Fontaines DC", &[ours]),
            ],
            "Fontaines D.C.",
            ours,
        );
        assert_eq!(found, (Some("real".to_owned()), TmBasis::Mbid));

        let vetoed = pick_tm_attraction(
            &[attraction("other", "Fontaines D.C.", &["bbbb"])],
            "Fontaines D.C.",
            ours,
        );
        assert_eq!(vetoed, (None, TmBasis::None));

        let by_name = pick_tm_attraction(
            &[attraction("named", "fontaines dc", &[])],
            "Fontaines D.C.",
            ours,
        );
        assert_eq!(by_name, (Some("named".to_owned()), TmBasis::ExactName));
    }
}
