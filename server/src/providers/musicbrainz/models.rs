//! MusicBrainz wire shapes.
//!
//! Tolerant of unknown fields (serde skips them), optional where the
//! service is sparse, and strict only on identity. Every struct with an
//! `id` requires it: a missing entity id fails decoding with a contract
//! error, never a defaulted empty value that looks real.

use std::collections::HashMap;

use serde::Deserialize;

use super::MbError;

/// One artist-credit entry: credited name, join phrase, and artist.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArtistCreditName {
    /// Name as credited on this release (differs from canonical at times).
    #[serde(default)]
    pub name: String,
    /// Exact join phrase (`"; "`, `" & "`, `""`); provider evidence, never
    /// reconstructed (live 2026-07-31, management notes).
    #[serde(default)]
    pub joinphrase: String,
    /// The credited artist.
    pub artist: ArtistRef,
}

/// Minimal artist reference inside credits.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArtistRef {
    /// Artist MBID: required identity.
    pub id: String,
    /// Canonical artist name.
    #[serde(default)]
    pub name: String,
    /// Sort name (`"Beatles, The"` style), when the service sends one.
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
}

/// Release-group reference shared by search hits and release lookups.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseGroupRef {
    /// Release-group MBID: required identity.
    pub id: String,
    /// Group title.
    #[serde(default)]
    pub title: Option<String>,
    /// First release date (`YYYY`, `YYYY-MM`, or `YYYY-MM-DD`).
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    /// Primary type; explicitly null on some groups (live 2026-08-15:
    /// "Haunt Me" returned null `primary-type` and `primary-type-id`).
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    /// Primary-type MBID; nullable for the same reason (the identifier was
    /// once modelled required and poisoned the shared breaker).
    #[serde(rename = "primary-type-id", default)]
    pub primary_type_id: Option<String>,
    /// Secondary types (`Compilation`, `Live`, `Remix`, ...).
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    /// Artist credit, when the include set carries it.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
}

/// Label reference inside label-info entries.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LabelRef {
    /// Label MBID: required identity.
    pub id: String,
    /// Label name.
    #[serde(default)]
    pub name: Option<String>,
}

/// One label-info entry: catalogue number plus a nullable label object.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LabelInfo {
    /// Catalogue number; explicitly null on some entries even when the
    /// label object is present (live 2026-07-28, Anthony Green _Avalon_).
    #[serde(rename = "catalog-number", default)]
    pub catalog_number: Option<String>,
    /// The label, or null.
    #[serde(default)]
    pub label: Option<LabelRef>,
}

/// Medium summary inside releases.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Medium {
    /// Medium position within the release.
    #[serde(default)]
    pub position: Option<u32>,
    /// Medium title, when set.
    #[serde(default)]
    pub title: Option<String>,
    /// Format (`CD`, `Vinyl`, ...); sparse on search summaries.
    #[serde(default)]
    pub format: Option<String>,
    /// Track count; sparse on search summaries.
    #[serde(rename = "track-count", default)]
    pub track_count: Option<u32>,
    /// Tracks, present only with the `recordings` include.
    #[serde(default)]
    pub tracks: Vec<Track>,
}

/// Recording reference inside release tracks.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RecordingRef {
    /// Recording MBID: required identity, distinct from the track id.
    pub id: String,
    /// Recording title: fallback only; edition surfaces prefer the
    /// release-track title (live 2026-07-29: Avalon track 14 differs from
    /// its recording by one word).
    #[serde(default)]
    pub title: Option<String>,
    /// Recording length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// Recording artist credit: fallback only; a present release-track
    /// credit wins (live 2026-07-31: Bach on the track, Gould on the
    /// recording of the same Goldberg track).
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
}

/// One release track. The release-track `id` is not the recording MBID:
/// management retains both and never derives one from the other (live
/// 2026-07-21, management notes).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Track {
    /// Release-track MBID: required identity.
    pub id: String,
    /// Numeric position within the medium.
    #[serde(default)]
    pub position: Option<u32>,
    /// Display number (`"A1"`, `"14"`).
    #[serde(default)]
    pub number: Option<String>,
    /// Release-track title: preferred over the recording title.
    #[serde(default)]
    pub title: Option<String>,
    /// Track length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// Release-track credit, when the release carries its own.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Option<Vec<ArtistCreditName>>,
    /// Linked recording, with the `recordings` include.
    #[serde(default)]
    pub recording: Option<RecordingRef>,
}

