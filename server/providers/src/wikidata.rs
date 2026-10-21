//! Artist bios, images, and relation targets via Wikidata/Wikipedia/Commons.
//!
//! A Rust port of v2's Wikidata repository. Bios
//! arrive in two hops (Wikidata entity to the English Wikipedia title, then
//! the plain-text intro extract); artist images arrive in two more (the P18
//! "image" claim filename, then the Commons file URL). Relation targets
//! reuse the same claims path with a caller-chosen property. An artist's
//! picture can also be found from its MusicBrainz id alone: a statement
//! search finds the one item carrying that id (P434), and one claims call
//! reads its Commons image (P18) and Deezer artist id (P2722). A Commons
//! picture comes with its credit: the author and licence its file page
//! states, as plain text, and a link to that page.
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

mod models;

use std::collections::HashMap;
use std::time::Duration;

use crate::RequestPriority;
use crate::client::{decode, parse_base, payload, unusable_base};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

use models::{
    Claim, ClaimsAnswer, Entity, EntityData, ExtMetadata, ExtractAnswer, ImageAnswer, ImageInfo,
    ImagePage, MetadataField, SearchAnswer, WikiPage,
};

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
/// The property holding an item's MusicBrainz artist id.
pub const PROPERTY_MUSICBRAINZ_ARTIST: &str = "P434";
/// The property holding an item's Deezer artist id.
pub const PROPERTY_DEEZER_ARTIST: &str = "P2722";

/// Width Commons scales artist portraits to: big enough for any artist
/// header, without downloading multi-megabyte originals. A standard Commons
/// thumbnail width (https://www.mediawiki.org/wiki/Common_thumbnail_sizes).
/// Verified against commons.wikimedia.org/w/api.php on 2026-10-20: a 1280 px
/// thumburl for File:RadioheadO2211125 composite.jpg, tests/fixtures/wikidata/commons_imageinfo_found.json.
/// The stored portrait is still shrunk to 1200 px.
pub const COMMONS_THUMB_WIDTH: &str = "1280";
/// The file page fields a Commons credit is made from.
const COMMONS_CREDIT_FIELDS: &str = "Artist|LicenseShortName|LicenseUrl";
/// Longest author name kept, in characters: a credit is one short line.
const MAX_AUTHOR_CHARS: usize = 120;
/// Longest licence name kept, in characters.
const MAX_LICENCE_CHARS: usize = 60;

/// A Commons picture: the URL to download and the credit its file page gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonsImage {
    /// The scaled thumbnail, or the original when Commons made none.
    pub url: String,
    /// Who made it and under which licence. `None` when the answer names no
    /// usable file page.
    pub credit: Option<CommonsCredit>,
}

/// The credit a Commons file page gives, as plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonsCredit {
    /// The author, its HTML reduced to text.
    pub author: Option<String>,
    /// The licence's short name, such as `CC BY-SA 4.0`.
    pub licence: Option<String>,
    /// The licence's page; only an http or https address is kept.
    pub licence_url: Option<String>,
    /// The file's page on Commons, http or https.
    pub file_url: String,
}

// ---------------------------------------------------------------------------
// Pure helpers (ports of the v2 static helpers)
// ---------------------------------------------------------------------------

/// Pull a `Q...` id out of a Wikidata URL (the
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

/// Pull a page title out of a Wikipedia URL (the `/wiki/(.+)$` pattern),
/// percent-decoded: it goes out again as a query value, which encodes it
/// once.
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

