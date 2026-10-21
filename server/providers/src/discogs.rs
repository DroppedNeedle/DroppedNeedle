//! Discogs contribution metadata: release lookup and release search.
//!
//! A Rust port of v2's Discogs repository (repository, models, and the API
//! notes seen on 2026-07-21, per discogs_API_NOTES.md). Only contribution
//! metadata crosses the boundary: image, community, and marketplace-adjacent
//! fields decode and are dropped, exactly as in v2. Each quirk below cites
//! the v2 code or note it came from.
//!
//! Requests go through the executor at the `discogs` row: 25 requests a
//! minute unauthenticated, spaced 2.4 seconds with no burst. Response
//! caching (6h) and dedup stay with wiring. `Ok(None)` and `Ok(vec![])`
//! are absence; every failure is a typed [`ProviderError`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::RequestPriority;
use crate::client::{decode, parse_base, payload, unusable_base};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};
use crate::limiter::RateLimiter;

mod models;

use models::{WireArtist, WireRelease, WireSearchResponse, WireSearchResult, WireTrack};

/// Live API origin, seen on 2026-07-21 per discogs_API_NOTES.md.
pub const API_BASE: &str = "https://api.discogs.com";
/// Origin for canonical release/master/artist/label links.
pub const WEB_BASE: &str = "https://www.discogs.com";
/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "discogs";
/// Largest answer read: a box set's tracklist can be long.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Discogs on the wire: the standard rules, and a 429 that names no wait
/// rests a minute.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

// ---------------------------------------------------------------------------
// Normalized models
// ---------------------------------------------------------------------------

/// One normalized artist credit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistCredit {
    /// Display name (`name`, falling back to `anv`).
    pub name: String,
    /// Name variation as printed, when present.
    pub credited_name: Option<String>,
    /// Join phrase to the next credit.
    pub join_phrase: String,
    /// Discogs artist id.
    pub artist_id: Option<String>,
    /// Canonical artist page.
    pub canonical_url: Option<String>,
}

/// One normalized format row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Format {
    /// Format name.
    pub name: String,
    /// Quantity, when the wire value parsed as an integer.
    pub quantity: Option<i64>,
    /// Format descriptions.
    pub descriptions: Vec<String>,
    /// Free-text format note.
    pub text: Option<String>,
}

/// One normalized identifier row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identifier {
    /// Identifier kind.
    pub kind: String,
    /// Identifier value.
    pub value: String,
    /// Identifier description.
    pub description: Option<String>,
}

/// One normalized label row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Label {
    /// Label name.
    pub name: String,
    /// Catalogue number.
    pub catalogue_number: Option<String>,
    /// Discogs label id.
    pub label_id: Option<String>,
    /// Canonical label page.
    pub canonical_url: Option<String>,
}

/// One normalized track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Printed position as received.
    pub source_position: Option<String>,
    /// Track number within its medium.
    pub number: Option<u32>,
    /// Track title.
    pub title: String,
    /// Duration in seconds.
    pub duration_seconds: Option<f64>,
    /// True for heading/index rows rather than playable tracks.
    pub heading: bool,
    /// Per-track artist credits.
    pub artists: Vec<ArtistCredit>,
}

/// One normalized medium (disc/side group).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Medium {
    /// 1-based medium position.
    pub position: u32,
    /// Format name shared from the release level.
    pub format: Option<String>,
    /// Tracks on this medium, in wire order.
    pub tracks: Vec<Track>,
}

/// One normalized release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
    /// Discogs release id.
    pub release_id: String,
    /// Discogs master id.
    pub master_id: Option<String>,
    /// Canonical release page.
    pub canonical_release_url: String,
    /// Canonical master page.
    pub canonical_master_url: Option<String>,
    /// Release title.
    pub title: String,
    /// Joined artist string.
    pub artist_name: String,
    /// Release-level artist credits.
    pub artists: Vec<ArtistCredit>,
    /// Release date string.
    pub released_date: Option<String>,
    /// Release year.
    pub year: Option<i64>,
    /// Country string.
    pub country: Option<String>,
    /// Label rows.
    pub labels: Vec<Label>,
    /// Identifier rows.
    pub identifiers: Vec<Identifier>,
    /// First barcode value with spaces and dashes stripped.
    pub barcode: Option<String>,
    /// Format rows.
    pub formats: Vec<Format>,
    /// Media with grouped tracks.
    pub media: Vec<Medium>,
    /// Unix time the source payload was fetched.
    pub source_fetched_at: f64,
}

