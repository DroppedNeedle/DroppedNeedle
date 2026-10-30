//! Newznab indexer search: fan-out across the configured indexers, the
//! query ladder and the result pool.
//!
//! Each indexer is called through its client in the providers crate
//! ([`NewznabClient`]), which paces it, honours its `Retry-After` and
//! "limit reached" answers and holds it to the daily caps it reports.
//! [`NewznabIndexer`] fans one logical search across the configured
//! indexers: caps-gated query strategy, the 202 `t=music` → `t=search`
//! fallback, the punctuation-normalized retry ladder (#259), search
//! caching, and cross-indexer dedup by the (title, size) identity. Every
//! search names its priority: a person's search is user priority, the
//! download worker's background.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::future::{BoxFuture, join_all};

use super::indexer::{Indexer, IndexerQuery};
use crate::downloads::source_tag::SourceTag;
use droppedneedle_providers::{ProviderError, RequestPriority};

pub use droppedneedle_providers::newznab::{
    NewznabApiLimits, NewznabCaps, NewznabCategory, NewznabClient, NewznabError,
    NewznabSubcategory, UsenetRelease, days_from_civil, parse_rfc2822_date,
};

/// One tagged search hit from any [`Indexer`](super::indexer::Indexer).
#[derive(Debug, Clone)]
pub struct IndexerResult {
    /// Always [`SourceTag::Usenet`]: every indexer feeds SABnzbd.
    pub source: SourceTag,
    /// The release.
    pub usenet: UsenetRelease,
}

/// Cross-indexer release identity: normalised title + size bucketed to the
/// MB, joined by the unit separator. Trivial
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
// Query normalization (issue #259).
// ---------------------------------------------------------------------------

/// Characters a scene release name never carries. A canonical MusicBrainz
/// title like "Honestly, Nevermind" must also be searchable without them:
/// live-verified NZBGeek behavior returns 0 releases for the comma query
/// and 3 lossless for the stripped one. Survivors are removed (not
/// spaced), so "Man's" becomes "Mans" exactly the way scene names drop
/// the apostrophe.
const QUERY_STRIP: [char; 8] = [',', '\'', '"', '?', '!', ';', ':', '&'];

/// Fold one typographic quote to ASCII.
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
// Fan-out indexer.
// ---------------------------------------------------------------------------

/// Newznab "No such function": fall back `t=music` → `t=search`.
const UNKNOWN_FUNCTION: i32 = 202;
/// Cap on cached search entries before opportunistic eviction.
const SEARCH_CACHE_CAP: usize = 256;
/// `(indexer id, query)` → expiry + pooled releases.
pub(crate) type SearchCache = Mutex<HashMap<(String, String), (Instant, Vec<UsenetRelease>)>>;

