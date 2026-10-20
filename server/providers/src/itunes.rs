//! iTunes Search API: the "Get it" purchase-link fallback.
//!
//! A Rust port of v2's iTunes repository. The keyless Search API returns hits
//! by other artists and tribute albums too, so a result is only trusted when
//! its artist fuzzy-matches the requested one. The wire shapes, and the dates
//! they were verified on, are in `itunes/models.rs`.
//!
//! Requests go through the executor at the `itunes` row (about 20 calls a
//! minute), which purchase links and the iTunes preview leg share.
//! `Ok(None)` is absence; every failure is a typed [`ProviderError`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

pub(crate) mod models;

use self::models::{AlbumHit, AlbumSearch};

/// The Search API endpoint the client sends to.
pub const SEARCH_URL: &str = "https://itunes.apple.com/search";
/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "itunes";
/// Largest search page read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The iTunes Search API on the wire: the standard rules, and a 429 that
/// names no wait rests a minute (v2's default).
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};
/// Page size v2 requests.
pub const SEARCH_LIMIT: u32 = 10;
/// Minimum token-set score (0-100) between the requested artist and the
/// hit's artist. Below this the hit is assumed to be a tribute or cover
/// album that outranked the real record.
pub const ARTIST_MATCH_THRESHOLD: f64 = 80.0;

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

/// Pick the first trustworthy album hit: the row must be
/// a `collection` with a store URL and an artist, and the artist must
/// fuzzy-match the requested one. The collection name falls back to the
/// requested album title when the wire omits it.
pub fn select_album(
    results: &[AlbumHit],
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

/// iTunes Search API client: purchase links here, and the preview leg of
/// [`PreviewClient`](crate::preview::PreviewClient) through [`search`](Self::search).
/// Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct ITunesClient {
    executor: Executor,
    search_url: Option<Url>,
}

impl ITunesClient {
    /// The client sending through `executor` to the search endpoint at
    /// `search_url`.
    #[must_use]
    pub fn new(executor: Executor, search_url: &str) -> Self {
        Self {
            executor,
            search_url: parse_base(search_url, SOURCE),
        }
    }

    /// The store page for an album, or `Ok(None)` when no trustworthy hit
    /// exists. A blank term makes no request at all.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn find_album(
        &self,
        artist_name: &str,
        album_title: &str,
        country: &str,
        priority: RequestPriority,
    ) -> Result<Option<AlbumResult>, ProviderError> {
        let term = format!("{artist_name} {album_title}");
        if term.trim().is_empty() {
            return Ok(None);
        }
        let limit = SEARCH_LIMIT.to_string();
        let Some(reply) = self
            .search(
                &[
                    ("term", term.trim()),
                    ("entity", "album"),
                    ("country", country),
                    ("limit", limit.as_str()),
                ],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let page: AlbumSearch = decode(&reply)?;
        Ok(select_album(&page.results, artist_name, album_title))
    }

    /// One search with `query`, or `None` on a 404.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn search(
        &self,
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        let base = self
            .search_url
            .as_ref()
            .ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(base, &[], query).map_err(|_| unusable_base(SOURCE))?;
        match self.executor.send(request, priority).await {
            Ok(reply) => Ok(Some(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