/// One normalized search candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    /// Discogs release id.
    pub release_id: String,
    /// Discogs master id.
    pub master_id: Option<String>,
    /// Release title (artist prefix split off when present).
    pub title: String,
    /// Artist prefix of the display title, or "" without a separator.
    pub artist_name: String,
    /// Canonical release page.
    pub canonical_url: String,
    /// Release year.
    pub year: Option<i64>,
    /// Country string.
    pub country: Option<String>,
    /// First label name.
    pub label: Option<String>,
    /// Catalogue number.
    pub catalogue_number: Option<String>,
    /// Joined format summary.
    pub format_summary: Option<String>,
    /// Unix time the source payload was fetched.
    pub fetched_at: f64,
}

/// The payload decoded but carries no usable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompleteRelease;

// ---------------------------------------------------------------------------
// Pure normalization (ports of the v2 module-level helpers)
// ---------------------------------------------------------------------------

/// Parse a "M:SS" or "H:MM:SS" duration. Seconds,
/// and minutes in the long form, must be below 60; anything else is not a
/// duration and yields `None` rather than a guess.
pub fn duration_seconds(value: &str) -> Option<f64> {
    if value.is_empty() {
        return None;
    }
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return None;
    }
    if parts
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let numbers: Vec<u64> = parts
        .iter()
        .map(|part| part.parse::<u64>().unwrap_or(u64::MAX))
        .collect();
    if numbers[numbers.len() - 1] >= 60 {
        return None;
    }
    if numbers.len() == 3 {
        if numbers[1] >= 60 {
            return None;
        }
        return Some((numbers[0] * 3600 + numbers[1] * 60 + numbers[2]) as f64);
    }
    Some((numbers[0] * 60 + numbers[1]) as f64)
}

/// True for heading/index rows (`type_` casefolded
/// against `"track"`).
pub fn is_heading(type_: &str) -> bool {
    !type_.eq_ignore_ascii_case("track")
}

/// Split a printed position into (medium, number):
/// "2-3"/"2.3" name disc 2 track 3, a bare number is a track on medium 1,
/// and a vinyl side letter maps pairs of sides onto one medium (A/B to
/// medium 1, C/D to medium 2, ...). Anything else keeps medium 1 with no
/// number.
pub fn parse_position(value: &str, fallback: u32) -> (u32, Option<u32>) {
    let text: String = value
        .trim()
        .chars()
        .flat_map(|c| c.to_uppercase())
        .collect();
    if let Some((disc, track)) = text.split_once('-').or_else(|| text.split_once('.')) {
        let cells = [disc, track];
        if cells
            .iter()
            .all(|cell| !cell.is_empty() && cell.bytes().all(|b| b.is_ascii_digit()))
            && let (Ok(disc), Ok(track)) = (disc.parse::<u32>(), track.parse::<u32>())
        {
            return (disc.max(1), Some(track));
        }
    }
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return (1, text.parse::<u32>().ok());
    }
    let mut chars = text.chars();
    if let Some(side) = chars.next()
        && side.is_ascii_alphabetic()
    {
        let rest: String = chars.collect();
        if rest.bytes().all(|b| b.is_ascii_digit()) {
            let medium = u32::from(side as u8 - b'A') / 2 + 1;
            let number = if rest.is_empty() {
                Some(fallback)
            } else {
                rest.parse::<u32>().ok()
            };
            return (medium.max(1), number);
        }
    }
    (1, None)
}

/// Flatten index sub-tracks depth-first, keeping parents before children.
pub fn flatten_tracks(rows: &[WireTrack]) -> Vec<&WireTrack> {
    let mut flat = Vec::new();
    for row in rows {
        flat.push(row);
        flat.extend(flatten_tracks(&row.sub_tracks));
    }
    flat
}

/// Normalize one artist credit.
pub fn artist_credit(artist: &WireArtist) -> ArtistCredit {
    let artist_id = artist.id.map(|id| id.to_string());
    let name = if artist.name.is_empty() {
        artist.anv.clone()
    } else {
        artist.name.clone()
    };
    ArtistCredit {
        name,
        credited_name: if artist.anv.is_empty() {
            None
        } else {
            Some(artist.anv.clone())
        },
        join_phrase: artist.join.clone(),
        artist_id: artist_id.clone(),
        canonical_url: artist_id.map(|id| format!("{WEB_BASE}/artist/{id}")),
    }
}