/// The entity an answer holds for `entity_id`. A redirected id answers with its
/// target under the target's own id, so one entity under another id is that
/// target. Verified against www.wikidata.org/wiki/Special:EntityData/Q13165295.json
/// on 2026-10-20: tests/fixtures/wikidata/entity_redirect.json. No entity, or
/// several with none of them asked for, is a payload error.
fn entity_for<'a>(answer: &'a EntityData, entity_id: &str) -> Result<&'a Entity, ProviderError> {
    if let Some(entity) = answer.entities.get(entity_id) {
        return Ok(entity);
    }
    let mut others = answer.entities.values();
    match (others.next(), others.next()) {
        (Some(target), None) => Ok(target),
        _ => Err(payload(
            SOURCE,
            PayloadKind::Decode,
            "no entity for the id asked",
        )),
    }
}

/// Read the `{lang}wiki` sitelink title of the entity an answer holds for
/// `entity_id`, or `None` when that sitelink is missing.
fn sitelink_title(
    answer: &EntityData,
    entity_id: &str,
    lang: &str,
) -> Result<Option<String>, ProviderError> {
    let entity = entity_for(answer, entity_id)?;
    Ok(entity
        .sitelinks
        .get(&format!("{lang}wiki"))
        .and_then(|link| link.title.clone())
        .filter(|title| !title.is_empty()))
}

/// Read the first usable extract from a Wikipedia answer. A page that does not
/// exist, or a title no page can carry, ends the lookup as absent; the first
/// non-empty extract wins.
fn select_extract(answer: &ExtractAnswer) -> Option<String> {
    let mut pages: Vec<(&String, &WikiPage)> = answer.query.pages.iter().collect();
    pages.sort_by(|a, b| a.0.cmp(b.0));
    for (_, page) in pages {
        match page {
            WikiPage::Found {
                extract: Some(text),
                ..
            } if !text.is_empty() => return Some(text.clone()),
            WikiPage::Found { .. } => {}
            WikiPage::Missing { .. } | WikiPage::Invalid { .. } => return None,
        }
    }
    None
}

/// Read the first usable Commons file: the first page carrying image info
/// decides, and an empty URL there ends the lookup as absent.
fn select_commons_image(answer: &ImageAnswer) -> Option<CommonsImage> {
    let mut pages: Vec<(&String, &ImagePage)> = answer.query.pages.iter().collect();
    pages.sort_by(|a, b| a.0.cmp(b.0));
    let (_, page) = pages
        .into_iter()
        .find(|(_, page)| !page.imageinfo.is_empty())?;
    let info = page.imageinfo.first()?;
    // The scaled thumbnail when Commons made one, else the original.
    let url = info
        .thumburl
        .as_deref()
        .filter(|url| !url.is_empty())
        .unwrap_or(info.url.as_str());
    (!url.is_empty()).then(|| CommonsImage {
        url: url.to_owned(),
        credit: commons_credit(info),
    })
}

/// The credit of one image info row, or `None` without a usable file page.
fn commons_credit(info: &ImageInfo) -> Option<CommonsCredit> {
    let file_url = web_url(info.descriptionurl.as_deref()?)?;
    let field = |pick: fn(&ExtMetadata) -> Option<&MetadataField>| {
        info.extmetadata
            .as_ref()
            .and_then(pick)
            .and_then(MetadataField::text)
    };
    Some(CommonsCredit {
        author: field(|fields| fields.artist.as_ref())
            .and_then(|html| plain_text(html, MAX_AUTHOR_CHARS)),
        licence: field(|fields| fields.license_short_name.as_ref())
            .and_then(|html| plain_text(html, MAX_LICENCE_CHARS)),
        licence_url: field(|fields| fields.license_url.as_ref()).and_then(web_url),
        file_url,
    })
}

/// `raw` as an http or https address with a host and no credentials, else
/// `None`.
fn web_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw.trim()).ok()?;
    let usable = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    usable.then(|| url.to_string())
}

/// HTML reduced to one line of plain text: tags dropped, character
/// references decoded, whitespace collapsed, and cut to `max` characters.
/// Empty text is `None`.
fn plain_text(html: &str, max: usize) -> Option<String> {
    let mut stripped = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                stripped.push(' ');
            }
            _ if !in_tag => stripped.push(character),
            _ => {}
        }
    }
    let decoded = decode_references(&stripped);
    let words: Vec<&str> = decoded.split_whitespace().collect();
    let text: String = words.join(" ").chars().take(max).collect();
    let text = text.trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Decode the character references HTML text carries: the common named ones
