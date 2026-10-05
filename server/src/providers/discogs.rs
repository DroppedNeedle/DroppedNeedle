//! Discogs contribution metadata: release lookup and release search.
//!
//! A Rust port of v2's `backend/repositories/discogs/` (repository, models,
//! and the notes verified live on 2026-07-21). Only contribution metadata
//! crosses the boundary: image, community, and marketplace-adjacent fields
//! decode and are dropped, exactly as in v2. Each quirk below cites the v2
//! code or note it came from.
//!
//! Transport rides the shared [`HttpPort`](super::client::HttpPort) GET port.
//! Retry, rate limiting (25 requests/minute unauthenticated,
//! no burst), response caching (6h), and request dedup stay with wiring;
//! degradation recording stays with enrichment, which treats `Ok(None)` and
//! `Ok(vec![])` as absence and `Err` as a degraded source.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::client::HttpPort;

/// Provider key used in logs and degradation records.
pub const PROVIDER_NAME: &str = "discogs";
/// Live API origin (discogs_API_NOTES.md, verified 2026-07-21).
pub const API_BASE: &str = "https://api.discogs.com";
/// Origin for canonical release/master/artist/label links.
pub const WEB_BASE: &str = "https://www.discogs.com";
/// Unauthenticated ceiling advertised by live responses (`x-discogs-ratelimit:
/// 25`). Enforced by wiring, recorded here so the port stays honest.
pub const RATE_LIMIT_PER_MINUTE: u32 = 25;

/// What can go wrong on a Discogs fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The wire failed or the upstream answered 5xx (retryable, degraded).
    Transport,
    /// The upstream answered 429; the caller backs off this long.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: f64,
    },
    /// Any other status, an undecodable body, or an incomplete identity.
    Unusable,
}

// ---------------------------------------------------------------------------
// Wire models (default-tolerant; unknown fields are ignored by serde)
// ---------------------------------------------------------------------------

/// One credited artist on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireArtist {
    /// Discogs artist id, absent on some rows.
    #[serde(default)]
    pub id: Option<i64>,
    /// Artist name.
    #[serde(default)]
    pub name: String,
    /// "Artist name variation" as printed on the release.
    #[serde(default)]
    pub anv: String,
    /// Join phrase between this credit and the next.
    #[serde(default)]
    pub join: String,
}

/// One label row on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireLabel {
    /// Discogs label id.
    #[serde(default)]
    pub id: Option<i64>,
    /// Label name.
    #[serde(default)]
    pub name: String,
    /// Catalogue number.
    #[serde(default)]
    pub catno: String,
}

/// One identifier row on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireIdentifier {
    /// Identifier kind, e.g. "Barcode".
    #[serde(default)]
    pub r#type: String,
    /// Identifier value.
    #[serde(default)]
    pub value: String,
    /// Identifier description.
    #[serde(default)]
    pub description: String,
}

/// One format row on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireFormat {
    /// Format name, e.g. "Vinyl".
    #[serde(default)]
    pub name: String,
    /// Quantity as a string; live data is not always numeric.
    #[serde(default)]
    pub qty: String,
    /// Format descriptions.
    #[serde(default)]
    pub descriptions: Vec<String>,
    /// Free-text format note.
    #[serde(default)]
    pub text: String,
}

fn default_track_type() -> String {
    "track".to_owned()
}

/// One tracklist row on the wire. The live API really does spell the
/// discriminator `type_` (see the v2 `release_249504.json` fixture), so the
/// field keeps that name instead of renaming to `type`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireTrack {
    /// Printed position ("A1", "2-3", "4", ...).
    #[serde(default)]
    pub position: String,
    /// Row kind: "track", "heading", or "index".
    #[serde(default = "default_track_type")]
    pub type_: String,
    /// Track title.
    #[serde(default)]
    pub title: String,
    /// Duration as "M:SS" or "H:MM:SS".
    #[serde(default)]
    pub duration: String,
    /// Per-track artist credits.
    #[serde(default)]
    pub artists: Vec<WireArtist>,
    /// Index sub-tracks, flattened during normalization.
    #[serde(default)]
    pub sub_tracks: Vec<WireTrack>,
}

/// One release on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireRelease {
    /// Discogs release id; 0 means the payload has no identity.
    #[serde(default)]
    pub id: i64,
    /// Discogs master id, when the release belongs to one.
    #[serde(default)]
    pub master_id: Option<i64>,
    /// Release title.
    #[serde(default)]
    pub title: String,
    /// Pre-joined artist string.
    #[serde(default)]
    pub artists_sort: String,
    /// Release-level artist credits.
    #[serde(default)]
    pub artists: Vec<WireArtist>,
    /// Release year.
    #[serde(default)]
    pub year: Option<i64>,
    /// Country of release.
    #[serde(default)]
    pub country: String,
    /// Release date string.
    #[serde(default)]
    pub released: String,
    /// Label rows.
    #[serde(default)]
    pub labels: Vec<WireLabel>,
    /// Format rows.
    #[serde(default)]
    pub formats: Vec<WireFormat>,
    /// Identifier rows.
    #[serde(default)]
    pub identifiers: Vec<WireIdentifier>,
    /// Raw tracklist rows.
    #[serde(default)]
    pub tracklist: Vec<WireTrack>,
}

/// One search-result format row on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSearchFormat {
    /// Format name.
    #[serde(default)]
    pub name: String,
}

