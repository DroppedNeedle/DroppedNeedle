//! Prowlarr indexer search: per-instance HTTP + JSON, ladder, cache.
//!
//! [`ProwlarrClient`] is the raw JSON wrapper around one Prowlarr
//! instance, ported from v2 `prowlarr_client.py` and live-verified against
//! Prowlarr 2.3.5.5327: auth rides an `X-Api-Key` header and is never
//! logged, `downloadUrl` embeds `?apikey=` so it is self-contained for
//! SABnzbd's server-side fetch (and must never be logged either),
//! `publishDate` is ISO-8601 with `Z`, and `ReleaseResource` carries no
//! password signal. [`ProwlarrIndexer`] is the `IndexerProtocol` shape
//! (v2 `prowlarr_indexer.py`): query ladder, search cache, backoff-skip,
//! per-call timeout; every member error maps to `[]`, never a failure.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;

use super::newznab::{
    IndexerHealth, IndexerResult, UsenetRelease, normalize_newznab_query, parse_rfc2822_date,
};

// ---------------------------------------------------------------------------
// Wire models (v2 `prowlarr_models.py`: devopsarr/prowlarr-py shapes,
// tolerant defaults so absent/unknown fields never break decode).
// ---------------------------------------------------------------------------

/// One category row inside a release.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrCategory {
    /// Newznab category id.
    #[serde(default)]
    pub id: i32,
}

/// One `ReleaseResource` from `/api/v1/search` (usenet + torrent mixed;
/// the client filters to `protocol == "usenet"` with a usable URL). Debug
/// is hand-written: the download URL embeds `?apikey=`.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrRelease {
    /// Release guid.
    #[serde(default)]
    pub guid: String,
    /// Release title.
    #[serde(default)]
    pub title: String,
    /// Size bytes.
    #[serde(default)]
    pub size: u64,
    /// File count, when reported.
    #[serde(default)]
    pub files: Option<i64>,
    /// Grab count, when reported.
    #[serde(default)]
    pub grabs: Option<i64>,
    /// Member indexer id.
    #[serde(default)]
    pub indexer_id: i32,
    /// Member indexer name.
    #[serde(default)]
    pub indexer: String,
    /// Category rows.
    #[serde(default)]
    pub categories: Vec<ProwlarrCategory>,
    /// NZB URL (embeds `?apikey=`; never logged).
    #[serde(default)]
    pub download_url: String,
    /// Magnet URL (torrent members; skipped, Usenet only).
    #[serde(default)]
    pub magnet_url: String,
    /// `usenet` or `torrent`.
    #[serde(default)]
    pub protocol: String,
    /// ISO-8601 when present, `""` when absent.
    #[serde(default)]
    pub publish_date: String,
    /// Seeders, when reported.
    #[serde(default)]
    pub seeders: Option<i64>,
    /// Leechers, when reported.
    #[serde(default)]
    pub leechers: Option<i64>,
}

impl std::fmt::Debug for ProwlarrRelease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProwlarrRelease")
            .field("guid", &self.guid)
            .field("title", &self.title)
            .field("size", &self.size)
            .field("files", &self.files)
            .field("grabs", &self.grabs)
            .field("indexer_id", &self.indexer_id)
            .field("indexer", &self.indexer)
            .field("categories", &self.categories)
            .field("download_url", &"<redacted>")
            .field("magnet_url", &self.magnet_url)
            .field("protocol", &self.protocol)
            .field("publish_date", &self.publish_date)
            .field("seeders", &self.seeders)
            .field("leechers", &self.leechers)
            .finish()
    }
}

/// One `IndexerResource` from `GET /api/v1/indexer` (identity + protocol
/// + enablement for health/count reporting).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrIndexerInfo {
    /// Indexer id.
    #[serde(default)]
    pub id: i32,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// `usenet` or `torrent`.
    #[serde(default)]
    pub protocol: String,
    /// Whether this member is searched.
    #[serde(default)]
    pub enable: bool,
}

/// `GET /api/v1/system/status` (subset; degraded-optional: callers must
/// not hard-fail when it 404s).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrSystemStatus {
    /// Server version.
    #[serde(default)]
    pub version: String,
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