impl Track {
    /// Edition title: release-track title first, recording title fallback.
    pub fn display_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .or_else(|| self.recording.as_ref()?.title.as_deref())
    }

    /// Edition credit: release-track credit first, recording fallback.
    pub fn credit(&self) -> &[ArtistCreditName] {
        if let Some(credit) = self.artist_credit.as_ref() {
            return credit;
        }
        self.recording
            .as_ref()
            .map_or(&[], |recording| &recording.artist_credit)
    }

    /// Length in milliseconds, track first then recording.
    pub fn length_ms(&self) -> Option<u64> {
        self.length.or_else(|| self.recording.as_ref()?.length)
    }
}

/// Linked work reference inside relationships.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WorkRef {
    /// Work MBID: required identity.
    pub id: String,
    /// Work title.
    #[serde(default)]
    pub title: Option<String>,
    /// Work type display string; explicitly null on some linked works.
    #[serde(rename = "type", default)]
    pub work_type: Option<String>,
    /// Work type MBID; explicitly null alongside it (live 2026-08-03) and
    /// must not fail decoding of an otherwise valid release.
    #[serde(rename = "type-id", default)]
    pub type_id: Option<String>,
}

/// Generic entity pointer for relationship targets.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct EntityRef {
    /// Target MBID: required identity.
    pub id: String,
}

/// One relationship. Relation `type-id` fields stay required: a
/// relationship-rich probe on 2026-08-15 returned null only for relation
/// `begin`/`end`, so identifiers stay strict until a live payload proves
/// otherwise (management notes).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Relation {
    /// Relationship type name.
    #[serde(rename = "type")]
    pub rel_type: String,
    /// Relationship type MBID: required until proven otherwise.
    #[serde(rename = "type-id")]
    pub type_id: String,
    /// Linked work, for work relationships.
    #[serde(default)]
    pub work: Option<WorkRef>,
    /// Linked artist, for artist relationships.
    #[serde(default)]
    pub artist: Option<EntityRef>,
    /// Linked release, for release relationships.
    #[serde(default)]
    pub release: Option<EntityRef>,
    /// Linked release group, for release-group relationships.
    #[serde(rename = "release-group", default)]
    pub release_group: Option<EntityRef>,
    /// Linked URL, for URL relationships (`url-rels` include).
    #[serde(default)]
    pub url: Option<UrlRef>,
    /// Whether the relationship has ended (a dead store page, an old
    /// homepage).
    #[serde(default)]
    pub ended: bool,
}

/// Linked URL inside a URL relationship.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct UrlRef {
    /// The URL itself: the only field a URL relationship is useful for.
    pub resource: String,
}

/// One folksonomy tag with its vote count.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Tag {
    /// Tag text.
    pub name: String,
    /// Vote count, when sent.
    #[serde(default)]
    pub count: Option<i64>,
}

/// One artist alias.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Alias {
    /// Alias text.
    pub name: String,
}