/// One `/database/search` hit on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSearchResult {
    /// Discogs release id; 0 means the hit has no identity.
    #[serde(default)]
    pub id: i64,
    /// Discogs master id.
    #[serde(default)]
    pub master_id: Option<i64>,
    /// "Artist - Title" display string.
    #[serde(default)]
    pub title: String,
    /// Release year.
    #[serde(default)]
    pub year: Option<i64>,
    /// Country string.
    #[serde(default)]
    pub country: String,
    /// Label names.
    #[serde(default)]
    pub label: Vec<String>,
    /// Catalogue number.
    #[serde(default)]
    pub catno: String,
    /// Flat format summary strings.
    #[serde(default)]
    pub format: Vec<String>,
    /// Structured format rows (fallback summary source).
    #[serde(default)]
    pub formats: Vec<WireSearchFormat>,
}

/// A `/database/search` page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSearchResponse {
    /// Search hits.
    #[serde(default)]
    pub results: Vec<WireSearchResult>,
}

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

/// Parse a "M:SS" or "H:MM:SS" duration (v2 `_duration_seconds`). Seconds,
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

/// True for heading/index rows (v2 `_normalize_media`: `type_` casefolded
/// against `"track"`).
pub fn is_heading(type_: &str) -> bool {
    !type_.eq_ignore_ascii_case("track")
}

/// Split a printed position into (medium, number) (v2 `_position`):
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

/// Flatten index sub-tracks depth-first, keeping parents before children
/// (v2 `_flatten_tracks`).
pub fn flatten_tracks(rows: &[WireTrack]) -> Vec<&WireTrack> {
    let mut flat = Vec::new();
    for row in rows {
        flat.push(row);
        flat.extend(flatten_tracks(&row.sub_tracks));
    }
    flat
}

/// Normalize one artist credit (v2 `_artist_credit`).
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

/// Group flattened rows into media (v2 `_normalize_media`). The fallback
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
/// failing the release (v2 `_normalized_formats`).
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

/// Normalize one release (v2 `normalize_release`). A payload without an id
/// or title is incomplete, never an empty release.
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
    // dashes stripped (v2 `normalize_release`).
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
/// at 200 characters (v2 `search_releases`).
pub fn normalize_query(query: &str) -> String {
    let collapsed = query.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(200).collect()
}

/// Normalize one search hit, skipping hits without an identity (v2
/// `search_releases`). The display title splits on the first " - " into
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
        year: item.year,
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

/// Parse `Retry-After` the way v2 does: a present value floors at one
/// second, while a missing or unparsable value waits a full minute
/// (v2 `_retry_after`).
pub fn retry_after_secs(value: Option<&str>) -> f64 {
    match value {
        Some(raw) => raw.parse::<f64>().map(|secs| secs.max(1.0)).unwrap_or(60.0),
        None => 60.0,
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Discogs catalog client. The clock stays outside: callers pass
/// `fetched_at` so wiring owns time, and caching/dedup wrap these calls.
pub struct DiscogsClient<'h, H: HttpPort> {
    /// Transport port (scripted fake in tests, real HTTP in wiring).
    pub http: &'h H,
    /// API origin; defaults to the live base.
    pub base_url: String,
}

impl<'h, H: HttpPort> DiscogsClient<'h, H> {
    /// Build a client against the live API origin.
    pub fn new(http: &'h H) -> Self {
        Self {
            http,
            base_url: API_BASE.to_owned(),
        }
    }

    /// Build a client against a scripted or mirrored origin.
    pub fn with_base(http: &'h H, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.to_owned(),
        }
    }

    /// Fetch one release. 404 is absence (`Ok(None)`); 429 stays actionable
    /// as `RateLimited`; 5xx and transport faults surface as `Transport` so
    /// wiring can retry; anything else is `Unusable` (v2 `_request`).
    pub async fn get_release(
        &self,
        release_id: &str,
        fetched_at: f64,
    ) -> Result<Option<Release>, FetchError> {
        let reply = self
            .http
            .get(&format!("{}/releases/{release_id}", self.base_url), &[])
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()),
            });
        }
        if reply.status >= 500 {
            return Err(FetchError::Transport);
        }
        if reply.status != 200 {
            return Err(FetchError::Unusable);
        }
        let wire: WireRelease =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        normalize_release(&wire, fetched_at)
            .map(Some)
            .map_err(|_| FetchError::Unusable)
    }

    /// Search releases. The limit clamps to 1..=10 and 404 is an empty page
    /// (v2 `search_releases`).
    pub async fn search_releases(
        &self,
        query: &str,
        limit: u32,
        fetched_at: f64,
    ) -> Result<Vec<ReleaseCandidate>, FetchError> {
        let bounded = limit.clamp(1, 10);
        let normalized = normalize_query(query);
        let per_page = bounded.to_string();
        let reply = self
            .http
            .get(
                &format!("{}/database/search", self.base_url),
                &[
                    ("type", "release"),
                    ("q", normalized.as_str()),
                    ("per_page", per_page.as_str()),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()),
            });
        }
        if reply.status >= 500 {
            return Err(FetchError::Transport);
        }
        if reply.status != 200 {
            return Err(FetchError::Unusable);
        }
        let wire: WireSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        Ok(wire
            .results
            .iter()
            .take(bounded as usize)
            .filter_map(|item| normalize_candidate(item, fetched_at))
            .collect())
    }
}