/// Prowlarr failure. Like the Newznab error, auth and rate-limit variants
/// let the indexer layer stand down without failing anything.
#[derive(Debug, Clone, Error)]
pub enum ProwlarrError {
    /// The request never got an answer (a malformed base URL maps here
    /// too, the v2 lidarr precedent: never a raw 500).
    #[error("Prowlarr request failed: {0}")]
    Transport(String),
    /// Instance answered HTTP 4xx/5xx (other than auth/429).
    #[error("Prowlarr returned HTTP {status}")]
    Http {
        /// Status code.
        status: u16,
        /// Same as `status`, for parity with v2's `code`.
        code: u16,
    },
    /// HTTP 401/403: the instance rejected the API key.
    #[error("Prowlarr rejected the API key")]
    Auth {
        /// Status code.
        code: u16,
    },
    /// HTTP 429.
    #[error("Prowlarr rate limited")]
    RateLimited {
        /// `Retry-After` seconds, when the header carried a number.
        retry_after: Option<f64>,
    },
    /// A 200 body that isn't the documented JSON (proxy/login page).
    #[error("Prowlarr response decode failed: {0}")]
    Decode(String),
}

impl ProwlarrError {
    /// `Retry-After` seconds, when rate-limited with a header value.
    pub fn retry_after(&self) -> Option<f64> {
        match self {
            ProwlarrError::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Raw client. Debug is hand-written: the key never appears in debug output.
// ---------------------------------------------------------------------------

/// Raw wrapper around one Prowlarr instance. The HTTP client is injected;
/// the key travels in the `X-Api-Key` header only.
pub struct ProwlarrClient {
    http: Client,
    base_url: String,
    api_key: String,
    indexer_name: String,
}

impl std::fmt::Debug for ProwlarrClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProwlarrClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("indexer_name", &self.indexer_name)
            .finish()
    }
}

impl ProwlarrClient {
    /// Build over an injected client. `base_url` is the bare origin (any
    /// `/api/v1` suffix stripped by settings); `/api/v1` is appended here.
    pub fn new(http: Client, base_url: &str, api_key: &str, indexer_name: &str) -> Self {
        ProwlarrClient {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            indexer_name: if indexer_name.is_empty() {
                "prowlarr".to_owned()
            } else {
                indexer_name.to_owned()
            },
        }
    }