/// Full release lookup document (identity-readiness surface).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbRelease {
    /// Release MBID: required identity.
    pub id: String,
    /// Release title.
    #[serde(default)]
    pub title: Option<String>,
    /// Disambiguation comment (`"deluxe edition"`, `"remaster"`).
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// Status; explicitly null on some releases (live 2026-08-15: "I
    /// Fought the Law" returned null `status` and `status-id`).
    #[serde(default)]
    pub status: Option<String>,
    /// Status MBID; nullable for the same reason.
    #[serde(rename = "status-id", default)]
    pub status_id: Option<String>,
    /// Release date.
    #[serde(default)]
    pub date: Option<String>,
    /// Release country.
    #[serde(default)]
    pub country: Option<String>,
    /// Barcode.
    #[serde(default)]
    pub barcode: Option<String>,
    /// Amazon identifier, when present.
    #[serde(default)]
    pub asin: Option<String>,
    /// Packaging display string; explicitly null on several releases.
    #[serde(default)]
    pub packaging: Option<String>,
    /// Packaging MBID; explicitly null alongside it (live 2026-07-28).
    #[serde(rename = "packaging-id", default)]
    pub packaging_id: Option<String>,
    /// Release artist credit.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Label info entries.
    #[serde(rename = "label-info", default)]
    pub label_info: Vec<LabelInfo>,
    /// Media, with track detail under the `recordings` include.
    #[serde(default)]
    pub media: Vec<Medium>,
    /// Release group: present only with the `release-groups` include (live
    /// 2026-08-10: Clairo _Immunity_ omits it without that include).
    #[serde(rename = "release-group", default)]
    pub release_group: Option<ReleaseGroupRef>,
    /// Relationships, under the relation includes.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Release-group lookup document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbReleaseGroup {
    /// Release-group MBID: required identity.
    pub id: String,
    /// Group title.
    #[serde(default)]
    pub title: Option<String>,
    /// First release date.
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    /// Primary type; nullable (live 2026-08-15).
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    /// Primary-type MBID; nullable (live 2026-08-15).
    #[serde(rename = "primary-type-id", default)]
    pub primary_type_id: Option<String>,
    /// Secondary types.
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    /// Artist credit.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Sibling releases, under the `releases` include.
    #[serde(default)]
    pub releases: Vec<MbRelease>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// Tags, under the `tags` include.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Relationships, under the relation includes.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Area reference on artists.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AreaRef {
    /// Area MBID: required identity.
    pub id: String,
    /// Area name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Artist life span.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LifeSpan {
    /// Begin date, partial shapes allowed.
    #[serde(default)]
    pub begin: Option<String>,
    /// End date, partial shapes allowed.
    #[serde(default)]
    pub end: Option<String>,
    /// Whether the artist has ended (a split band, a deceased person).
    #[serde(default)]
    pub ended: Option<bool>,
}

/// Artist lookup document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbArtist {
    /// Artist MBID: required identity.
    pub id: String,
    /// Canonical name.
    #[serde(default)]
    pub name: Option<String>,
    /// Sort name.
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// Artist type (`Person`, `Group`, ...).
    #[serde(rename = "type", default)]
    pub artist_type: Option<String>,
    /// Gender, for persons.
    #[serde(default)]
    pub gender: Option<String>,
    /// Home area.
    #[serde(default)]
    pub area: Option<AreaRef>,
    /// Life span.
    #[serde(rename = "life-span", default)]
    pub life_span: Option<LifeSpan>,
    /// ISO country code.
    #[serde(default)]
    pub country: Option<String>,
    /// Tags, under the `tags` include.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Aliases, under the `aliases` include.
    #[serde(default)]
    pub aliases: Vec<Alias>,
    /// Relationships, under the `url-rels` include.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Release attached to a recording lookup.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RecordingRelease {
    /// Release MBID: required identity.
    pub id: String,
    /// Release status (`Official`, `Bootleg`, ...).
    #[serde(default)]
    pub status: Option<String>,
    /// Release date.
    #[serde(default)]
    pub date: Option<String>,
    /// Release group carrying the ranking fields (live 2026-07-20,
    /// `musicbrainz_API_NOTES.md`).
    #[serde(rename = "release-group", default)]
    pub release_group: Option<ReleaseGroupRef>,
}

/// Recording lookup document.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MbRecording {
    /// Recording MBID: required identity (canonical after redirects).
    pub id: String,
    /// Recording title.
    #[serde(default)]
    pub title: Option<String>,
    /// Length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// ISRCs.
    #[serde(default)]
    pub isrcs: Vec<String>,
    /// Recording artist credit.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Releases carrying this recording, under `inc=releases`.
    #[serde(default)]
    pub releases: Vec<RecordingRelease>,
    /// Relationships, under the relation includes.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

/// Release search hit. The search index carries release-group,
/// artist-credit, label-info, and medium facets without any `inc`
/// parameter; optional fields stay absent on many releases (live 2026-08-11,
/// `musicbrainz_release_search_models.py`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseSearchHit {
    /// Release MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Release title.
    #[serde(default)]
    pub title: Option<String>,
    /// Artist credit facet.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Release-group facet.
    #[serde(rename = "release-group", default)]
    pub release_group: Option<ReleaseGroupRef>,
    /// Release date.
    #[serde(default)]
    pub date: Option<String>,
    /// Release country.
    #[serde(default)]
    pub country: Option<String>,
    /// Release status.
    #[serde(default)]
    pub status: Option<String>,
    /// Packaging.
    #[serde(default)]
    pub packaging: Option<String>,
    /// Medium facet.
    #[serde(default)]
    pub media: Vec<Medium>,
    /// Label-info facet.
    #[serde(rename = "label-info", default)]
    pub label_info: Vec<LabelInfo>,
    /// Barcode.
    #[serde(default)]
    pub barcode: Option<String>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
}

