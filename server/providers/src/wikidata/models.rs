//! Wikidata, Wikipedia and Wikimedia Commons wire shapes, read by the client in
//! `wikidata.rs`.
//!
//! Tolerant of unknown fields. Identity is required: an entity without its
//! `id`, a page that is neither found nor marked `missing` or `invalid`, a
//! Commons image row without its `url`, and a claim value without its `value`
//! each fail decoding, and so does a search hit without its title. Absence has
//! its own shapes: a page marked `missing` or `invalid`, a Commons file with no
//! image info, a property with no statement, a search with no hits, and an
//! error answer whose code names no entity or a merged item.

use std::collections::HashMap;

use serde::Deserialize;
use serde::de::IgnoredAny;

/// A Special:EntityData answer, `GET /wiki/Special:EntityData/<id>.json`.
///
/// Verified against www.wikidata.org/wiki/Special:EntityData/Q44190.json on 2026-10-20: tests/fixtures/wikidata/entity_found.json.
/// A redirected id, whose one entity is its target under the target's id: tests/fixtures/wikidata/entity_redirect.json.
#[derive(Debug, Clone, Deserialize)]
pub struct EntityData {
    /// Entities keyed by id. The client reads the one it asked for, or the
    /// single target of a redirect.
    pub entities: HashMap<String, Entity>,
}

/// One entity. Only its id and sitelinks are read.
#[derive(Debug, Clone, Deserialize)]
pub struct Entity {
    /// The entity id. Required: an entity without it fails decoding.
    #[allow(dead_code)]
    pub id: String,
    /// Sitelinks keyed by wiki id ("enwiki", ...).
    #[serde(default)]
    pub sitelinks: HashMap<String, Sitelink>,
}

/// One sitelink.
#[derive(Debug, Clone, Deserialize)]
pub struct Sitelink {
    /// The page title on that wiki.
    pub title: Option<String>,
}

/// A claims answer, `GET /w/api.php?action=wbgetclaims` on www.wikidata.org,
/// for one property or, with no `property`, for all of them. `wbgetclaims`
/// takes one property or none: `property=P18|P2722` is refused with an error
/// answer, so the artist picture lookup reads every claim in one call.
///
/// Verified against www.wikidata.org/w/api.php on 2026-10-20: tests/fixtures/wikidata/claims_p18_found.json.
/// Every claim of an item: tests/fixtures/wikidata/claims_all.json.
/// A property with no statement: tests/fixtures/wikidata/claims_no_statement.json.
/// An id that names no entity: tests/fixtures/wikidata/claims_no_such_entity.json.
/// An id the id builder refuses: tests/fixtures/wikidata/claims_invalid_id.json.
/// A merged item: tests/fixtures/wikidata/claims_unresolved_redirect.json.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ClaimsAnswer {
    /// The statements by property. A property with none is not in the map.
    Claims {
        /// Statements keyed by property id ("P18", ...).
        claims: HashMap<String, Vec<Claim>>,
    },
    /// An error answer. The API sends it with status 200.
    Refused {
        /// What the API refused.
        error: WikidataError,
    },
}

/// The error object of a Wikidata API answer.
#[derive(Debug, Clone, Deserialize)]
pub struct WikidataError {
    /// The error code, such as `no-such-entity`. Required.
    pub code: String,
}

/// One statement. Only its main value and rank are read.
#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    /// The main value. Absent for a statement with no value.
    #[serde(default)]
    pub mainsnak: Option<Snak>,
    /// `preferred`, `normal` or `deprecated`.
    #[serde(default)]
    pub rank: Option<String>,
}

impl Claim {
    /// The statement's string value, unless the statement is deprecated.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        if self.rank.as_deref() == Some("deprecated") {
            return None;
        }
        self.mainsnak.as_ref()?.datavalue.as_ref()?.value.text()
    }
}

/// The main value of a statement.
#[derive(Debug, Clone, Deserialize)]
pub struct Snak {
    /// The value. Absent for a "novalue" or "somevalue" snak.
    #[serde(default)]
    pub datavalue: Option<DataValue>,
}

/// A value with its kind. The kind is not read: the value decides.
#[derive(Debug, Clone, Deserialize)]
pub struct DataValue {
    /// The value. Required: a datavalue without it fails decoding.
    pub value: ClaimValue,
}

/// A datavalue's value. A `commonsMedia` statement carries its file name as a
/// string, and an `external-id` statement (such as P2722, the Deezer artist id)
/// its id. Entity references, quantities and times decode as `Other` and are
/// not read.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ClaimValue {
    /// A string: a Commons file name or an external id.
    Text(String),
    /// Any other value. Kept so one unread statement does not fail the answer.
    #[allow(dead_code)]
    Other(IgnoredAny),
}

impl ClaimValue {
    /// The value, when it is a string.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Other(_) => None,
        }
    }
}

