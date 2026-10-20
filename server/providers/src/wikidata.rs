//! Artist bios, images, and relation targets via Wikidata/Wikipedia/Commons.
//!
//! A Rust port of v2's Wikidata repository. Bios
//! arrive in two hops (Wikidata entity to the English Wikipedia title, then
//! the plain-text intro extract); artist images arrive in two more (the P18
//! "image" claim filename, then the Commons file URL). Relation targets
//! reuse the same claims path with a caller-chosen property.
//!
//! Absence-vs-outage semantics, carried over from v2: a
//! clean miss (no sitelink, no extract, no claim) is `Ok(None)` and is
//! safe for wiring to negative-cache briefly, while `Err` means the fetch
//! itself failed and must stay uncached so the next request retries. v2
//! parks a falsy `""` sentinel for 600s on clean misses, caches extracts
//! for 7 days and images for 24h; those TTLs belong to the wiring.
//!
//! Requests to all three wikis go through the executor at the
//! `wikimedia-api` row, under the robot policy's rate and concurrency of
//! one. Dedup and caching stay with wiring. One intended difference: v2
//! answers `None` for any non-200 status, while this client answers
//! `Ok(None)` only for 404 and a typed [`ProviderError`] otherwise, so an
//! outage cannot be mistaken for "this artist has no bio".

use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

/// The provider name errors carry.
pub const SOURCE: &str = "wikidata";
/// The rate row Wikidata, Wikipedia and Commons share.
pub const RATE_ROW: &str = "wikimedia-api";
/// Largest answer read: entities carry every language.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The wikis on the wire: the standard rules, and a 429 that names no wait
/// rests a minute (v2's default).
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

/// Live Wikidata origin.
pub const WIKIDATA_BASE: &str = "https://www.wikidata.org";
/// Live Wikipedia origin pattern; `{lang}` selects the edition.
pub const WIKIPEDIA_PATTERN: &str = "https://{lang}.wikipedia.org";
/// Live Commons origin.
pub const COMMONS_BASE: &str = "https://commons.wikimedia.org";
/// The "image" property behind artist photos.
pub const PROPERTY_IMAGE: &str = "P18";

// ---------------------------------------------------------------------------
// Wire models (default-tolerant; unknown fields are ignored by serde)
// ---------------------------------------------------------------------------

/// One sitelink on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSitelink {
    /// Page title on the linked wiki.
    #[serde(default)]
    pub title: Option<String>,
}

/// One entity on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireEntity {
    /// Sitelinks keyed by wiki id ("enwiki", ...).
    #[serde(default)]
    pub sitelinks: HashMap<String, WireSitelink>,
}

/// An entity-data answer on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireEntityResponse {
    /// Entities keyed by id ("Q...").
    #[serde(default)]
    pub entities: HashMap<String, WireEntity>,
}

/// One claim datavalue on the wire. The value keeps its raw JSON form: P18
/// filenames are strings, while relation targets are `{"id": "Q..."}` entity
/// objects, and both must decode without failing the page.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireDatavalue {
    /// Raw datavalue.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

impl WireDatavalue {
    /// The value when it is a plain string (P18 filenames).
    pub fn string_value(&self) -> Option<&str> {
        self.value.as_ref().and_then(serde_json::Value::as_str)
    }
}

/// One claim snak on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSnak {
    /// Main datavalue.
    #[serde(default)]
    pub datavalue: Option<WireDatavalue>,
}

/// One claim on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireClaim {
    /// Main snak.
    #[serde(default)]
    pub mainsnak: Option<WireSnak>,
}

/// A claims answer on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireClaimsResponse {
    /// Claims keyed by property ("P18", ...).
    #[serde(default)]
    pub claims: HashMap<String, Vec<WireClaim>>,
}

/// One Wikipedia page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireWikiPage {
    /// Page id; missing or negative means the page does not exist.
    #[serde(default)]
    pub pageid: Option<i64>,
    /// Plain-text intro extract.
    #[serde(default)]
    pub extract: Option<String>,
}

