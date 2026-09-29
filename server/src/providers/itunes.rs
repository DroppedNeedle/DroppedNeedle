//! iTunes Search API: the "Get it" purchase-link fallback.
//!
//! A Rust port of v2's `backend/repositories/itunes_repository.py` plus the
//! ranking quirk in `ITUNES_API_NOTES.md` (verified live 2026-07-10). The
//! keyless Search API ranks by popularity, not relevance, so a result is
//! only trusted when its artist fuzzy-matches the requested one.
//!
//! Transport rides the shared [`HttpPort`](super::client::HttpPort) GET port.
//! Retry, rate limiting (~20 calls/minute/IP), and caching
//! stay with wiring; degradation recording stays with enrichment, which
//! treats `Ok(None)` as absence and `Err` as a degraded source.

use serde::{Deserialize, Serialize};

use super::client::HttpPort;

/// Provider key used in logs and degradation records.
pub const PROVIDER_NAME: &str = "itunes";
/// Live Search API endpoint (ITUNES_API_NOTES.md).
pub const SEARCH_URL: &str = "https://itunes.apple.com/search";
/// Default storefront country.
pub const DEFAULT_COUNTRY: &str = "US";
/// Page size v2 requests.
pub const SEARCH_LIMIT: u32 = 10;
/// Minimum token-set score (0-100) between the requested artist and the
/// hit's artist. Below this the hit is assumed to be a tribute or cover
/// album that outranked the real record (v2 `_ARTIST_MATCH_THRESHOLD`).
pub const ARTIST_MATCH_THRESHOLD: f64 = 80.0;

/// What can go wrong on an iTunes fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The wire failed (retryable, degraded).
    Transport,
    /// The upstream answered 429; the caller backs off this long.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: f64,
    },
    /// Any other status or an undecodable body.
    Unusable,
}

// ---------------------------------------------------------------------------
// Wire models (default-tolerant; unknown fields are ignored by serde)
// ---------------------------------------------------------------------------

/// One album hit on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireAlbum {
    /// Result kind; only `"collection"` rows are albums.
    #[serde(default, rename = "wrapperType")]
    pub wrapper_type: String,
    /// Album title.
    #[serde(default, rename = "collectionName")]
    pub collection_name: String,
    /// Album artist.
    #[serde(default, rename = "artistName")]
    pub artist_name: String,
    /// Store page for the album.
    #[serde(default, rename = "collectionViewUrl")]
    pub collection_view_url: String,
}

/// A Search API page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSearchResponse {
    /// Hit count.
    #[serde(default, rename = "resultCount")]
    pub result_count: i64,
    /// Raw hits.
    #[serde(default)]
    pub results: Vec<WireAlbum>,
}

// ---------------------------------------------------------------------------
// Normalized model
// ---------------------------------------------------------------------------

/// One matched album on the iTunes/Apple Music store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumResult {
    /// Store page URL.
    pub url: String,
    /// Album title.
    pub collection_name: String,
    /// Album artist.
    pub artist_name: String,
}

// ---------------------------------------------------------------------------
// Pure matching (ports of the v2 selection rules)
// ---------------------------------------------------------------------------

/// Split a name into lowercase alphanumeric tokens.
pub fn tokens(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Levenshtein distance over characters.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Length-normalized similarity in 0..=100, rapidfuzz-ratio style.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a_len: usize = a.chars().count();
    let b_len: usize = b.chars().count();
    if a_len == 0 && b_len == 0 {
        return 100.0;
    }
    let total = a_len + b_len;
    if total == 0 {
        return 100.0;
    }
    let distance = levenshtein(a, b);
    100.0 * (total.saturating_sub(distance)) as f64 / total as f64
}