    /// `GET /api/v1/system/status`. `None` on 404 (older/alternate builds)
    /// so callers degrade, never hard-fail.
    pub async fn system_status(
        &self,
        timeout: Duration,
    ) -> Result<Option<ProwlarrSystemStatus>, ProwlarrError> {
        match self.get("/system/status", &[], timeout).await {
            Ok(content) => Ok(Some(decode(&content)?)),
            Err(ProwlarrError::Http { code: 404, .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// `GET /api/v1/indexer`: member indexers.
    pub async fn list_indexers(
        &self,
        timeout: Duration,
    ) -> Result<Vec<ProwlarrIndexerInfo>, ProwlarrError> {
        let content = self.get("/indexer", &[], timeout).await?;
        decode(&content)
    }

    /// Basic free-text `GET /api/v1/search` (no `type` param: that is the
    /// parameterized-search path). Verified live: repeated `categories` /
    /// `indexerIds` params and `limit` are all honored.
    pub async fn search(
        &self,
        query: &str,
        categories: &[i32],
        indexer_ids: &[i32],
        limit: u32,
        timeout: Duration,
    ) -> Result<Vec<UsenetRelease>, ProwlarrError> {
        let mut params: Vec<(String, String)> = vec![("query".to_owned(), query.to_owned())];
        for cat in categories {
            params.push(("categories".to_owned(), cat.to_string()));
        }
        for id in indexer_ids {
            params.push(("indexerIds".to_owned(), id.to_string()));
        }
        params.push(("limit".to_owned(), limit.to_string()));
        let content = self.get("/search", &params, timeout).await?;
        let releases: Vec<ProwlarrRelease> = decode(&content)?;
        Ok(releases
            .iter()
            .filter_map(|release| self.to_usenet(release))
            .collect())
    }

    /// Map one release; `None` skips it. Torrent members are skipped,
    /// never failed (only Usenet is supported), as are usenet rows without a
    /// download URL.
    fn to_usenet(&self, release: &ProwlarrRelease) -> Option<UsenetRelease> {
        if !release.protocol.eq_ignore_ascii_case("usenet") {
            return None;
        }
        // Verified live: downloadUrl embeds `?apikey=`, so it is
        // self-contained for SABnzbd's fetch; use as-is, never log it.
        if release.download_url.is_empty() {
            return None;
        }
        Some(UsenetRelease {
            indexer_id: format!("prowlarr:{}", release.indexer_id),
            indexer_name: if release.indexer.is_empty() {
                self.indexer_name.clone()
            } else {
                release.indexer.clone()
            },
            guid: if release.guid.is_empty() {
                release.download_url.clone()
            } else {
                release.guid.clone()
            },
            title: release.title.clone(),
            nzb_url: release.download_url.clone(),
            size_bytes: release.size,
            category_ids: release.categories.iter().map(|cat| cat.id).collect(),
            grabs: release.grabs,
            files: release.files,
            usenet_date: parse_prowlarr_date(&release.publish_date),
            // Verified live: ReleaseResource carries no password signal,
            // so 0 (not passworded) stands.
            password: 0,
        })
    }

    async fn get(
        &self,
        path: &str,
        params: &[(String, String)],
        timeout: Duration,
    ) -> Result<Vec<u8>, ProwlarrError> {
        let url = format!("{}{path}", self.api_base());
        let refs: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let response = self
            .http
            .get(&url)
            .query(&refs)
            .header("X-Api-Key", &self.api_key)
            .timeout(timeout)
            .send()
            .await
            .map_err(|err| ProwlarrError::Transport(transport_detail(&err)))?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            return Err(ProwlarrError::Auth { code: status });
        }
        if status == 429 {
            return Err(ProwlarrError::RateLimited {
                retry_after: retry_after(&response),
            });
        }
        if status >= 400 {
            return Err(ProwlarrError::Http {
                status,
                code: status,
            });
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|err| ProwlarrError::Transport(transport_detail(&err)))
    }

    fn api_base(&self) -> String {
        format!("{}/api/v1", self.base_url)
    }
}

fn decode<T: serde::de::DeserializeOwned>(content: &[u8]) -> Result<T, ProwlarrError> {
    serde_json::from_slice(content).map_err(|err| ProwlarrError::Decode(err.to_string()))
}

fn transport_detail(err: &reqwest::Error) -> String {
    let text = err.to_string();
    if text.trim().is_empty() {
        if err.is_timeout() {
            "timeout".to_owned()
        } else if err.is_connect() {
            "connect error".to_owned()
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
        .map(|seconds| seconds.max(0.0))
}

/// `publishDate` (ISO-8601 when present) → unix time. ISO first, then the
/// reused Newznab RFC-2822 parser (which returns `None` on garbage):
/// never raises, never a new date parser (v2 `_parse_prowlarr_date`).
fn parse_prowlarr_date(value: &str) -> Option<f64> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    parse_iso8601(text).or_else(|| parse_rfc2822_date(text))
}

fn parse_iso8601(text: &str) -> Option<f64> {
    let (date_part, time_part) = text.split_once('T').or_else(|| text.split_once('t'))?;
    let mut date = date_part.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    if date.next().is_some() {
        return None;
    }
    let (clock, zone) = split_zone(time_part)?;
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second_text = clock_parts.next().unwrap_or("0");
    let second: i64 = second_text.split(['.', ',']).next()?.parse().ok()?;
    if clock_parts.next().is_some() {
        return None;
    }
    let days = super::newznab::days_from_civil(year, month, day)?;
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second - zone) as f64)
}

/// Split `HH:MM:SS[.frac](Z|±HH:?MM?)` into clock + offset seconds.
fn split_zone(time_part: &str) -> Option<(&str, i64)> {
    if let Some(clock) = time_part.strip_suffix(['Z', 'z']) {
        return Some((clock, 0));
    }
    let bytes = time_part.as_bytes();
    let mut index = bytes.len();
    while index > 0 {
        index -= 1;
        if bytes[index] == b'+' || (bytes[index] == b'-' && index > 2) {
            let clock = &time_part[..index];
            let zone = &time_part[index..];
            let sign = if bytes[index] == b'-' { -1 } else { 1 };
            let digits: String = zone[1..].chars().filter(|ch| ch.is_ascii_digit()).collect();
            if digits.len() != 4 && digits.len() != 2 {
                return None;
            }
            let hours: i64 = digits[..2].parse().ok()?;
            let minutes: i64 = if digits.len() == 4 {
                digits[2..].parse().ok()?
            } else {
                0
            };
            return Some((clock, sign * (hours * 3_600 + minutes * 60)));
        }
        if !bytes[index].is_ascii_digit() && !matches!(bytes[index], b':' | b'.' | b',') {
            return None;
        }
    }
    // Naive timestamps are read as UTC (v2 attaches UTC when tz-naive).
    Some((time_part, 0))
}

// ---------------------------------------------------------------------------
// Indexer (v2 `ProwlarrIndexer`).
// ---------------------------------------------------------------------------

/// Cap on cached search entries before opportunistic eviction.
const SEARCH_CACHE_CAP: usize = 256;

/// One Prowlarr instance as an indexer: either/or with the native Newznab
/// list (the `usenet_search_backend` selector picks the composite primary).
/// Mirrors the Newznab shape: query ladder, `("prowlarr", query)` search
/// cache, rate-limit backoff-skip, per-call timeout. Every member error
/// maps to `[]`: the fan-out never fails because of Prowlarr.
pub struct ProwlarrIndexer {
    client: Option<ProwlarrClient>,
    categories: Vec<i32>,
    enabled: bool,
    search_cache_ttl: Duration,
    rate_limit_backoff: Duration,
    timeout: Duration,
    search_cache: super::newznab::SearchCache,
    backoff_until: Mutex<Option<Instant>>,
}

impl ProwlarrIndexer {
    /// Build over one instance client (`None` = unconfigured).
    pub fn new(
        client: Option<ProwlarrClient>,
        categories: Vec<i32>,
        enabled: bool,
        search_cache_ttl: Duration,
        rate_limit_backoff: Duration,
        per_indexer_timeout: Duration,
    ) -> Self {
        ProwlarrIndexer {
            client,
            categories,
            enabled,
            search_cache_ttl,
            rate_limit_backoff,
            timeout: per_indexer_timeout,
            search_cache: Mutex::new(HashMap::new()),
            backoff_until: Mutex::new(None),
        }
    }

    /// Indexer name for routing.
    pub fn indexer_name(&self) -> &'static str {
        "usenet"
    }

    /// Configured means enabled with a client.
    pub fn is_configured(&self) -> bool {
        self.enabled && self.client.is_some()
    }

    /// Health never raises. `system_status` is degraded-optional: a `None`
    /// (404) still counts as reachable when the indexer list answers;
    /// only enabled rows count (disabled rows are never searched).
    pub async fn health_check(&self) -> IndexerHealth {
        if !self.is_configured() {
            return IndexerHealth {
                status: "error".to_owned(),
                version: None,
                message: "Prowlarr not configured".to_owned(),
            };
        }
        let Some(client) = &self.client else {
            return IndexerHealth {
                status: "error".to_owned(),
                version: None,
                message: "Prowlarr not configured".to_owned(),
            };
        };
        let status = client.system_status(self.timeout).await;
        let indexers = client.list_indexers(self.timeout).await;
        let (Ok(status), Ok(indexers)) = (status, indexers) else {
            tracing::warn!("prowlarr health: instance unreachable");
            return IndexerHealth {
                status: "error".to_owned(),
                version: None,
                message: "Prowlarr unreachable".to_owned(),
            };
        };
        let version = status.map(|status| status.version);
        let enabled = indexers.iter().filter(|indexer| indexer.enable).count();
        IndexerHealth {
            status: "ok".to_owned(),
            version,
            message: format!("Prowlarr OK - {enabled} enabled indexer(s)"),
        }
    }

    /// Album search: `artist album` free text.
    pub async fn search_album(
        &self,
        artist_name: &str,
        album_title: &str,
        timeout: Duration,
    ) -> Vec<IndexerResult> {
        let query = format!("{artist_name} {album_title}");
        self.search_with_ladder(query.trim(), timeout)
            .await
            .into_iter()
            .map(|usenet| IndexerResult {
                source: "usenet".to_owned(),
                usenet,
            })
            .collect()
    }

    /// Track search: free-text `artist track`, same as the Newznab path.
    pub async fn search_track(
        &self,
        artist_name: &str,
        track_title: &str,
        timeout: Duration,
    ) -> Vec<IndexerResult> {
        let query = format!("{artist_name} {track_title}");
        self.search_with_ladder(query.trim(), timeout)
            .await
            .into_iter()
            .map(|usenet| IndexerResult {
                source: "usenet".to_owned(),
                usenet,
            })
            .collect()
    }

    /// Canonical query first; on a genuine clean empty, one normalized
    /// retry (the #259 ladder, same contract as `NewznabIndexer`).
    async fn search_with_ladder(&self, query: &str, timeout: Duration) -> Vec<UsenetRelease> {
        if query.is_empty() {
            return Vec::new();
        }
        let (releases, clean) = self.search_one(query, timeout).await;
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
            "prowlarr.query_normalized_retry"
        );
        let (releases, _) = self.search_one(&normalized, timeout).await;
        releases
    }

