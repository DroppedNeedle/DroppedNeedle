//! Newznab wire shapes, read by the client in `newznab.rs`.
//!
//! Newznab answers XML only, so these are read off the element tree
//! `xml.rs` builds rather than decoded by serde. Unknown elements and
//! attributes are ignored. Caps fall back to permissive defaults, so a
//! caps answer that reads badly never disables an indexer. A feed item
//! without an NZB enclosure is skipped; a torrent enclosure means a
//! Torznab indexer was added as Newznab, and the whole feed is dropped.
//!
//! Verified against DrunkenSlug's caps and search answers (owner's
//! account, 2026-07-01) and the recorded feeds the in-repo mock serves.
//! The `apilimits` attributes beyond `apiCurrent` and `grabCurrent`
//! (which DrunkenSlug sends) follow NZBHydra2's reader
//! (`NewznabXmlApilimits`, read 2026-10-30): `apiMax`, `grabMax`,
//! `apiOldestTime` and `grabOldestTime`, each in camel or lower case, the
//! times as RFC 2822 dates.

use std::collections::HashMap;

use crate::executor::Url;

use super::xml::{self, Element};

/// One NZB release from a Newznab indexer. Debug is hand-written: the
/// enclosure URL embeds the indexer key.
#[derive(Clone)]
pub struct UsenetRelease {
    /// Owning indexer id.
    pub indexer_id: String,
    /// Owning indexer name.
    pub indexer_name: String,
    /// Per-indexer guid (never the dedup key).
    pub guid: String,
    /// Release title.
    pub title: String,
    /// NZB enclosure URL (self-authenticating; never logged).
    pub nzb_url: String,
    /// Size bytes (`size` attr, enclosure `length` fallback).
    pub size_bytes: u64,
    /// Category ids.
    pub category_ids: Vec<i32>,
    /// Grab count bonus signal, never relied on.
    pub grabs: Option<i64>,
    /// File count bonus signal, never relied on.
    pub files: Option<i64>,
    /// Usenet post date as unix time (`usenetdate` attr, then `pubDate`).
    pub usenet_date: Option<f64>,
    /// Newznab `password` attr: positive = protected, 0/absent = none.
    /// Aggregators use negative for unknown: never rejected as protected.
    pub password: i32,
}

impl std::fmt::Debug for UsenetRelease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsenetRelease")
            .field("indexer_id", &self.indexer_id)
            .field("indexer_name", &self.indexer_name)
            .field("guid", &self.guid)
            .field("title", &self.title)
            .field("nzb_url", &"<redacted>")
            .field("size_bytes", &self.size_bytes)
            .field("category_ids", &self.category_ids)
            .field("grabs", &self.grabs)
            .field("files", &self.files)
            .field("usenet_date", &self.usenet_date)
            .field("password", &self.password)
            .finish()
    }
}

/// One Newznab subcategory.
#[derive(Debug, Clone)]
pub struct NewznabSubcategory {
    /// Category id.
    pub id: i32,
    /// Display name.
    pub name: String,
}

/// One Newznab category with its subcategories.
#[derive(Debug, Clone)]
pub struct NewznabCategory {
    /// Category id.
    pub id: i32,
    /// Display name.
    pub name: String,
    /// Subcategories.
    pub subcats: Vec<NewznabSubcategory>,
}

/// Parsed `t=caps`. Drives the query strategy; permissive defaults so a
/// caps failure doesn't disable the indexer (Lidarr/Prowlarr precedent).
#[derive(Debug, Clone)]
pub struct NewznabCaps {
    /// Server title.
    pub server_title: Option<String>,
    /// Server version.
    pub server_version: Option<String>,
    /// Default page limit.
    pub limit_default: u32,
    /// Maximum page limit.
    pub limit_max: u32,
    /// Text search available.
    pub supports_text_search: bool,
    /// Advertised text-search params.
    pub text_search_params: Vec<String>,
    /// Structured audio search available.
    pub supports_audio_search: bool,
    /// Advertised audio-search params.
    pub audio_search_params: Vec<String>,
    /// Category tree.
    pub categories: Vec<NewznabCategory>,
}