/// and numeric ones. Anything else is kept as written.
fn decode_references(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest
            .char_indices()
            .take(12)
            .find(|(_, character)| *character == ';')
            .and_then(|(end, _)| reference(&rest[1..end]).map(|character| (character, end + 1)));
        match decoded {
            Some((character, used)) => {
                out.push(character);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character one reference names (`amp`, `#39`, `#x27`), if known.
fn reference(name: &str) -> Option<char> {
    let named = match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let number = name.strip_prefix('#')?;
    let code = match number.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(code).filter(|character| !character.is_control())
}

/// The error codes that read as absence: the id names no entity, or it is a
/// merged item that this endpoint does not resolve to its target.
/// Verified against www.wikidata.org/w/api.php on 2026-10-20: `no-such-entity`
/// for a well-formed unknown id (tests/fixtures/wikidata/claims_no_such_entity.json),
/// `param-invalid` for an id the id builder refuses (claims_invalid_id.json), and
/// `unresolved-redirect` for a merged item (claims_unresolved_redirect.json).
fn reads_as_absent(code: &str) -> bool {
    matches!(
        code,
        "no-such-entity" | "param-invalid" | "unresolved-redirect"
    )
}

/// The ids an artist's picture is found by, read from its Wikidata item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistPictureIds {
    /// The Commons file name (P18).
    pub commons_file: Option<String>,
    /// The Deezer artist id (P2722).
    pub deezer_artist: Option<u64>,
}

/// The one item a P434 search found. No hit, or more than one, is no match:
/// a picture is never chosen between items.
fn single_item(answer: &SearchAnswer) -> Option<String> {
    let query = &answer.query;
    let total = query.searchinfo.as_ref().and_then(|info| info.totalhits);
    if query.search.len() != 1 || total.is_some_and(|total| total != 1) {
        return None;
    }
    query
        .search
        .first()
        .map(|hit| hit.title.as_str())
        .filter(|title| is_item_id(title))
        .map(str::to_owned)
}

/// The first usable P18 and P2722 values among an item's claims.
fn picture_ids(claims: &HashMap<String, Vec<Claim>>) -> ArtistPictureIds {
    let texts = |property: &str| {
        claims
            .get(property)
            .into_iter()
            .flatten()
            .filter_map(Claim::text)
    };
    ArtistPictureIds {
        commons_file: texts(PROPERTY_IMAGE)
            .find(|name| !name.is_empty())
            .map(str::to_owned),
        deezer_artist: texts(PROPERTY_DEEZER_ARTIST).find_map(deezer_artist_id),
    }
}

/// A Deezer artist id: a plain positive integer, digits only. Anything else
/// is ignored.
#[must_use]
pub fn deezer_artist_id(raw: &str) -> Option<u64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u64>().ok().filter(|id| *id > 0)
}

/// True for a MusicBrainz id in its canonical form (lowercase hex, 8-4-4-4-12).
fn is_mbid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(at, b)| match at {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
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

    /// Resolve a Wikidata id to its Wikipedia title in one language.
    /// An id that is not an item id
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
        let answer: EntityData = decode(&reply)?;
        sitelink_title(&answer, wikidata_id, lang)
    }

    /// Fetch one plain-text intro extract.
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
        let answer: ExtractAnswer = decode(&reply)?;
        Ok(select_extract(&answer))
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
    ) -> Result<Option<CommonsImage>, ProviderError> {
        let filename = self
            .claim_string(wikidata_id, PROPERTY_IMAGE, priority)
            .await?;
        let Some(filename) = filename.filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        self.commons_image(&filename, priority).await
    }

    /// The scaled Commons URL of one file (a P18 value), with its credit.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn commons_image(
        &self,
        filename: &str,
        priority: RequestPriority,
    ) -> Result<Option<CommonsImage>, ProviderError> {
        let titles = format!("File:{filename}");
        let Some(reply) = self
            .get(
                self.commons.as_ref(),
                &["w", "api.php"],
                &[
                    ("action", "query"),
                    ("titles", titles.as_str()),
                    ("prop", "imageinfo"),
                    ("iiprop", "url|extmetadata"),
                    ("iiurlwidth", COMMONS_THUMB_WIDTH),
                    ("iiextmetadatafilter", COMMONS_CREDIT_FIELDS),
                    ("format", "json"),
                ],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let answer: ImageAnswer = decode(&reply)?;
        Ok(select_commons_image(&answer))
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
        let Some(claims) = self
            .fetch_claims(wikidata_id, Some(property), priority)
            .await?
        else {
            return Ok(None);
        };
        Ok(claims
            .get(property)
            .and_then(|statements| statements.first())
            .and_then(|claim| claim.mainsnak.as_ref())
            .and_then(|snak| snak.datavalue.as_ref())
            .and_then(|datavalue| datavalue.value.text())
            .map(str::to_owned))
    }

    /// The item whose MusicBrainz artist id (P434) is `mbid`, by exact
    /// statement search. `None` when no item or more than one has it, and
    /// for a value that is not an MBID (never sent).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn item_for_musicbrainz_artist(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        if !is_mbid(mbid) {
            return Ok(None);
        }
        let search = format!("haswbstatement:{PROPERTY_MUSICBRAINZ_ARTIST}={mbid}");
        let Some(reply) = self
            .get(
                self.wikidata.as_ref(),
                &["w", "api.php"],
                &[
                    ("action", "query"),
                    ("list", "search"),
                    ("srsearch", search.as_str()),
                    ("srlimit", "2"),
                    ("srprop", ""),
                    ("format", "json"),
                ],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let answer: SearchAnswer = decode(&reply)?;
        Ok(single_item(&answer))
    }

    /// The Commons file (P18) and Deezer artist id (P2722) of one item, in
    /// one claims call for all its properties. Absent as `fetch_claims` reads
    /// it.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn artist_picture_ids(
        &self,
        wikidata_id: &str,
        priority: RequestPriority,
    ) -> Result<Option<ArtistPictureIds>, ProviderError> {
        let claims = self.fetch_claims(wikidata_id, None, priority).await?;
        Ok(claims.as_ref().map(picture_ids))
    }

    /// Fetch one property's statements, or every property's with `None`,
    /// with 404 as absence. An id that is not
    /// an item id is never sent and reads as absence, and so does an error
    /// answer that `reads_as_absent` lists. Any other error answer is a payload
    /// error.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    async fn fetch_claims(
        &self,
        wikidata_id: &str,
        property: Option<&str>,
        priority: RequestPriority,
    ) -> Result<Option<HashMap<String, Vec<Claim>>>, ProviderError> {
        if !is_item_id(wikidata_id) {
            return Ok(None);
        }
        let mut query = vec![("action", "wbgetclaims"), ("entity", wikidata_id)];
        query.extend(property.map(|property| ("property", property)));
        query.push(("format", "json"));
        let Some(reply) = self
            .get(self.wikidata.as_ref(), &["w", "api.php"], &query, priority)
            .await?
        else {
            return Ok(None);
        };
        match decode::<ClaimsAnswer>(&reply)? {
            ClaimsAnswer::Claims { claims } => Ok(Some(claims)),
            ClaimsAnswer::Refused { error } if reads_as_absent(&error.code) => Ok(None),
            ClaimsAnswer::Refused { .. } => Err(payload(
                SOURCE,
                PayloadKind::Contract,
                "the API answered with an error",
            )),
        }
    }
}