    /// One Prowlarr search. The bool reports a successful answer (results
    /// or a genuine empty); backoff skips, auth failures, and rate limits
    /// report false so the ladder never retries on their silence. Never
    /// raises.
    async fn search_one(&self, query: &str, timeout: Duration) -> (Vec<UsenetRelease>, bool) {
        if !self.is_configured() {
            return (Vec::new(), false);
        }
        let now = Instant::now();
        if self
            .backoff_until
            .lock()
            .map(|guard| guard.is_some_and(|until| until > now))
            .unwrap_or(false)
        {
            tracing::info!("prowlarr instance in rate-limit backoff; skipping");
            return (Vec::new(), false);
        }
        let cache_key = ("prowlarr".to_owned(), query.to_owned());
        if let Ok(guard) = self.search_cache.lock()
            && let Some((until, releases)) = guard.get(&cache_key)
            && *until > now
        {
            return (releases.clone(), true);
        }
        if let Ok(mut guard) = self.search_cache.lock()
            && guard.len() > SEARCH_CACHE_CAP
        {
            guard.retain(|_, (until, _)| *until > now);
        }
        let per_call = timeout.min(self.timeout);
        let Some(client) = &self.client else {
            return (Vec::new(), false);
        };
        match client
            .search(query, &self.categories, &[], 100, per_call)
            .await
        {
            Ok(releases) => {
                if let Ok(mut guard) = self.search_cache.lock() {
                    guard.insert(cache_key, (now + self.search_cache_ttl, releases.clone()));
                }
                (releases, true)
            }
            Err(ProwlarrError::RateLimited { retry_after }) => {
                let backoff = retry_after
                    .map(Duration::from_secs_f64)
                    .unwrap_or(self.rate_limit_backoff);
                if let Ok(mut guard) = self.backoff_until.lock() {
                    *guard = Some(now + backoff);
                }
                tracing::warn!(
                    backoff_secs = backoff.as_secs(),
                    "prowlarr rate-limited; backing off"
                );
                (Vec::new(), false)
            }
            Err(err @ ProwlarrError::Auth { .. }) => {
                tracing::warn!(error = err.to_string().as_str(), "prowlarr auth failed");
                (Vec::new(), false)
            }
            Err(err) => {
                tracing::warn!(error = err.to_string().as_str(), "prowlarr search failed");
                (Vec::new(), false)
            }
        }
    }
}
