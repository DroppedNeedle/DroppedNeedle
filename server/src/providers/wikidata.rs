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
//! Transport rides the shared [`HttpPort`](super::client::HttpPort) GET port.
//! Retries, dedup, caching, and degradation recording stay
//! with wiring/enrichment. One intended difference: v2 answers
//! `None` for any non-200 status, while this port answers `Ok(None)` only
//! for 404 and `Err(Transport)` otherwise, so an outage cannot be mistaken
//! for "this artist has no bio".

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::client::HttpPort;

/// Provider key used in logs and degradation records.
pub const PROVIDER_NAME: &str = "wikidata";
/// Live Wikidata origin.
pub const WIKIDATA_BASE: &str = "https://www.wikidata.org";
/// Live Wikipedia origin pattern; `{lang}` selects the edition.
pub const WIKIPEDIA_PATTERN: &str = "https://{lang}.wikipedia.org";
/// Live Commons origin.
pub const COMMONS_BASE: &str = "https://commons.wikimedia.org";
/// The "image" property behind artist photos.
pub const PROPERTY_IMAGE: &str = "P18";

/// What can go wrong on a wiki fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The wire failed or the upstream answered a non-404 error status.
    Transport,
    /// The upstream answered 429; the caller backs off this long: the
    /// honored `Retry-After`, or 60s when the response gave none.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: f64,
    },
    /// The body did not decode.
    Unusable,
}

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

    /// The target entity id when the value is an entity object.
    pub fn entity_id(&self) -> Option<&str> {
        self.value
            .as_ref()
            .and_then(|value| value.get("id"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
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

/// One Commons imageinfo row on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireImageInfo {
    /// Direct file URL.
    #[serde(default)]
    pub url: Option<String>,
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
// Normalized model
// ---------------------------------------------------------------------------

/// A resolved relation target: the entity id behind one claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedEntity {
    /// Target entity id ("Q...").
    pub entity_id: String,
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
/// the `/wiki/(.+)$` pattern).
pub fn extract_wikipedia_title(url: &str) -> Option<String> {
    url.find("/wiki/").and_then(|pos| {
        let title = &url[pos + "/wiki/".len()..];
        if title.is_empty() {
            None
        } else {
            Some(title.to_owned())
        }
    })
}

/// Percent-encode one query value (RFC 3986 unreserved set, uppercase hex).
/// v2 quotes titles and filenames into the URL with `urllib`; this port
/// passes them as query pairs and encodes the same bytes.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
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
        return entry
            .imageinfo
            .first()
            .and_then(|info| info.url.clone())
            .filter(|url| !url.is_empty());
    }
    None
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Wikidata/Wikipedia/Commons client.
pub struct WikidataClient<'h, H: HttpPort> {
    /// Transport port (scripted fake in tests, real HTTP in wiring).
    pub http: &'h H,
    /// Wikidata origin; defaults to live.
    pub wikidata_base: String,
    /// Wikipedia origin pattern with `{lang}`; defaults to live.
    pub wikipedia_pattern: String,
    /// Commons origin; defaults to live.
    pub commons_base: String,
}

impl<'h, H: HttpPort> WikidataClient<'h, H> {
    /// Build a client against the live origins.
    pub fn new(http: &'h H) -> Self {
        Self {
            http,
            wikidata_base: WIKIDATA_BASE.to_owned(),
            wikipedia_pattern: WIKIPEDIA_PATTERN.to_owned(),
            commons_base: COMMONS_BASE.to_owned(),
        }
    }

    /// Build a client against scripted or mirrored origins.
    pub fn with_bases(
        http: &'h H,
        wikidata_base: &str,
        wikipedia_pattern: &str,
        commons_base: &str,
    ) -> Self {
        Self {
            http,
            wikidata_base: wikidata_base.to_owned(),
            wikipedia_pattern: wikipedia_pattern.to_owned(),
            commons_base: commons_base.to_owned(),
        }
    }

    fn wikipedia_base(&self, lang: &str) -> String {
        self.wikipedia_pattern.replace("{lang}", lang)
    }

    /// Resolve a Wikidata id to its Wikipedia title in one language (v2
    /// `_get_wikipedia_title_from_wikidata`).
    pub async fn wikipedia_title_from_wikidata(
        &self,
        wikidata_id: &str,
        lang: &str,
    ) -> Result<Option<String>, FetchError> {
        let reply = self
            .http
            .get(
                &format!(
                    "{}/wiki/Special:EntityData/{wikidata_id}.json",
                    self.wikidata_base
                ),
                &[],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: WireEntityResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        Ok(sitelink_title(&page, wikidata_id, lang))
    }

    /// Fetch one plain-text intro extract (v2 `_fetch_wikipedia_extract`).
    pub async fn wikipedia_extract(
        &self,
        page_title: &str,
        lang: &str,
    ) -> Result<Option<String>, FetchError> {
        let reply = self
            .http
            .get(
                &format!("{}/w/api.php", self.wikipedia_base(lang)),
                &[
                    ("action", "query"),
                    ("titles", page_title),
                    ("prop", "extracts"),
                    ("exintro", "1"),
                    ("explaintext", "1"),
                    ("format", "json"),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: WireQueryResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        Ok(select_extract(&page))
    }

    /// The bio extract behind a Wikidata or Wikipedia URL (v2
    /// `get_wikipedia_extract` without its cache/dedup shell, which stays
    /// with wiring). A Wikidata URL hops through the entity's sitelink;
    /// anything else is read as a Wikipedia title directly.
    pub async fn get_bio_extract(
        &self,
        wiki_url: &str,
        lang: &str,
    ) -> Result<Option<String>, FetchError> {
        let title = if let Some(id) = extract_wikidata_id(wiki_url) {
            self.wikipedia_title_from_wikidata(&id, lang).await?
        } else {
            extract_wikipedia_title(wiki_url)
        };
        let Some(title) = title else {
            return Ok(None);
        };
        self.wikipedia_extract(&title, lang).await
    }

    /// The artist image behind a Wikidata id (v2
    /// `get_artist_image_from_wikidata` without its cache/dedup shell): the
    /// first P18 claim's filename resolves through Commons image info.
    pub async fn get_artist_image(&self, wikidata_id: &str) -> Result<Option<String>, FetchError> {
        let filename = self.claim_string(wikidata_id, PROPERTY_IMAGE).await?;
        let Some(filename) = filename.filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        let titles = format!("File:{filename}");
        let reply = self
            .http
            .get(
                &format!("{}/w/api.php", self.commons_base),
                &[
                    ("action", "query"),
                    ("titles", titles.as_str()),
                    ("prop", "imageinfo"),
                    ("iiprop", "url"),
                    ("format", "json"),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: WireCommonsResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        Ok(select_commons_url(&page))
    }

    /// The first claim's string value for one property (the P18 path behind
    /// `get_artist_image`).
    pub async fn claim_string(
        &self,
        wikidata_id: &str,
        property: &str,
    ) -> Result<Option<String>, FetchError> {
        let page = self.fetch_claims(wikidata_id, property).await?;
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

    /// Entity ids targeted by one property's claims, in wire order. This
    /// powers relation reads (performer, composer, member-of, ...) over the
    /// same `wbgetclaims` path v2 uses for P18. New for v3: v2 reads only
    /// string filenames here, so there is no v2 quirk to cite beyond the
    /// shared claims envelope.
    pub async fn related_entity_ids(
        &self,
        wikidata_id: &str,
        property: &str,
    ) -> Result<Vec<RelatedEntity>, FetchError> {
        let page = self.fetch_claims(wikidata_id, property).await?;
        let Some(page) = page else {
            return Ok(Vec::new());
        };
        Ok(page
            .claims
            .get(property)
            .map(|claims| claims.as_slice())
            .unwrap_or(&[])
            .iter()
            .filter_map(|claim| claim.mainsnak.as_ref())
            .filter_map(|snak| snak.datavalue.as_ref())
            .filter_map(WireDatavalue::entity_id)
            .map(|id| RelatedEntity {
                entity_id: id.to_owned(),
            })
            .collect())
    }

    /// Fetch one property's claims, with 404 as absence.
    pub async fn fetch_claims(
        &self,
        wikidata_id: &str,
        property: &str,
    ) -> Result<Option<WireClaimsResponse>, FetchError> {
        let reply = self
            .http
            .get(
                &format!("{}/w/api.php", self.wikidata_base),
                &[
                    ("action", "wbgetclaims"),
                    ("entity", wikidata_id),
                    ("property", property),
                    ("format", "json"),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        serde_json::from_slice(&reply.body)
            .map(Some)
            .map_err(|_| FetchError::Unusable)
    }
}

/// Honor a positive `Retry-After` delay; missing, unparsable, or
/// non-positive values yield `None` so the caller falls back.
fn retry_after_secs(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
}
