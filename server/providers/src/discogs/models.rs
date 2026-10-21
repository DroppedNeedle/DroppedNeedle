//! Discogs wire shapes, read by the release lookup and the release search in
//! `discogs.rs`.
//!
//! Tolerant of unknown fields. A release or a search hit without its `id`
//! fails decoding: the id is the identity, and a default of 0 would read as
//! an answer with no release behind it. The other fields are optional or
//! default to empty, and the normalization refuses or skips what is empty.

use serde::Deserialize;

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

/// One tracklist row on the wire. The live API spells the discriminator
/// `type_` (tests/fixtures/discogs/release_249504.json), so the field keeps
/// that name instead of renaming to `type`.
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
///
/// Verified against api.discogs.com/releases/{id} on 2026-10-21: tests/fixtures/discogs/release_249504.json.
/// Verified against api.discogs.com/releases/{id} on 2026-10-21: tests/fixtures/discogs/release_3562468.json.
/// Nothing found, recorded on 2026-10-21: tests/fixtures/discogs/release_not_found.json.
#[derive(Debug, Clone, Deserialize)]
pub struct WireRelease {
    /// Discogs release id: required identity. A payload without it fails
    /// decoding, and a zero id is refused at normalization.
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
#[derive(Debug, Clone, Deserialize)]
pub struct WireSearchResult {
    /// Discogs release id: required identity. A hit without it fails the page.
    pub id: i64,
    /// Discogs master id.
    #[serde(default)]
    pub master_id: Option<i64>,
    /// "Artist - Title" display string.
    #[serde(default)]
    pub title: String,
    /// Release year as text ("2015"). Absent on some hits.
    #[serde(default)]
    pub year: Option<String>,
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
///
/// Verified against api.discogs.com/database/search on 2026-10-21: tests/fixtures/discogs/search_found.json.
/// No results, recorded on 2026-10-21: tests/fixtures/discogs/search_no_results.json.
#[derive(Debug, Clone, Deserialize)]
pub struct WireSearchResponse {
    /// Search hits. Required: a page without the list fails decoding. An
    /// empty list is the no-hit answer.
    pub results: Vec<WireSearchResult>,
}