/// Group flattened rows into media. The fallback
/// number always counts medium 1 (`counters[1] + 1`), even for rows that
/// land on later media; only real tracks claim the per-medium counter.
pub fn normalize_media(tracks: &[WireTrack], formats: &[Format]) -> Vec<Medium> {
    let mut grouped: BTreeMap<u32, Vec<Track>> = BTreeMap::new();
    let mut counters: BTreeMap<u32, u32> = BTreeMap::new();
    for row in flatten_tracks(tracks) {
        let fallback = counters.get(&1).copied().unwrap_or(0) + 1;
        let (medium, mut number) = parse_position(&row.position, fallback);
        counters
            .entry(medium)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        if number.is_none() && !is_heading(&row.type_) {
            number = counters.get(&medium).copied();
        }
        grouped.entry(medium).or_default().push(Track {
            source_position: if row.position.is_empty() {
                None
            } else {
                Some(row.position.clone())
            },
            number,
            title: row.title.clone(),
            duration_seconds: duration_seconds(&row.duration),
            heading: is_heading(&row.type_),
            artists: row.artists.iter().map(artist_credit).collect(),
        });
    }
    let format_name = formats.first().map(|format| format.name.clone());
    grouped
        .into_iter()
        .map(|(position, tracks)| Medium {
            position,
            format: format_name.clone(),
            tracks,
        })
        .collect()
}

/// Normalize format rows; a non-numeric `qty` becomes `None` rather than
/// failing the release.
pub fn normalize_formats(release: &WireRelease) -> Vec<Format> {
    release
        .formats
        .iter()
        .map(|item| Format {
            name: item.name.clone(),
            quantity: if item.qty.is_empty() {
                None
            } else {
                item.qty.parse::<i64>().ok()
            },
            descriptions: item.descriptions.clone(),
            text: if item.text.is_empty() {
                None
            } else {
                Some(item.text.clone())
            },
        })
        .collect()
}