impl Default for NewznabCaps {
    fn default() -> Self {
        NewznabCaps {
            server_title: None,
            server_version: None,
            limit_default: 100,
            limit_max: 100,
            supports_text_search: true,
            text_search_params: Vec::new(),
            supports_audio_search: false,
            audio_search_params: Vec::new(),
            categories: Vec::new(),
        }
    }
}

/// `<newznab:apilimits>`: the indexer's daily API and grab counters. Any
/// of them may be absent; an indexer that names no maximum has no cap
/// DroppedNeedle can hold it to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewznabApiLimits {
    /// API hits used.
    pub api_current: Option<i64>,
    /// API hit budget.
    pub api_max: Option<i64>,
    /// Grabs used.
    pub grab_current: Option<i64>,
    /// Grab budget.
    pub grab_max: Option<i64>,
    /// When the oldest counted API hit was made, as unix seconds.
    pub api_oldest: Option<i64>,
    /// When the oldest counted grab was made, as unix seconds.
    pub grab_oldest: Option<i64>,
}

/// Parse XML, hardening on failure (as Prowlarr's `XmlCleaner` does): strip
/// illegal control chars, then escape bare ampersands and unknown entities
/// and retry. The error text is the parser's, never the body's.
pub(crate) fn safe_parse(raw: &[u8]) -> Result<Element, String> {
    let text = String::from_utf8_lossy(raw);
    let cleaned = xml::strip_illegal(&text);
    if let Ok(root) = xml::parse(&cleaned) {
        return Ok(root);
    }
    let hardened = xml::escape_bare_amps(&cleaned);
    xml::parse(&hardened).map_err(|error| error.to_string())
}

/// What an `<error code= description=>` answer says. HTTP may still be
/// 200.
pub(crate) enum ErrorAnswer {
    /// Code 100-199, or a description naming the key: the key is refused.
    Key,
    /// "Request limit reached" and its kin.
    Limit,
    /// Anything else, such as 202 for a function the indexer lacks.
    Other {
        /// Newznab error code, 0 when absent.
        code: i32,
        /// The description, or `newznab error <code>`.
        message: String,
    },
}

/// The error a document carries, when its root is `<error>`.
pub(crate) fn error_answer(root: &Element) -> Option<ErrorAnswer> {
    if xml::local(&root.tag) != "error" {
        return None;
    }
    let code = root
        .attr("code")
        .and_then(|raw| raw.trim().parse::<i32>().ok())
        .unwrap_or(0);
    let description = root.attr("description").unwrap_or("").to_owned();
    let low = description.to_ascii_lowercase();
    if (100..=199).contains(&code) || low.contains("apikey") || low.contains("api key") {
        return Some(ErrorAnswer::Key);
    }
    if low.contains("limit reached") {
        return Some(ErrorAnswer::Limit);
    }
    Some(ErrorAnswer::Other {
        code,
        message: if description.is_empty() {
            format!("newznab error {code}")
        } else {
            description
        },
    })
}

pub(crate) fn parse_caps(root: &Element) -> NewznabCaps {
    let server = root.child("server");
    let limits = root.child("limits");
    let searching = root.child("searching");
    let text_el = searching.and_then(|el| el.child("search"));
    // `<audio-search>` first, `<music-search>` fallback: Prowlarr-as-server
    // emits both, real indexers vary.
    let audio_el = searching.and_then(|el| {
        el.child("audio-search")
            .or_else(|| el.child("music-search"))
    });
    let mut categories = Vec::new();
    if let Some(cats_el) = root.child("categories") {
        for cat in cats_el.children_named("category") {
            let Some(id) = cat
                .attr("id")
                .and_then(|raw| raw.trim().parse::<i32>().ok())
            else {
                continue;
            };
            let subcats = cat
                .children_named("subcat")
                .filter_map(|sub| {
                    sub.attr("id")
                        .and_then(|raw| raw.trim().parse::<i32>().ok())
                        .map(|sid| NewznabSubcategory {
                            id: sid,
                            name: sub.attr("name").unwrap_or("").to_owned(),
                        })
                })
                .collect();
            categories.push(NewznabCategory {
                id,
                name: cat.attr("name").unwrap_or("").to_owned(),
                subcats,
            });
        }
    }
    NewznabCaps {
        server_title: server.and_then(|el| el.attr("title")).map(str::to_owned),
        server_version: server.and_then(|el| el.attr("version")).map(str::to_owned),
        limit_default: limits
            .and_then(|el| el.attr("default"))
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(100),
        limit_max: limits
            .and_then(|el| el.attr("max"))
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(100),
        supports_text_search: available(text_el),
        text_search_params: params_of(text_el),
        supports_audio_search: available(audio_el),
        audio_search_params: params_of(audio_el),
        categories,
    }
}

