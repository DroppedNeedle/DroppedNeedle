//! Newznab indexer search: per-indexer HTTP + XML, fan-out, ladder.
//!
//! [`NewznabClient`] is the raw XML wrapper around one indexer (no fan-out
//! logic), ported from v2 `newznab_client.py`: XML-only, `extended=1`
//! always, `<error>` checked on caps and search alike, auth via an
//! `apikey` query param that is never logged. [`NewznabIndexer`] fans one
//! logical search across the configured indexers (v2 `newznab_indexer.py`):
//! caps-gated query strategy, the 202 `t=music` → `t=search` fallback, the
//! punctuation-normalized retry ladder (#259), rate-limit backoff, search
//! caching, and cross-indexer dedup by the (title, size) identity.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use reqwest::Client;
use thiserror::Error;

use super::xml::{self, Element};

// ---------------------------------------------------------------------------
// Boundary types (v2 `repositories/protocols/indexer.py` + `download_identity.py`).
// ---------------------------------------------------------------------------

/// One NZB release from a Newznab indexer (release-granular). Debug is
/// hand-written: the enclosure URL embeds the indexer key.
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
    /// Usenet post date as unix time (`usenetdate` attr → `pubDate`).
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

/// One tagged search hit. The coordinator's union also spans soulseek and
/// plugin hits; this module only ever produces `usenet` ones.
#[derive(Debug, Clone)]
pub struct IndexerResult {
    /// Always `usenet` here.
    pub source: String,
    /// The release.
    pub usenet: UsenetRelease,
}

/// Cross-indexer release identity: normalised title + size bucketed to the
/// MB, joined by the unit separator (v2 `usenet_identity`). Trivial
/// spacing/case differences dedup together; a byte or two of size jitter
/// doesn't split the identity.
pub fn usenet_identity(title: &str, size_bytes: u64) -> String {
    let mut norm = String::with_capacity(title.len());
    let mut gap = false;
    for word in title.split_whitespace() {
        if gap {
            norm.push(' ');
        }
        gap = true;
        norm.push_str(&word.to_ascii_lowercase());
    }
    format!("{norm}\u{1f}{}", size_bytes / (1024 * 1024))
}

// ---------------------------------------------------------------------------
// Caps models (v2 `newznab_models.py`).
// ---------------------------------------------------------------------------

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

impl NewznabCaps {
    /// Every audio category id advertised (the 3000 parent + subcats),
    /// e.g. DrunkenSlug → 3000,3010,3020,3030,3040,3060,3999.
    pub fn audio_category_ids(&self) -> Vec<i32> {
        let mut out = Vec::new();
        for cat in &self.categories {
            if (3000..4000).contains(&cat.id) {
                out.push(cat.id);
                out.extend(cat.subcats.iter().map(|sub| sub.id));
            }
        }
        out
    }

    /// The Audio/Other id read from caps: 3050 on standard Prowlarr, 3999
    /// on nZEDb-derived indexers like DrunkenSlug. Never hardcoded.
    pub fn other_audio_category_id(&self) -> Option<i32> {
        for cat in &self.categories {
            if cat.id == 3000 || cat.name.eq_ignore_ascii_case("audio") {
                for sub in &cat.subcats {
                    if sub.name.eq_ignore_ascii_case("other") {
                        return Some(sub.id);
                    }
                }
            }
        }
        None
    }
}