/// Normalize one release. A zero id or an empty title is incomplete, never
/// an empty release. A missing id fails decoding before this point.
pub fn normalize_release(
    release: &WireRelease,
    fetched_at: f64,
) -> Result<Release, IncompleteRelease> {
    if release.id <= 0 || release.title.is_empty() {
        return Err(IncompleteRelease);
    }
    let formats = normalize_formats(release);
    let identifiers: Vec<Identifier> = release
        .identifiers
        .iter()
        .filter(|item| !item.r#type.is_empty() && !item.value.is_empty())
        .map(|item| Identifier {
            kind: item.r#type.clone(),
            value: item.value.clone(),
            description: if item.description.is_empty() {
                None
            } else {
                Some(item.description.clone())
            },
        })
        .collect();
    // Quirk: the barcode is the first barcode identifier with spaces and
    // dashes stripped.
    let barcode = identifiers
        .iter()
        .find(|item| item.kind.eq_ignore_ascii_case("barcode"))
        .map(|item| item.value.replace([' ', '-'], ""));
    let artist_name = if release.artists_sort.is_empty() {
        release
            .artists
            .iter()
            .filter(|artist| !artist.name.is_empty())
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        release.artists_sort.clone()
    };
    let media = normalize_media(&release.tracklist, &formats);
    Ok(Release {
        release_id: release.id.to_string(),
        master_id: release.master_id.map(|id| id.to_string()),
        canonical_release_url: format!("{WEB_BASE}/release/{}", release.id),
        canonical_master_url: release
            .master_id
            .map(|id| format!("{WEB_BASE}/master/{id}")),
        title: release.title.clone(),
        artist_name,
        artists: release.artists.iter().map(artist_credit).collect(),
        released_date: if release.released.is_empty() {
            None
        } else {
            Some(release.released.clone())
        },
        year: release.year,
        country: if release.country.is_empty() {
            None
        } else {
            Some(release.country.clone())
        },
        labels: release
            .labels
            .iter()
            .filter(|label| !label.name.is_empty())
            .map(|label| Label {
                name: label.name.clone(),
                catalogue_number: if label.catno.is_empty() {
                    None
                } else {
                    Some(label.catno.clone())
                },
                label_id: label.id.map(|id| id.to_string()),
                canonical_url: label.id.map(|id| format!("{WEB_BASE}/label/{id}")),
            })
            .collect(),
        identifiers,
        barcode,
        formats,
        media,
        source_fetched_at: fetched_at,
    })
}

/// Normalize the search query the way v2 does: collapse whitespace and cap
/// at 200 characters.
pub fn normalize_query(query: &str) -> String {
    let collapsed = query.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(200).collect()
}

/// Normalize one search hit. A zero id or an empty title skips the hit.
/// The display title splits on the first " - " into
/// artist and title; without a separator the whole string is the title and
/// the artist stays empty. The format summary prefers the flat `format`
/// list and falls back to the structured rows.
pub fn normalize_candidate(item: &WireSearchResult, fetched_at: f64) -> Option<ReleaseCandidate> {
    if item.id <= 0 || item.title.is_empty() {
        return None;
    }
    let (artist_name, title) = match item.title.split_once(" - ") {
        Some((artist, title)) => (artist.to_owned(), title.to_owned()),
        None => (String::new(), item.title.clone()),
    };
    let mut formats = item.format.clone();
    if formats.is_empty() {
        formats = item
            .formats
            .iter()
            .filter(|format| !format.name.is_empty())
            .map(|format| format.name.clone())
            .collect();
    }
    Some(ReleaseCandidate {
        release_id: item.id.to_string(),
        master_id: item.master_id.map(|id| id.to_string()),
        title,
        artist_name,
        canonical_url: format!("{WEB_BASE}/release/{}", item.id),
        // The search answer sends the year as text; text that is not a number reads as no year.
        year: item.year.as_deref().and_then(|year| year.parse().ok()),
        country: if item.country.is_empty() {
            None
        } else {
            Some(item.country.clone())
        },
        label: item.label.first().cloned(),
        catalogue_number: if item.catno.is_empty() {
            None
        } else {
            Some(item.catno.clone())
        },
        format_summary: if formats.is_empty() {
            None
        } else {
            Some(formats.join(", "))
        },
        fetched_at,
    })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Discogs catalog client. The clock stays outside: callers pass
/// `fetched_at` so wiring owns time, and caching/dedup wrap these calls.
/// Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct DiscogsClient {
    executor: Executor,
    base: Option<Url>,
}

impl DiscogsClient {
    /// The client sending through `executor` to the API origin at
    /// `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        Self {
            executor,
            base: parse_base(base_url, SOURCE),
        }
    }

    /// The limiter the client paces through, for a caller that will not
    /// wait for a turn.
    #[must_use]
    pub fn limiter(&self) -> &RateLimiter {
        self.executor.limiter()
    }

    /// One GET of `segments` under the origin, or `None` on a 404.
    async fn get(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, segments, query).map_err(|_| unusable_base(SOURCE))?;
        match self.executor.send(request, priority).await {
            Ok(reply) => Ok(Some(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Fetch one release. A 404 is absence (`Ok(None)`); a release without
    /// an identity is a payload failure.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn get_release(
        &self,
        release_id: &str,
        fetched_at: f64,
        priority: RequestPriority,
    ) -> Result<Option<Release>, ProviderError> {
        let Some(reply) = self.get(&["releases", release_id], &[], priority).await? else {
            return Ok(None);
        };
        let wire: WireRelease = decode(&reply)?;
        normalize_release(&wire, fetched_at)
            .map(Some)
            .map_err(|IncompleteRelease| {
                payload(
                    SOURCE,
                    PayloadKind::Contract,
                    "release without an id or a title",
                )
            })
    }

    /// Search releases. The limit clamps to 1..=10 and 404 is an empty page.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_releases(
        &self,
        query: &str,
        limit: u32,
        fetched_at: f64,
        priority: RequestPriority,
    ) -> Result<Vec<ReleaseCandidate>, ProviderError> {
        let bounded = limit.clamp(1, 10);
        let normalized = normalize_query(query);
        let per_page = bounded.to_string();
        let Some(reply) = self
            .get(
                &["database", "search"],
                &[
                    ("type", "release"),
                    ("q", normalized.as_str()),
                    ("per_page", per_page.as_str()),
                ],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let wire: WireSearchResponse = decode(&reply)?;
        Ok(wire
            .results
            .iter()
            .take(bounded as usize)
            .filter_map(|item| normalize_candidate(item, fetched_at))
            .collect())
    }
}