/// One configured indexer paired with its client. A runtime holder the
/// wiring builds from settings; never serialised.
pub struct NewznabIndexerEntry {
    /// The indexer's client.
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

/// Health answer.
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
/// `t=search`) and a short-TTL search cache so fan-out + failover +
/// auto-retry don't each re-hit the daily API budget. Pacing, rate limits
/// and the daily caps are the client's. Results pool and dedup by the
/// normalised (title, size) identity, the higher-priority indexer's copy
/// winning. One indexer erroring never fails the fan-out.
pub struct NewznabIndexer {
    entries: Vec<NewznabIndexerEntry>,
    search_cache_ttl: Duration,
    caps_cache_ttl: Duration,
    caps_cache: Mutex<HashMap<String, (Instant, NewznabCaps)>>,
    search_cache: SearchCache,
}

impl NewznabIndexer {
    /// Build over the configured entries (sorted: higher priority first,
    /// so dedup keeps the preferred copy).
    pub fn new(
        mut entries: Vec<NewznabIndexerEntry>,
        search_cache_ttl: Duration,
        caps_cache_ttl: Duration,
    ) -> Self {
        entries.sort_by_key(|entry| entry.priority);
        NewznabIndexer {
            entries,
            search_cache_ttl,
            caps_cache_ttl,
            caps_cache: Mutex::new(HashMap::new()),
            search_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Configured means at least one entry is enabled.
    pub fn is_configured(&self) -> bool {
        self.entries.iter().any(|entry| entry.enabled)
    }

    /// Health never raises: unreachable indexers are skipped with a warning.
    /// An indexer that is rate limiting, or at its daily cap, is reachable.
    pub async fn health_check(&self, priority: RequestPriority) -> IndexerHealth {
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
            match entry.client.caps(priority).await {
                Ok(caps) => {
                    reachable += 1;
                    if version.is_none() {
                        version = caps.server_version;
                    }
                }
                Err(NewznabError::Provider(ProviderError::RateLimited { .. })) => reachable += 1,
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
        priority: RequestPriority,
    ) -> Vec<IndexerResult> {
        let query = format!("{artist_name} {album_title}");
        let query = query.trim().to_owned();
        self.search_with_ladder(&query, artist_name, Some(album_title), year, priority)
            .await
            .into_iter()
            .map(|usenet| IndexerResult {
                source: SourceTag::Usenet,
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
        priority: RequestPriority,
    ) -> Vec<IndexerResult> {
        let query = format!("{artist_name} {track_title}");
        let query = query.trim().to_owned();
        self.search_with_ladder(&query, artist_name, None, None, priority)
            .await
            .into_iter()
            .map(|usenet| IndexerResult {
                source: SourceTag::Usenet,
                usenet,
            })
            .collect()
    }

    /// Canonical query first; on a genuine empty, one normalized retry
    /// (#259). The retry fires only when every enabled indexer actually
    /// answered (at least one successful empty, none rate-limited,
    /// auth-failed, or errored): an indexer that sat out a rate limit may
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
        priority: RequestPriority,
    ) -> Vec<UsenetRelease> {
        let (releases, clean) = self
            .fan_out(query, artist, album, year, priority, false)
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
            .fan_out(&normalized, artist, album, year, priority, true)
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
        priority: RequestPriority,
        text_only: bool,
    ) -> (Vec<UsenetRelease>, bool) {
        let enabled: Vec<&NewznabIndexerEntry> =
            self.entries.iter().filter(|entry| entry.enabled).collect();
        if enabled.is_empty() {
            return (Vec::new(), false);
        }
        let results =
            join_all(enabled.iter().map(|entry| {
                self.search_one(entry, query, artist, album, year, priority, text_only)
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
    /// (results or a genuine empty); rate limits (the indexer's, its daily
    /// cap, or a turn too far off) and refused keys report false so the
    /// ladder never retries on their silence.
    #[allow(clippy::too_many_arguments)]
    async fn search_one(
        &self,
        entry: &NewznabIndexerEntry,
        query: &str,
        artist: &str,
        album: Option<&str>,
        year: Option<i32>,
        priority: RequestPriority,
        text_only: bool,
    ) -> Result<(Vec<UsenetRelease>, bool), NewznabError> {
        let now = Instant::now();
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
        let caps = self.caps_for(entry, priority).await;
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
            self.music_or_fallback(entry, artist, album, query, year_param, limit, priority)
                .await
        } else {
            entry
                .client
                .search(query, &entry.categories, 0, limit, priority)
                .await
                .map(|(releases, _)| releases)
        };
        let releases = match outcome {
            Ok(releases) => releases,
            Err(NewznabError::Provider(ProviderError::RateLimited { retry_after, .. })) => {
                tracing::warn!(
                    indexer = entry.name.as_str(),
                    wait_secs = retry_after.map(|wait| wait.as_secs()),
                    "newznab indexer rate-limited; skipping"
                );
                return Ok((Vec::new(), false));
            }
            Err(
                err @ NewznabError::Provider(
                    ProviderError::Unauthorized { .. } | ProviderError::Forbidden { .. },
                ),
            ) => {
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
        priority: RequestPriority,
    ) -> Result<Vec<UsenetRelease>, NewznabError> {
        match entry
            .client
            .music_search(artist, album, &entry.categories, year, 0, limit, priority)
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
                    .search(query, &entry.categories, 0, limit, priority)
                    .await
                    .map(|(releases, _)| releases)
            }
            Err(err) => Err(err),
        }
    }

    /// Cached caps with the permissive default: a fetch failure keeps the
    /// indexer enabled on working-indexer defaults.
    async fn caps_for(
        &self,
        entry: &NewznabIndexerEntry,
        priority: RequestPriority,
    ) -> NewznabCaps {
        let now = Instant::now();
        if let Ok(guard) = self.caps_cache.lock()
            && let Some((until, caps)) = guard.get(&entry.id)
            && *until > now
        {
            return caps.clone();
        }
        match entry.client.caps(priority).await {
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

impl Indexer for NewznabIndexer {
    fn search<'a>(
        &'a self,
        query: &'a IndexerQuery<'a>,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Vec<IndexerResult>> {
        Box::pin(async move {
            match query {
                IndexerQuery::Album(target) => {
                    self.search_album(&target.artist, &target.album_title, target.year, priority)
                        .await
                }
                IndexerQuery::Track { artist, title } => {
                    self.search_track(artist, title, priority).await
                }
            }
        })
    }
}

/// Pool to one list, deduped by the cross-indexer (title, size) identity.
/// Higher-priority indexers come first, so first-seen-wins keeps the
/// preferred copy.
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