/// One MediaWiki query block on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireQuery {
    /// Pages keyed by page id.
    #[serde(default)]
    pub pages: HashMap<String, WireWikiPage>,
}

/// A Wikipedia query answer on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireQueryResponse {
    /// Query block.
    #[serde(default)]
    pub query: Option<WireQuery>,
}

/// Width Commons scales artist portraits to: big enough for any artist
/// header, without downloading multi-megabyte originals. A standard Commons
/// thumbnail width (https://www.mediawiki.org/wiki/Common_thumbnail_sizes).
/// Checked 2026-10-19: imageinfo at 1280 for File:RadioheadO2211125
/// composite.jpg (5940 px original) returned a 1280 px thumburl on
/// thumb.wikimedia.org. The stored portrait is still shrunk to 1200 px.
pub const COMMONS_THUMB_WIDTH: &str = "1280";

/// One Commons imageinfo row on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireImageInfo {
    /// Direct file URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Scaled thumbnail URL, when one was asked for (`iiurlwidth`).
    #[serde(default)]
    pub thumburl: Option<String>,
}

/// One Commons page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireCommonsPage {
    /// Image info rows.
    #[serde(default)]
    pub imageinfo: Vec<WireImageInfo>,
}

/// One Commons query block on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireCommonsQuery {
    /// Pages keyed by page id.
    #[serde(default)]
    pub pages: HashMap<String, WireCommonsPage>,
}

/// A Commons query answer on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireCommonsResponse {
    /// Query block.
    #[serde(default)]
    pub query: Option<WireCommonsQuery>,
}

// ---------------------------------------------------------------------------
// Pure helpers (ports of the v2 static helpers)
// ---------------------------------------------------------------------------

/// Pull a `Q...` id out of a Wikidata URL (v2 `_extract_wikidata_id`: the
/// `/wiki/(Q\d+)` pattern).
pub fn extract_wikidata_id(url: &str) -> Option<String> {
    let marker = "/wiki/";
    let mut rest = url;
    while let Some(pos) = rest.find(marker) {
        rest = &rest[pos + marker.len()..];
        if let Some(id) = wikidata_id_at(rest) {
            return Some(id);
        }
    }
    None
}

/// Read a `Q...` id at the start of `rest`, if one is there.
fn wikidata_id_at(rest: &str) -> Option<String> {
    let digits: String = rest
        .strip_prefix('Q')?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(format!("Q{digits}"))
    }
}

/// Pull a page title out of a Wikipedia URL (v2 `_extract_wikipedia_title`:
/// the `/wiki/(.+)$` pattern), percent-decoded: it goes out again as a
/// query value, which encodes it once.
pub fn extract_wikipedia_title(url: &str) -> Option<String> {
    url.find("/wiki/").and_then(|pos| {
        let title = &url[pos + "/wiki/".len()..];
        if title.is_empty() {
            None
        } else {
            Some(percent_decode(title))
        }
    })
}

/// Decode `%XX` escapes; text that does not decode to UTF-8 is kept as it
/// came.
fn percent_decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8()
        .map_or_else(|_| text.to_owned(), |decoded| decoded.into_owned())
}