/// `<newznab:apilimits>` usage counters. Parsed and returned but not
/// consumed for proactive backoff (`*_max` are often absent); the
/// enforced mechanism is the 429 / "limit reached" backoff.
#[derive(Debug, Clone, Default)]
pub struct NewznabApiLimits {
    /// API hits used.
    pub api_current: Option<i64>,
    /// API hit budget.
    pub api_max: Option<i64>,
    /// Grabs used.
    pub grab_current: Option<i64>,
    /// Grab budget.
    pub grab_max: Option<i64>,
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// Newznab failure. Auth and rate-limit variants let the indexer layer
/// back off or stand down without failing the fan-out.
#[derive(Debug, Clone, Error)]
pub enum NewznabError {
    /// The request never got an answer (the exception class name rides
    /// along because timeouts stringify to `""`, v2 #389).
    #[error("newznab request failed: {0}")]
    Transport(String),
    /// Indexer answered HTTP 4xx/5xx.
    #[error("newznab returned HTTP {status}")]
    Http {
        /// Status code.
        status: u16,
        /// Body snippet.
        snippet: String,
        /// Same as `status`, for parity with v2's `code`.
        code: u16,
    },
    /// `<error code= description=>` with a non-auth, non-limit code.
    #[error("{message}")]
    Api {
        /// Description text.
        message: String,
        /// Newznab error code.
        code: i32,
    },
    /// Code 100-199 or an apikey-shaped description.
    #[error("{message}")]
    Auth {
        /// Description text.
        message: String,
        /// Newznab error code.
        code: i32,
    },
    /// HTTP 429 or a "request limit reached" description.
    #[error("{message}")]
    RateLimited {
        /// Description text.
        message: String,
        /// `Retry-After` seconds, when the header carried an integer.
        retry_after: Option<f64>,
    },
    /// The body survived hardening but still isn't XML.
    #[error("newznab returned unparseable XML: {0}")]
    Unparseable(String),
}

impl NewznabError {
    /// Newznab error code, when the failure carries one.
    pub fn code(&self) -> Option<i32> {
        match self {
            NewznabError::Api { code, .. } | NewznabError::Auth { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// `Retry-After` seconds, when rate-limited with a header value.
    pub fn retry_after(&self) -> Option<f64> {
        match self {
            NewznabError::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Query normalization (v2 `normalize_newznab_query`, #259).
// ---------------------------------------------------------------------------

/// Characters a scene release name never carries. A canonical MusicBrainz
/// title like "Honestly, Nevermind" must also be searchable without them:
/// live-verified NZBGeek behavior returns 0 releases for the comma query
/// and 3 lossless for the stripped one. Survivors are removed (not
/// spaced), so "Man's" becomes "Mans" exactly the way scene names drop
/// the apostrophe.
const QUERY_STRIP: [char; 8] = [',', '\'', '"', '?', '!', ';', ':', '&'];

/// Fold one typographic quote to ASCII (v2 `_TYPOGRAPHIC_QUOTES`).
fn fold_quote(ch: char) -> char {
    match ch {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{2035}' | '`'
        | '\u{00B4}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{00AB}' | '\u{00BB}' | '\u{201E}' | '\u{201F}' => '"',
        _ => ch,
    }
}

/// Punctuation-normalized free-text query rung. Outbound-query-only: the
/// canonical title still flows untouched to the scorer/import and to the
/// structured `t=music` params. Idempotent, no-op on clean input.
pub fn normalize_newznab_query(query: &str) -> String {
    let folded: String = query.chars().map(fold_quote).collect();
    let stripped: String = folded
        .chars()
        .filter(|ch| !QUERY_STRIP.contains(ch))
        .collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Date parsing (no date crate needed).
// ---------------------------------------------------------------------------

/// RFC-2822 (`pubDate`/`usenetdate`) → unix time. Weekday optional;
/// numeric zones + GMT/UT/single military letters; `None` on garbage.
pub fn parse_rfc2822_date(value: &str) -> Option<f64> {
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
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds) as f64)
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
        let hours: i64 = zone[1..3].parse().ok()?;
        let minutes: i64 = zone[3..5].parse().ok()?;
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        return Some(sign * (hours * 3_600 + minutes * 60));
    }
    None
}

/// Howard Hinnant's days-from-civil; `None` for out-of-range input.
/// Shared with the Prowlarr ISO-8601 parser (never a second copy).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
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

// ---------------------------------------------------------------------------
// Raw client.
// ---------------------------------------------------------------------------

/// Raw httpx-style wrapper around one Newznab indexer. The HTTP client is
/// injected; auth is an `apikey` query param, never logged.
pub struct NewznabClient {
    http: Client,
    base_url: String,
    api_key: String,
    indexer_id: String,
    indexer_name: String,
}

impl NewznabClient {
    /// Build over an injected client. `base_url` is the full API path the
    /// user pasted (e.g. `https://idx/api`), kept verbatim.
    pub fn new(
        http: Client,
        base_url: &str,
        api_key: &str,
        indexer_id: &str,
        indexer_name: &str,
    ) -> Self {
        NewznabClient {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            indexer_id: indexer_id.to_owned(),
            indexer_name: if indexer_name.is_empty() {
                base_url.to_owned()
            } else {
                indexer_name.to_owned()
            },
        }
    }

    /// Indexer id this client searches.
    pub fn indexer_id(&self) -> &str {
        &self.indexer_id
    }

    /// Indexer display name.
    pub fn indexer_name(&self) -> &str {
        &self.indexer_name
    }

    /// Parsed `t=caps`.
    pub async fn caps(&self, timeout: Duration) -> Result<NewznabCaps, NewznabError> {
        let root = self.get(&[("t", "caps")], timeout).await?;
        check_error(&root)?;
        Ok(parse_caps(&root))
    }

    /// Free-text `t=search`: the always-available path (DrunkenSlug only
    /// supports this; `t=music` returns error 202).
    pub async fn search(
        &self,
        query: &str,
        categories: &[i32],
        offset: u32,
        limit: u32,
        timeout: Duration,
    ) -> Result<(Vec<UsenetRelease>, Option<NewznabApiLimits>), NewznabError> {
        let offset_text = offset.to_string();
        let limit_text = limit.to_string();
        let mut params: Vec<(String, String)> = vec![
            ("t".to_owned(), "search".to_owned()),
            ("q".to_owned(), query.to_owned()),
            ("extended".to_owned(), "1".to_owned()),
            ("offset".to_owned(), offset_text),
            ("limit".to_owned(), limit_text),
        ];
        if !categories.is_empty() {
            let cats = categories
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            params.push(("cat".to_owned(), cats));
        }
        self.search_request(&params, timeout).await
    }

    /// Structured `t=music`, used only when caps advertises audio-search
    /// with artist/album params. The caller falls back to `search` on a 202.
    #[allow(clippy::too_many_arguments)]
    pub async fn music_search(
        &self,
        artist: &str,
        album: &str,
        categories: &[i32],
        year: Option<i32>,
        offset: u32,
        limit: u32,
        timeout: Duration,
    ) -> Result<(Vec<UsenetRelease>, Option<NewznabApiLimits>), NewznabError> {
        let offset_text = offset.to_string();
        let limit_text = limit.to_string();
        let mut params: Vec<(String, String)> = vec![
            ("t".to_owned(), "music".to_owned()),
            ("artist".to_owned(), artist.to_owned()),
            ("album".to_owned(), album.to_owned()),
            ("extended".to_owned(), "1".to_owned()),
            ("offset".to_owned(), offset_text),
            ("limit".to_owned(), limit_text),
        ];
        if let Some(year) = year {
            params.push(("year".to_owned(), year.to_string()));
        }
        if !categories.is_empty() {
            let cats = categories
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            params.push(("cat".to_owned(), cats));
        }
        self.search_request(&params, timeout).await
    }

    async fn search_request(
        &self,
        params: &[(String, String)],
        timeout: Duration,
    ) -> Result<(Vec<UsenetRelease>, Option<NewznabApiLimits>), NewznabError> {
        let refs: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let root = self.get(&refs, timeout).await?;
        check_error(&root)?;
        let kind = params
            .iter()
            .find(|(key, _)| key == "t")
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        let request_url = format!("{}?{kind}", self.base_url);
        let releases = parse_items(&root, &self.indexer_id, &self.indexer_name, &request_url);
        Ok((releases, parse_apilimits(&root)))
    }

    async fn get(
        &self,
        params: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Element, NewznabError> {
        let mut merged: Vec<(&str, &str)> = params.to_vec();
        merged.push(("apikey", &self.api_key));
        let response = self
            .http
            .get(&self.base_url)
            .query(&merged)
            .timeout(timeout)
            .send()
            .await
            .map_err(|err| NewznabError::Transport(transport_detail(&err)))?;
        if response.status().as_u16() == 429 {
            return Err(NewznabError::RateLimited {
                message: "newznab: rate limited".to_owned(),
                retry_after: retry_after(&response),
            });
        }
        if response.status().as_u16() >= 400 {
            let status = response.status().as_u16();
            let snippet = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            return Err(NewznabError::Http {
                status,
                snippet,
                code: status,
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| NewznabError::Transport(transport_detail(&err)))?;
        safe_parse(&bytes)
    }
}

/// Transport detail that never collapses to `""`: timeouts stringify
/// empty, so the error type name stands in (v2 #389).
fn transport_detail(err: &reqwest::Error) -> String {
    let text = err.to_string();
    if text.trim().is_empty() {
        if err.is_timeout() {
            "timeout".to_owned()
        } else if err.is_connect() {
            "connect error".to_owned()
        } else if err.is_request() {
            "request error".to_owned()
        } else if err.is_body() {
            "body error".to_owned()
        } else {
            "transport error".to_owned()
        }
    } else {
        text
    }
}

fn retry_after(response: &reqwest::Response) -> Option<f64> {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.trim().parse::<f64>().ok())
}

/// Parse XML, hardening on failure (v2 `_safe_fromstring`, Prowlarr
/// `XmlCleaner`): strip illegal control chars, then escape bare
/// ampersands/non-predefined entities and retry.
fn safe_parse(raw: &[u8]) -> Result<Element, NewznabError> {
    let text = String::from_utf8_lossy(raw);
    let cleaned = xml::strip_illegal(&text);
    if let Ok(root) = xml::parse(&cleaned) {
        return Ok(root);
    }
    let hardened = xml::escape_bare_amps(&cleaned);
    xml::parse(&hardened)
        .map_err(|err| NewznabError::Unparseable(format!("{err} (offset {})", err.offset)))
}

/// `<error code= description=>`: HTTP may still be 200. 100-199 / apikey
/// ⇒ auth; "request limit reached" ⇒ rate limit; else generic.
fn check_error(root: &Element) -> Result<(), NewznabError> {
    if xml::local(&root.tag) != "error" {
        return Ok(());
    }
    let code = root
        .attr("code")
        .and_then(|raw| raw.trim().parse::<i32>().ok())
        .unwrap_or(0);
    let description = root.attr("description").unwrap_or("").to_owned();
    let low = description.to_ascii_lowercase();
    if (100..=199).contains(&code) || low.contains("apikey") || low.contains("api key") {
        return Err(NewznabError::Auth {
            message: if description.is_empty() {
                "newznab auth failure".to_owned()
            } else {
                description
            },
            code,
        });
    }
    if low.contains("request limit reached") || low.contains("limit reached") {
        return Err(NewznabError::RateLimited {
            message: if description.is_empty() {
                "newznab request limit reached".to_owned()
            } else {
                description
            },
            retry_after: None,
        });
    }
    Err(NewznabError::Api {
        message: if description.is_empty() {
            format!("newznab error {code}")
        } else {
            description
        },
        code,
    })
}

fn parse_caps(root: &Element) -> NewznabCaps {
    let server = root.child("server");
    let limits = root.child("limits");
    let searching = root.child("searching");
    let text_el = searching.and_then(|el| el.child("search"));
    // `<audio-search>` first, `<music-search>` fallback: Prowlarr-as-server
    // emits both, real indexers vary (v2 robustness finding).
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

fn parse_items(
    root: &Element,
    indexer_id: &str,
    indexer_name: &str,
    request_url: &str,
) -> Vec<UsenetRelease> {
    let mut releases = Vec::new();
    for item in root.descendants("item") {
        match parse_item(item, indexer_id, indexer_name, request_url) {
            Ok(Some(release)) => releases.push(release),
            Ok(None) => {}
            Err(ItemFault::Torznab) => {
                // Torrent/magnet enclosures: a Torznab indexer added as
                // Generic Newznab. Bail the whole feed (v2 `_TorznabFeed`).
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
    request_url: &str,
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
    let Some(nzb_url) = nzb_url.filter(|url| !url.is_empty()) else {
        // Not an NZB item; `<link>` is intentionally ignored (v2).
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
        nzb_url: join_url(request_url, &nzb_url),
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

fn parse_apilimits(root: &Element) -> Option<NewznabApiLimits> {
    let element = root
        .child("channel")
        .and_then(|channel| channel.child("apilimits"))
        .or_else(|| root.child("apilimits"))?;
    Some(NewznabApiLimits {
        api_current: element
            .attr("apiCurrent")
            .and_then(|raw| raw.trim().parse::<i64>().ok()),
        api_max: element
            .attr("apiMax")
            .and_then(|raw| raw.trim().parse::<i64>().ok()),
        grab_current: element
            .attr("grabCurrent")
            .and_then(|raw| raw.trim().parse::<i64>().ok()),
        grab_max: element
            .attr("grabMax")
            .and_then(|raw| raw.trim().parse::<i64>().ok()),
    })
}

/// Minimal `urljoin` for enclosure URLs: absolute URLs pass through,
/// root-relative ones join the origin, the rest join the API directory.
fn join_url(request_url: &str, nzb_url: &str) -> String {
    if nzb_url.contains("://") || nzb_url.starts_with("//") {
        return nzb_url.to_owned();
    }
    let (origin, _) = match request_url.split_once("://") {
        Some((scheme, rest)) => {
            let origin_end = rest
                .find('/')
                .map(|index| index + scheme.len() + 3)
                .unwrap_or(request_url.len());
            (&request_url[..origin_end], &request_url[origin_end..])
        }
        None => ("", request_url),
    };
    if nzb_url.starts_with('/') {
        return format!("{origin}{nzb_url}");
    }
    let base_path = request_url.split('?').next().unwrap_or(request_url);
    let base_dir = base_path
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or(base_path);
    format!("{base_dir}/{nzb_url}")
}

// ---------------------------------------------------------------------------
// Fan-out indexer (v2 `NewznabIndexer`).
// ---------------------------------------------------------------------------

/// Newznab "No such function": fall back `t=music` → `t=search`.
const UNKNOWN_FUNCTION: i32 = 202;
/// Cap on cached search entries before opportunistic eviction.
const SEARCH_CACHE_CAP: usize = 256;
/// `(indexer id, query)` → expiry + pooled releases.
pub(crate) type SearchCache = Mutex<HashMap<(String, String), (Instant, Vec<UsenetRelease>)>>;

/// One configured indexer paired with its HTTP client. A runtime
/// holder the wiring builds from settings; never serialised.
pub struct NewznabIndexerEntry {
    /// Raw client for this indexer.
    pub client: NewznabClient,
    /// Indexer id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Categories to search.
    pub categories: Vec<i32>,
    /// Disabled indexers never search.
    pub enabled: bool,
    /// Lower wins dedup ties.
    pub priority: u32,
    /// Page limit requested (clamped to caps).
    pub limit: u32,
}

/// Health answer (v2 `ServiceStatus` shape).
#[derive(Debug, Clone)]
pub struct IndexerHealth {
    /// `ok` or `error`.
    pub status: String,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
}

/// Fans one logical search across the N configured Newznab indexers.
///
/// Per indexer: caps-gated query strategy (structured `t=music` only when
/// caps advertises audio-search with artist/album params, else free-text
/// `t=search`), per-call timeout, rate-limit backoff, and a short-TTL
/// search cache so fan-out + failover + auto-retry don't each re-hit the
/// daily API budget. Results pool and dedup by the normalised (title,
/// size) identity, the higher-priority indexer's copy winning. One
/// indexer erroring never fails the fan-out.
pub struct NewznabIndexer {
    entries: Vec<NewznabIndexerEntry>,
    search_cache_ttl: Duration,
    caps_cache_ttl: Duration,
    rate_limit_backoff: Duration,
    timeout: Duration,
    caps_cache: Mutex<HashMap<String, (Instant, NewznabCaps)>>,
    search_cache: SearchCache,
    backoff_until: Mutex<HashMap<String, Instant>>,
}

impl NewznabIndexer {
    /// Build over the configured entries (sorted: higher priority first,
    /// so dedup keeps the preferred copy).
    pub fn new(
        mut entries: Vec<NewznabIndexerEntry>,
        search_cache_ttl: Duration,
        caps_cache_ttl: Duration,
        rate_limit_backoff: Duration,
        per_indexer_timeout: Duration,
    ) -> Self {
        entries.sort_by_key(|entry| entry.priority);
        NewznabIndexer {
            entries,
            search_cache_ttl,
            caps_cache_ttl,
            rate_limit_backoff,
            timeout: per_indexer_timeout,
            caps_cache: Mutex::new(HashMap::new()),
            search_cache: Mutex::new(HashMap::new()),
            backoff_until: Mutex::new(HashMap::new()),
        }
    }

    /// Indexer name for routing.
    pub fn indexer_name(&self) -> &'static str {
        "usenet"
    }

    /// Configured means at least one entry is enabled.
    pub fn is_configured(&self) -> bool {
        self.entries.iter().any(|entry| entry.enabled)
    }

    /// Health never raises: unreachable indexers are skipped with a warning.
    pub async fn health_check(&self) -> IndexerHealth {
        let enabled: Vec<&NewznabIndexerEntry> =
            self.entries.iter().filter(|entry| entry.enabled).collect();
        if enabled.is_empty() {
            return IndexerHealth {
                status: "error".to_owned(),
                version: None,
                message: "No indexers configured".to_owned(),
            };
        }
        let mut reachable = 0;
        let mut version: Option<String> = None;
        for entry in &enabled {
            match entry.client.caps(self.timeout).await {
                Ok(caps) => {
                    reachable += 1;
                    if version.is_none() {
                        version = caps.server_version;
                    }
                }
                Err(err) => tracing::warn!(
                    indexer = entry.name.as_str(),
                    error = err.to_string().as_str(),
                    "newznab health: indexer unreachable"
                ),
            }
        }
        if reachable > 0 {
            IndexerHealth {
                status: "ok".to_owned(),
                version,
                message: format!("{reachable}/{} indexer(s) reachable", enabled.len()),
            }
        } else {
            IndexerHealth {
                status: "error".to_owned(),
                version: None,
                message: "No indexer reachable".to_owned(),
            }
        }
    }

    /// Album search: `artist album` free text, structured `t=music` when
    /// caps allows it.
    pub async fn search_album(
        &self,
        artist_name: &str,
        album_title: &str,
        year: Option<i32>,
        timeout: Duration,
    ) -> Vec<IndexerResult> {
        let query = format!("{artist_name} {album_title}");
        let query = query.trim().to_owned();
        self.search_with_ladder(&query, artist_name, Some(album_title), year, timeout)
            .await
            .into_iter()
            .map(|usenet| IndexerResult {
                source: "usenet".to_owned(),
                usenet,
            })
            .collect()
    }

    /// Track search: free-text `artist track` only. There is no reliable
    /// single-track Usenet search; the orchestrator resolves a track to
    /// the album, and `album=None` keeps the structured path off here.
    pub async fn search_track(
        &self,
        artist_name: &str,
        track_title: &str,
        timeout: Duration,
    ) -> Vec<IndexerResult> {
        let query = format!("{artist_name} {track_title}");
        let query = query.trim().to_owned();
        self.search_with_ladder(&query, artist_name, None, None, timeout)
            .await
            .into_iter()
            .map(|usenet| IndexerResult {
                source: "usenet".to_owned(),
                usenet,
            })
            .collect()
    }

    /// Canonical query first; on a genuine empty, one normalized retry
    /// (#259). The retry fires only when every enabled indexer actually
    /// answered (at least one successful empty, none rate-limited,
    /// auth-failed, or errored): an indexer that sat out in backoff may
    /// hold the release, so an empty pool with a silent indexer is not
    /// proof the query is dead. The retry rung forces free-text `t=search`
    /// even when caps advertises `t=music`: the structured params stay
    /// canonical by decision, so re-sending them would be a byte-identical
    /// repeat; the normalized free text is the new signal.
    async fn search_with_ladder(
        &self,
        query: &str,
        artist: &str,
        album: Option<&str>,
        year: Option<i32>,
        timeout: Duration,
    ) -> Vec<UsenetRelease> {
        let (releases, clean) = self
            .fan_out(query, artist, album, year, timeout, false)
            .await;
        if !releases.is_empty() {
            return releases;
        }
        let normalized = normalize_newznab_query(query);
        if normalized == query || !clean {
            return releases;
        }
        tracing::info!(
            query = query,
            normalized_query = normalized.as_str(),
            "newznab.query_normalized_retry"
        );
        let (releases, _) = self
            .fan_out(&normalized, artist, album, year, timeout, true)
            .await;
        releases
    }

    /// Fan one logical search out across the enabled indexers. Returns
    /// `(pooled, clean)` where `clean` holds only when every enabled
    /// indexer returned a successful response (even an empty one).
    async fn fan_out(
        &self,
        query: &str,
        artist: &str,
        album: Option<&str>,
        year: Option<i32>,
        timeout: Duration,
        text_only: bool,
    ) -> (Vec<UsenetRelease>, bool) {
        let enabled: Vec<&NewznabIndexerEntry> =
            self.entries.iter().filter(|entry| entry.enabled).collect();
        if enabled.is_empty() {
            return (Vec::new(), false);
        }
        let per_call = timeout.min(self.timeout);
        let results =
            join_all(enabled.iter().map(|entry| {
                self.search_one(entry, query, artist, album, year, per_call, text_only)
            }))
            .await;
        let mut pooled = Vec::new();
        let mut clean = true;
        for (entry, result) in enabled.iter().zip(results) {
            match result {
                Ok((releases, ok)) => {
                    if !ok {
                        clean = false;
                    }
                    pooled.extend(releases);
                }
                Err(err) => {
                    tracing::warn!(
                        indexer = entry.name.as_str(),
                        error = err.to_string().as_str(),
                        "newznab indexer search failed"
                    );
                    clean = false;
                }
            }
        }
        (dedup_releases(pooled), clean)
    }

    /// One indexer's search. The bool reports a successful indexer answer
    /// (results or a genuine empty); backoff skips, rate limits, and auth
    /// failures report false so the ladder never retries on their silence.
    #[allow(clippy::too_many_arguments)]
    async fn search_one(
        &self,
        entry: &NewznabIndexerEntry,
        query: &str,
        artist: &str,
        album: Option<&str>,
        year: Option<i32>,
        timeout: Duration,
        text_only: bool,
    ) -> Result<(Vec<UsenetRelease>, bool), NewznabError> {
        let now = Instant::now();
        if self
            .backoff_until
            .lock()
            .map(|guard| guard.get(&entry.id).is_some_and(|until| *until > now))
            .unwrap_or(false)
        {
            tracing::info!(
                indexer = entry.name.as_str(),
                "newznab indexer in rate-limit backoff; skipping"
            );
            return Ok((Vec::new(), false));
        }
        let cache_key = (entry.id.clone(), query.to_owned());
        if let Ok(guard) = self.search_cache.lock()
            && let Some((until, releases)) = guard.get(&cache_key)
            && *until > now
        {
            return Ok((releases.clone(), true));
        }
        if let Ok(mut guard) = self.search_cache.lock()
            && guard.len() > SEARCH_CACHE_CAP
        {
            guard.retain(|_, (until, _)| *until > now);
        }
        let caps = self.caps_for(entry, timeout).await;
        let entry_limit = if entry.limit == 0 {
            caps.limit_max
        } else {
            entry.limit
        };
        let limit = entry_limit.min(caps.limit_max);
        let audio_params: std::collections::HashSet<&str> = caps
            .audio_search_params
            .iter()
            .map(String::as_str)
            .collect();
        let use_music = !text_only
            && album.is_some_and(|album| !album.is_empty())
            && caps.supports_audio_search
            && audio_params.contains("artist")
            && audio_params.contains("album");
        // Only send year when the indexer advertises it (Lidarr/Prowlarr
        // send only advertised params); an unadvertised param can be
        // rejected or silently widen.
        let year_param = if audio_params.contains("year") {
            year
        } else {
            None
        };
        let outcome = if use_music {
            let album = album.unwrap_or("");
            self.music_or_fallback(entry, artist, album, query, year_param, limit, timeout)
                .await
        } else {
            entry
                .client
                .search(query, &entry.categories, 0, limit, timeout)
                .await
                .map(|(releases, _)| releases)
        };
        let releases = match outcome {
            Ok(releases) => releases,
            Err(NewznabError::RateLimited { retry_after, .. }) => {
                let backoff = retry_after
                    .map(Duration::from_secs_f64)
                    .unwrap_or(self.rate_limit_backoff);
                if let Ok(mut guard) = self.backoff_until.lock() {
                    guard.insert(entry.id.clone(), now + backoff);
                }
                tracing::warn!(
                    indexer = entry.name.as_str(),
                    backoff_secs = backoff.as_secs(),
                    "newznab indexer rate-limited; backing off"
                );
                return Ok((Vec::new(), false));
            }
            Err(err @ NewznabError::Auth { .. }) => {
                tracing::warn!(
                    indexer = entry.name.as_str(),
                    error = err.to_string().as_str(),
                    "newznab indexer auth failed"
                );
                return Ok((Vec::new(), false));
            }
            Err(err) => return Err(err),
        };
        if let Ok(mut guard) = self.search_cache.lock() {
            guard.insert(cache_key, (now + self.search_cache_ttl, releases.clone()));
        }
        Ok((releases, true))
    }

    /// Structured attempt with the 202 fallback: caps may advertise
    /// audio-search while `t=music` still 202s (a real quirk), so fall
    /// back to `t=search` on `UNKNOWN_FUNCTION` only.
    #[allow(clippy::too_many_arguments)]
    async fn music_or_fallback(
        &self,
        entry: &NewznabIndexerEntry,
        artist: &str,
        album: &str,
        query: &str,
        year: Option<i32>,
        limit: u32,
        timeout: Duration,
    ) -> Result<Vec<UsenetRelease>, NewznabError> {
        match entry
            .client
            .music_search(artist, album, &entry.categories, year, 0, limit, timeout)
            .await
        {
            Ok((releases, _)) => Ok(releases),
            Err(err) if err.code() == Some(UNKNOWN_FUNCTION) => {
                tracing::info!(
                    indexer = entry.name.as_str(),
                    "newznab indexer lacks t=music; falling back to t=search"
                );
                entry
                    .client
                    .search(query, &entry.categories, 0, limit, timeout)
                    .await
                    .map(|(releases, _)| releases)
            }
            Err(err) => Err(err),
        }
    }

    /// Cached caps with the permissive default: a fetch failure keeps the
    /// indexer enabled on working-indexer defaults (v2 `_caps`).
    async fn caps_for(&self, entry: &NewznabIndexerEntry, timeout: Duration) -> NewznabCaps {
        let now = Instant::now();
        if let Ok(guard) = self.caps_cache.lock()
            && let Some((until, caps)) = guard.get(&entry.id)
            && *until > now
        {
            return caps.clone();
        }
        match entry.client.caps(timeout).await {
            Ok(caps) => {
                if let Ok(mut guard) = self.caps_cache.lock() {
                    guard.insert(entry.id.clone(), (now + self.caps_cache_ttl, caps.clone()));
                }
                caps
            }
            Err(err) => {
                tracing::warn!(
                    indexer = entry.name.as_str(),
                    error = err.to_string().as_str(),
                    "newznab caps fetch failed"
                );
                NewznabCaps::default()
            }
        }
    }
}

/// Pool to one list, deduped by the cross-indexer (title, size) identity.
/// Higher-priority indexers come first, so first-seen-wins keeps the
/// preferred copy (v2 `_dedup`).
fn dedup_releases(releases: Vec<UsenetRelease>) -> Vec<UsenetRelease> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(releases.len());
    let mut dropped = 0;
    for release in releases {
        if seen.insert(usenet_identity(&release.title, release.size_bytes)) {
            out.push(release);
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        tracing::info!(
            dropped,
            "newznab fan-out deduped cross-indexer duplicate(s)"
        );
    }
    out
}