/// Release-group search hit.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseGroupSearchHit {
    /// Release-group MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Group title.
    #[serde(default)]
    pub title: Option<String>,
    /// Primary type.
    #[serde(rename = "primary-type", default)]
    pub primary_type: Option<String>,
    /// Secondary types.
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    /// First release date.
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    /// Artist credit facet.
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCreditName>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
}

/// Artist search hit.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArtistSearchHit {
    /// Artist MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Canonical name.
    #[serde(default)]
    pub name: Option<String>,
    /// Sort name.
    #[serde(rename = "sort-name", default)]
    pub sort_name: Option<String>,
    /// Disambiguation comment.
    #[serde(default)]
    pub disambiguation: Option<String>,
    /// Artist type.
    #[serde(rename = "type", default)]
    pub artist_type: Option<String>,
    /// Home area.
    #[serde(default)]
    pub area: Option<AreaRef>,
}

/// Recording search hit: candidate plus the releases it appears on.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RecordingSearchHit {
    /// Recording MBID: required identity.
    pub id: String,
    /// Plain score key.
    #[serde(default)]
    pub score: Option<i64>,
    /// Namespaced score key.
    #[serde(rename = "ext:score", default)]
    pub ext_score: Option<i64>,
    /// Recording title.
    #[serde(default)]
    pub title: Option<String>,
    /// Recording length in milliseconds.
    #[serde(default)]
    pub length: Option<u64>,
    /// Releases carrying this recording.
    #[serde(default)]
    pub releases: Vec<RecordingRelease>,
}

/// Search page: items plus the envelope counters. The wire shape is
/// `{count, created, offset, <entities>}` (contribution probe 2026-07-21);
/// `created` is ignored and each search reads its own entity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage<T> {
    /// Total matches in the index.
    pub count: u64,
    /// Offset of this page.
    pub offset: u64,
    /// Decoded page items.
    pub items: Vec<T>,
}

/// Decode one search page from the entity array inside the envelope.
pub(crate) fn decode_search_page<T>(body: &[u8], array_key: &str) -> Result<SearchPage<T>, MbError>
where
    T: for<'de> Deserialize<'de>,
{
    let envelope: HashMap<String, serde_json::Value> = serde_json::from_slice(body)
        .map_err(|error| MbError::Contract(format!("unparseable search payload: {error}")))?;
    let missing_id = |error: serde_json::Error| {
        MbError::Contract(format!("search hit breaks the identity contract: {error}"))
    };
    let items = match envelope.get(array_key) {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value.clone()).map_err(missing_id)?,
    };
    let count = envelope
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let offset = envelope
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(SearchPage {
        count,
        offset,
        items,
    })
}

/// URL-resolution response: the full relation list, retained so a
/// multi-target response reads as ambiguity rather than silently selecting
/// its first item (live 2026-07-21, contribution notes).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct UrlResolution {
    /// Resource URL echoed back.
    #[serde(default)]
    pub resource: Option<String>,
    /// Every relation found; 404 resolves to an empty list, not an error.
    #[serde(default)]
    pub relations: Vec<Relation>,
}

impl UrlResolution {
    /// Empty resolution for a 404: the URL simply has no relations.
    pub fn empty(resource: &str) -> Self {
        Self {
            resource: Some(resource.to_owned()),
            relations: Vec::new(),
        }
    }
}

/// One page of an artist's release groups from the browse endpoint
/// (`/release-group?artist=`). The envelope is
/// `{"release-group-count", "release-group-offset", "release-groups"}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseGroupBrowsePage {
    /// Total release groups the artist has.
    #[serde(rename = "release-group-count", default)]
    pub count: u64,
    /// Offset of this page.
    #[serde(rename = "release-group-offset", default)]
    pub offset: u64,
    /// This page's release groups.
    #[serde(rename = "release-groups", default)]
    pub items: Vec<ReleaseGroupRef>,
}