/// True for a Wikidata item id (`Q` and digits), the only kind sent.
fn is_item_id(value: &str) -> bool {
    value
        .strip_prefix('Q')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// True for a wiki language code (`en`, `pt-br`, `be-tarask`).
fn is_language(value: &str) -> bool {
    (2..=12).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

/// Read the `{lang}wiki` sitelink title for one entity, or `None` when the
/// entity or sitelink is missing (v2 `_get_wikipedia_title_from_wikidata`).
pub fn sitelink_title(page: &WireEntityResponse, entity_id: &str, lang: &str) -> Option<String> {
    page.entities
        .get(entity_id)
        .and_then(|entity| entity.sitelinks.get(&format!("{lang}wiki")))
        .and_then(|link| link.title.clone())
        .filter(|title| !title.is_empty())
}

/// Read the first usable extract from a Wikipedia answer (v2
/// `_fetch_wikipedia_extract`): a missing page (absent or negative id) ends
/// the lookup as absent, and the first non-empty extract wins.
pub fn select_extract(page: &WireQueryResponse) -> Option<String> {
    let pages = page.query.as_ref().map(|query| &query.pages)?;
    // Single-title lookups carry one page; iterate deterministically anyway.
    let mut keys: Vec<&String> = pages.keys().collect();
    keys.sort();
    for key in keys {
        let Some(entry) = pages.get(key) else {
            continue;
        };
        if entry.pageid.unwrap_or(-1) < 0 {
            return None;
        }
        if let Some(extract) = entry.extract.as_deref().filter(|text| !text.is_empty()) {
            return Some(extract.to_owned());
        }
    }
    None
}

/// Read the first usable Commons file URL (v2
/// `_load_artist_image_from_wikidata`): the first page carrying image info
/// decides, and an empty URL there ends the lookup as absent.
pub fn select_commons_url(page: &WireCommonsResponse) -> Option<String> {
    let pages = page.query.as_ref().map(|query| &query.pages)?;
    let mut keys: Vec<&String> = pages.keys().collect();
    keys.sort();
    for key in keys {
        let Some(entry) = pages.get(key) else {
            continue;
        };
        if entry.imageinfo.is_empty() {
            continue;
        }
        // The scaled thumbnail when Commons made one, else the original.
        return entry
            .imageinfo
            .first()
            .and_then(|info| {
                info.thumburl
                    .clone()
                    .filter(|url| !url.is_empty())
                    .or_else(|| info.url.clone())
            })
            .filter(|url| !url.is_empty());
    }
    None
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Wikidata/Wikipedia/Commons client. Built once with the providers; cheap
/// to clone.
#[derive(Debug, Clone)]
pub struct WikidataClient {
    executor: Executor,
    wikidata: Option<Url>,
    /// Wikipedia origin pattern with `{lang}`.
    wikipedia_pattern: String,
    commons: Option<Url>,
}

impl WikidataClient {
    /// The client sending through `executor` to the given origins.
    #[must_use]
    pub fn new(
        executor: Executor,
        wikidata_base: &str,
        wikipedia_pattern: &str,
        commons_base: &str,
    ) -> Self {
        Self {
            executor,
            wikidata: parse_base(wikidata_base, SOURCE),
            wikipedia_pattern: wikipedia_pattern.trim().to_owned(),
            commons: parse_base(commons_base, SOURCE),
        }
    }

    /// The Wikipedia origin for `lang`, or `None` for a code that is not a
    /// language.
    fn wikipedia(&self, lang: &str) -> Option<Url> {
        if !is_language(lang) {
            return None;
        }
        let base = Url::parse(&self.wikipedia_pattern.replace("{lang}", lang)).ok()?;
        (matches!(base.scheme(), "http" | "https") && !base.cannot_be_a_base()).then_some(base)
    }

    /// One GET of `segments` under `base`, or `None` on a 404.
    async fn get(
        &self,
        base: Option<&Url>,
        segments: &[&str],
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        let base = base.ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, segments, query).map_err(|_| unusable_base(SOURCE))?;
        match self.executor.send(request, priority).await {
            Ok(reply) => Ok(Some(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Resolve a Wikidata id to its Wikipedia title in one language (v2
    /// `_get_wikipedia_title_from_wikidata`). An id that is not an item id
    /// is never sent and reads as absence.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn wikipedia_title_from_wikidata(
        &self,
        wikidata_id: &str,
        lang: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        if !is_item_id(wikidata_id) {
            return Ok(None);
        }
        let document = format!("{wikidata_id}.json");
        let Some(reply) = self
            .get(
                self.wikidata.as_ref(),
                &["wiki", "Special:EntityData", document.as_str()],
                &[],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let page: WireEntityResponse = decode(&reply)?;
        Ok(sitelink_title(&page, wikidata_id, lang))
    }

    /// Fetch one plain-text intro extract (v2 `_fetch_wikipedia_extract`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn wikipedia_extract(
        &self,
        page_title: &str,
        lang: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let Some(reply) = self
            .get(
                self.wikipedia(lang).as_ref(),
                &["w", "api.php"],
                &[
                    ("action", "query"),
                    ("titles", page_title),
                    ("prop", "extracts"),
                    ("exintro", "1"),
                    ("explaintext", "1"),
                    ("format", "json"),
                ],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let page: WireQueryResponse = decode(&reply)?;
        Ok(select_extract(&page))
    }

    /// The bio extract behind a Wikidata or Wikipedia URL (v2
    /// `get_wikipedia_extract` without its cache/dedup shell, which stays
    /// with wiring). A Wikidata URL hops through the entity's sitelink;
    /// anything else is read as a Wikipedia title directly.
    ///
    /// # Errors
    ///
    /// The typed failure of either call or of its answer.
    pub async fn get_bio_extract(
        &self,
        wiki_url: &str,
        lang: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let title = if let Some(id) = extract_wikidata_id(wiki_url) {
            self.wikipedia_title_from_wikidata(&id, lang, priority)
                .await?
        } else {
            extract_wikipedia_title(wiki_url)
        };
        let Some(title) = title else {
            return Ok(None);
        };
        self.wikipedia_extract(&title, lang, priority).await
    }

    /// The artist image behind a Wikidata id (v2
    /// `get_artist_image_from_wikidata` without its cache/dedup shell): the
    /// first P18 claim's filename resolves through Commons image info.
    ///
    /// # Errors
    ///
    /// The typed failure of either call or of its answer.
    pub async fn get_artist_image(
        &self,
        wikidata_id: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let filename = self
            .claim_string(wikidata_id, PROPERTY_IMAGE, priority)
            .await?;
        let Some(filename) = filename.filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        let titles = format!("File:{filename}");
        let Some(reply) = self
            .get(
                self.commons.as_ref(),
                &["w", "api.php"],
                &[
                    ("action", "query"),
                    ("titles", titles.as_str()),
                    ("prop", "imageinfo"),
                    ("iiprop", "url"),
                    ("iiurlwidth", COMMONS_THUMB_WIDTH),
                    ("format", "json"),
                ],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let page: WireCommonsResponse = decode(&reply)?;
        Ok(select_commons_url(&page))
    }

    /// The first claim's string value for one property (the P18 path behind
    /// `get_artist_image`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn claim_string(
        &self,
        wikidata_id: &str,
        property: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let page = self.fetch_claims(wikidata_id, property, priority).await?;
        let Some(page) = page else {
            return Ok(None);
        };
        Ok(page
            .claims
            .get(property)
            .and_then(|claims| claims.first())
            .and_then(|claim| claim.mainsnak.as_ref())
            .and_then(|snak| snak.datavalue.as_ref())
            .and_then(WireDatavalue::string_value)
            .map(str::to_owned))
    }

    /// Fetch one property's claims, with 404 as absence. An id that is not
    /// an item id is never sent and reads as absence.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn fetch_claims(
        &self,
        wikidata_id: &str,
        property: &str,
        priority: RequestPriority,
    ) -> Result<Option<WireClaimsResponse>, ProviderError> {
        if !is_item_id(wikidata_id) {
            return Ok(None);
        }
        let Some(reply) = self
            .get(
                self.wikidata.as_ref(),
                &["w", "api.php"],
                &[
                    ("action", "wbgetclaims"),
                    ("entity", wikidata_id),
                    ("property", property),
                    ("format", "json"),
                ],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        decode(&reply).map(Some)
    }
}