/// The full-text search, `GET /w/api.php?action=query&list=search`, sent as
/// `srsearch=haswbstatement:P434=<mbid>`.
///
/// Verified against www.wikidata.org/w/api.php on 2026-10-20: tests/fixtures/wikidata/search_p434.json.
/// A search with no hits is an empty `search`: tests/fixtures/wikidata/search_p434_no_hits.json.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchAnswer {
    /// The search block. An error answer has none and fails decoding.
    pub query: SearchQuery,
}

/// The search block.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchQuery {
    /// Totals, when the API sends them.
    #[serde(default)]
    pub searchinfo: Option<SearchInfo>,
    /// The hits, at most as many as `srlimit` asked for.
    pub search: Vec<SearchHit>,
}

/// The search totals.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchInfo {
    /// Every item matching the query, beyond the hits sent.
    #[serde(default)]
    pub totalhits: Option<u64>,
}

/// One search hit. Its title is the item id (`Q...`) in the main namespace.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchHit {
    /// The item id. Required: a hit without it fails decoding.
    pub title: String,
}

/// The `query` block of a MediaWiki answer.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryBlock<P> {
    /// Pages keyed by page id, or by `-1` for a title that has no page.
    pub pages: HashMap<String, P>,
}

/// A Wikipedia answer to `action=query&prop=extracts`.
///
/// Verified against en.wikipedia.org/w/api.php on 2026-10-20: tests/fixtures/wikidata/extract_found.json.
/// A title with no page: tests/fixtures/wikidata/extract_not_found.json.
/// A title no page can carry: tests/fixtures/wikidata/extract_invalid.json.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractAnswer {
    /// The query block.
    pub query: QueryBlock<WikiPage>,
}

/// One page of a text query.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WikiPage {
    /// A page that exists.
    Found {
        /// The page id. Required: without it and without a mark, the page fails decoding.
        #[allow(dead_code)]
        pageid: i64,
        /// The plain-text intro. Absent or empty when the page has none.
        extract: Option<String>,
    },
    /// A title with no page behind it.
    Missing {
        /// Present, and empty, on a missing page.
        #[allow(dead_code)]
        missing: IgnoredAny,
    },
    /// A title that no page can carry.
    Invalid {
        /// Present, and empty, on an invalid title.
        #[allow(dead_code)]
        invalid: IgnoredAny,
    },
}

/// A Commons answer to `action=query&prop=imageinfo`.
///
/// Verified against commons.wikimedia.org/w/api.php on 2026-10-20: tests/fixtures/wikidata/commons_imageinfo_found.json.
/// A file that does not exist: tests/fixtures/wikidata/commons_imageinfo_not_found.json.
/// With `iiprop=url|extmetadata` and `iiextmetadatafilter=Artist|LicenseShortName|LicenseUrl`,
/// verified on 2026-10-21: tests/fixtures/wikidata/commons_imageinfo_credit.json.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageAnswer {
    /// The query block.
    pub query: QueryBlock<ImagePage>,
}

/// One Commons page. A file that does not exist carries no image info.
#[derive(Debug, Clone, Deserialize)]
pub struct ImagePage {
    /// Image info rows.
    #[serde(default)]
    pub imageinfo: Vec<ImageInfo>,
}

/// One image info row.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageInfo {
    /// The file's own URL. Required: a row without it fails decoding.
    pub url: String,
    /// The scaled thumbnail, when Commons made one for the width asked.
    pub thumburl: Option<String>,
    /// The file's page on Commons.
    #[serde(default)]
    pub descriptionurl: Option<String>,
    /// The author and licence fields asked for, when the file page has them.
    #[serde(default)]
    pub extmetadata: Option<ExtMetadata>,
}

/// The `extmetadata` fields the credit reads. Each is absent when the file
/// page does not state it.
///
/// Verified on 2026-10-21 (tests/fixtures/wikidata/commons_imageinfo_credit.json):
/// each field is an object with a string `value` and a `source`
/// (`commons-desc-page`). `Artist` is HTML, here a link to the photographer's
/// Flickr page; `LicenseShortName` is plain text (`CC BY 4.0`) and
/// `LicenseUrl` a bare URL.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtMetadata {
    /// Who made the file, as HTML.
    #[serde(rename = "Artist", default)]
    pub artist: Option<MetadataField>,
    /// The licence's short name, such as `CC BY-SA 4.0`.
    #[serde(rename = "LicenseShortName", default)]
    pub license_short_name: Option<MetadataField>,
    /// The licence's own page.
    #[serde(rename = "LicenseUrl", default)]
    pub license_url: Option<MetadataField>,
}

/// One `extmetadata` field.
#[derive(Debug, Clone, Deserialize)]
pub struct MetadataField {
    /// The field's value. A value that is not a string is not read.
    #[serde(default)]
    pub value: Option<MetadataValue>,
}

impl MetadataField {
    /// The value, when it is a string.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self.value.as_ref()? {
            MetadataValue::Text(text) => Some(text),
            MetadataValue::Other(_) => None,
        }
    }
}

/// An `extmetadata` value: a string, or anything else, which is not read.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    /// A string value.
    Text(String),
    /// Any other value. Kept so one odd field does not fail the answer.
    #[allow(dead_code)]
    Other(IgnoredAny),
}