fn available(element: Option<&Element>) -> bool {
    element.is_some_and(|el| {
        el.attr("available")
            .unwrap_or("")
            .eq_ignore_ascii_case("yes")
    })
}

fn params_of(element: Option<&Element>) -> Vec<String> {
    element
        .and_then(|el| el.attr("supportedParams"))
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The releases of a search feed. `request_url` is what relative
/// enclosure URLs are read against.
pub(crate) fn parse_items(
    root: &Element,
    indexer_id: &str,
    indexer_name: &str,
    request_url: &Url,
) -> Vec<UsenetRelease> {
    let mut releases = Vec::new();
    for item in root.descendants("item") {
        match parse_item(item, indexer_id, indexer_name, request_url) {
            Ok(Some(release)) => releases.push(release),
            Ok(None) => {}
            Err(ItemFault::Torznab) => {
                // Torrent/magnet enclosures: a Torznab indexer added as
                // Generic Newznab. Bail the whole feed.
                tracing::warn!(
                    indexer = indexer_name,
                    "newznab indexer returned torrent enclosures; is a Torznab indexer added as Generic Newznab?"
                );
                return Vec::new();
            }
        }
    }
    releases
}

/// Why a feed item stops the parse: only a Torznab enclosure does; a bad
/// item is skipped as `Ok(None)` so it cannot sink the feed.
enum ItemFault {
    Torznab,
}

fn parse_item(
    item: &Element,
    indexer_id: &str,
    indexer_name: &str,
    request_url: &Url,
) -> Result<Option<UsenetRelease>, ItemFault> {
    let mut nzb_url: Option<String> = None;
    let mut enclosure_size: u64 = 0;
    for enclosure in item.children_named("enclosure") {
        let kind = enclosure.attr("type").unwrap_or("").to_ascii_lowercase();
        let url = enclosure.attr("url").unwrap_or("").to_owned();
        if kind.contains("torrent") || url.starts_with("magnet:") {
            return Err(ItemFault::Torznab);
        }
        // The NZB enclosure is MIME-enforced; a no-type enclosure is accepted.
        if kind.contains("x-nzb") || kind.is_empty() {
            nzb_url = Some(url);
            enclosure_size = enclosure
                .attr("length")
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .unwrap_or(0);
            if kind.contains("x-nzb") {
                break;
            }
        }
    }
    // Not an NZB item, or an enclosure that is no address; `<link>` is
    // intentionally ignored.
    let Some(nzb_url) = nzb_url.and_then(|url| enclosure_address(request_url, &url)) else {
        return Ok(None);
    };
    let attrs = attr_map(item);
    let size = first_of(attrs.get("size"))
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(enclosure_size);
    let category_ids: Vec<i32> = attrs
        .get("category")
        .map(|values| {
            values
                .iter()
                .filter_map(|raw| raw.trim().parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default();
    let date_text = first_of(attrs.get("usenetdate"))
        .map(str::to_owned)
        .or_else(|| item.child_text("pubDate").map(str::to_owned));
    let usenet_date = date_text.as_deref().and_then(parse_rfc2822_date);
    let title = item
        .child_text("title")
        .unwrap_or("Unknown")
        .trim()
        .to_owned();
    Ok(Some(UsenetRelease {
        indexer_id: indexer_id.to_owned(),
        indexer_name: indexer_name.to_owned(),
        guid: item.child_text("guid").unwrap_or("").to_owned(),
        title,
        nzb_url,
        size_bytes: size,
        category_ids,
        grabs: first_of(attrs.get("grabs")).and_then(|raw| raw.trim().parse::<i64>().ok()),
        files: first_of(attrs.get("files")).and_then(|raw| raw.trim().parse::<i64>().ok()),
        usenet_date,
        password: first_of(attrs.get("password"))
            .and_then(|raw| raw.trim().parse::<i32>().ok())
            .unwrap_or(0),
    }))
}

/// An enclosure as an http or https address. An absolute one is kept as
/// the indexer wrote it (some put the query after `.nzb&` with no `?`);
/// a relative one is read against the request.
fn enclosure_address(request_url: &Url, raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let absolute = Url::parse(raw).is_ok();
    let parsed = request_url.join(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    Some(if absolute {
        raw.to_owned()
    } else {
        parsed.to_string()
    })
}

/// `{name.lower(): [values]}` from the item's `<newznab:attr>` elements
/// (`category` repeats). Attr-name match is case-insensitive.
fn attr_map(item: &Element) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for attr in item.children_named("attr") {
        let name = attr.attr("name").unwrap_or("").to_ascii_lowercase();
        if let (false, Some(value)) = (name.is_empty(), attr.attr("value")) {
            out.entry(name).or_default().push(value.to_owned());
        }
    }
    out
}

fn first_of(values: Option<&Vec<String>>) -> Option<&str> {
    values.and_then(|values| values.first().map(String::as_str))
}

/// The counters a caps or search answer carries, anywhere under its root.
pub(crate) fn parse_apilimits(root: &Element) -> Option<NewznabApiLimits> {
    let element = if xml::local(&root.tag) == "apilimits" {
        root
    } else {
        root.descendants("apilimits").into_iter().next()?
    };
    let number =
        |name: &str| attr_any_case(element, name).and_then(|raw| raw.trim().parse::<i64>().ok());
    let time = |name: &str| attr_any_case(element, name).and_then(rfc2822_seconds);
    Some(NewznabApiLimits {
        api_current: number("apiCurrent"),
        api_max: number("apiMax"),
        grab_current: number("grabCurrent"),
        grab_max: number("grabMax"),
        api_oldest: time("apiOldestTime"),
        grab_oldest: time("grabOldestTime"),
    })
}

fn attr_any_case<'a>(element: &'a Element, name: &str) -> Option<&'a str> {
    element
        .attrs
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// RFC 2822 (`pubDate`, `usenetdate`) to unix time, as the release
/// dates are kept.
#[must_use]
pub fn parse_rfc2822_date(value: &str) -> Option<f64> {
    rfc2822_seconds(value).map(|seconds| seconds as f64)
}

/// RFC 2822 to unix seconds. Weekday optional; numeric zones plus
/// GMT/UT/single military letters; `None` on garbage.
fn rfc2822_seconds(value: &str) -> Option<i64> {
    let text = value.trim();
    let without_weekday = text
        .split_once(',')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(text);
    let mut parts = without_weekday.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month = month_number(parts.next()?)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let clock = parts.next()?;
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next().unwrap_or("0").parse().ok()?;
    let zone = parts.next().unwrap_or("GMT");
    let offset_seconds = zone_offset_seconds(zone)?;
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds)
}

fn month_number(name: &str) -> Option<i64> {
    match name.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn zone_offset_seconds(zone: &str) -> Option<i64> {
    let zone = zone.trim();
    if zone.eq_ignore_ascii_case("gmt")
        || zone.eq_ignore_ascii_case("ut")
        || zone.eq_ignore_ascii_case("utc")
        || zone.eq_ignore_ascii_case("z")
    {
        return Some(0);
    }
    if zone.len() == 1 && zone.chars().all(|ch| ch.is_ascii_alphabetic()) {
        // Single military letter: Z already handled; A-I = +1..+9,
        // K-M = +10..+12, N-Y = -1..-12 (J/local is unknown).
        let letter = zone.to_ascii_uppercase().chars().next()?;
        if letter == 'J' {
            return None;
        }
        let position = i64::from(letter as u8 - b'A');
        return Some(if letter < 'N' {
            (position + 1) * 3_600
        } else {
            -(position + 1 - 13) * 3_600
        });
    }
    if zone.len() == 5 && (zone.starts_with('+') || zone.starts_with('-')) {
        let hours: i64 = zone.get(1..3)?.parse().ok()?;
        let minutes: i64 = zone.get(3..5)?.parse().ok()?;
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        return Some(sign * (hours * 3_600 + minutes * 60));
    }
    None
}

/// Howard Hinnant's days-from-civil; `None` for out-of-range input.
/// Shared with the Prowlarr ISO-8601 parser (never a second copy).
#[must_use]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year.rem_euclid(400);
    let month_prime = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}