/// Token-set similarity in 0..=100, a compatible port of the
/// `rapidfuzz.fuzz.token_set_ratio` behavior v2 relies on: the shared
/// tokens are compared against each side's shared-plus-remainder string,
/// and the best pairing wins. Word order stops mattering, and a name
/// whose tokens are a subset of the other scores high; names with no
/// token in common score 0. That is exactly the tribute boundary v2
/// draws: "Piano Tribute Players" shares no token with "Nirvana" and is
/// rejected, while the real artist's own spellings match.
pub fn token_set_ratio(a: &str, b: &str) -> f64 {
    let mut tokens_a = tokens(a);
    let mut tokens_b = tokens(b);
    tokens_a.sort();
    tokens_b.sort();
    let mut common: Vec<String> = Vec::new();
    let mut rest_a: Vec<String> = Vec::new();
    let mut rest_b: Vec<String> = tokens_b;
    for token in tokens_a {
        if let Some(pos) = rest_b.iter().position(|other| *other == token) {
            rest_b.remove(pos);
            common.push(token);
        } else {
            rest_a.push(token);
        }
    }
    if common.is_empty() {
        return 0.0;
    }
    common.sort();
    let mut full_a = common.clone();
    full_a.extend(rest_a);
    let mut full_b = common.clone();
    full_b.extend(rest_b);
    let common = common.join(" ");
    let full_a = full_a.join(" ");
    let full_b = full_b.join(" ");
    similarity(&common, &full_a)
        .max(similarity(&common, &full_b))
        .max(similarity(&full_a, &full_b))
}

/// Pick the first trustworthy album hit (v2 `find_album`): the row must be
/// a `collection` with a store URL and an artist, and the artist must
/// fuzzy-match the requested one. The collection name falls back to the
/// requested album title when the wire omits it.
pub fn select_album(
    results: &[WireAlbum],
    artist_name: &str,
    album_title: &str,
) -> Option<AlbumResult> {
    for hit in results {
        if hit.wrapper_type != "collection" {
            continue;
        }
        if hit.collection_view_url.is_empty() || hit.artist_name.is_empty() {
            continue;
        }
        if token_set_ratio(artist_name, &hit.artist_name) < ARTIST_MATCH_THRESHOLD {
            continue;
        }
        return Some(AlbumResult {
            url: hit.collection_view_url.clone(),
            collection_name: if hit.collection_name.is_empty() {
                album_title.to_owned()
            } else {
                hit.collection_name.clone()
            },
            artist_name: hit.artist_name.clone(),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// iTunes catalog client.
pub struct ITunesClient<'h, H: HttpPort> {
    /// Transport port (scripted fake in tests, real HTTP in wiring).
    pub http: &'h H,
    /// Search endpoint; defaults to the live URL.
    pub search_url: String,
}

impl<'h, H: HttpPort> ITunesClient<'h, H> {
    /// Build a client against the live endpoint.
    pub fn new(http: &'h H) -> Self {
        Self {
            http,
            search_url: SEARCH_URL.to_owned(),
        }
    }

    /// Build a client against a scripted or mirrored endpoint.
    pub fn with_search_url(http: &'h H, search_url: &str) -> Self {
        Self {
            http,
            search_url: search_url.to_owned(),
        }
    }

    /// The store page for an album, or `Ok(None)` when no trustworthy hit
    /// exists. A blank term makes no request at all (v2 `find_album`).
    /// 429 stays actionable with the honored `Retry-After` (60s when the
    /// response gave none); anything else unusual is `Transport` (wire
    /// faults) or `Unusable`.
    pub async fn find_album(
        &self,
        artist_name: &str,
        album_title: &str,
        country: &str,
    ) -> Result<Option<AlbumResult>, FetchError> {
        let term = format!("{artist_name} {album_title}");
        if term.trim().is_empty() {
            return Ok(None);
        }
        let limit = SEARCH_LIMIT.to_string();
        let reply = self
            .http
            .get(
                &self.search_url,
                &[
                    ("term", term.trim()),
                    ("entity", "album"),
                    ("country", country),
                    ("limit", limit.as_str()),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Unusable);
        }
        let page: WireSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        Ok(select_album(&page.results, artist_name, album_title))
    }
}

/// Honor a positive `Retry-After` delay; missing, unparsable, or
/// non-positive values yield `None` so the caller falls back.
fn retry_after_secs(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
}
