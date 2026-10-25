//! YouTube Data API client: preview search with a quota-file governor.
//!
//! Port of v2's YouTube repository, its quota store, the
//! `YouTubeConnectionSettings` validation, and the `YouTubeQuotaResponse`
//! shape.
//!
//! This client only searches for preview videos and verifies API keys.
//! Stored preview links live elsewhere; link persistence is not part of
//! this file.
//!
//! The quota governor is the heart of the port. Every fresh search reserves
//! one unit from a daily budget persisted in `youtube_quota.json` before any
//! HTTP leaves the box, so two concurrent searches (two users) cannot spend
//! the last slot twice, and a restart never forgets what today already spent.
//! Rollover is by UTC date, read from the kernel clock the client is given:
//! a file stamped yesterday reads as zero. The write path is atomic (temp
//! file, fsync, rename, directory fsync), kept from v2.
//!
//! The daily budget is not a rate: it stays here, before the executor.
//! Each reserved unit pays for exactly one request, so a search is sent
//! once and never retried by the executor. The request itself goes through
//! the executor at the `youtube` rate row, with the key as a query pair,
//! never in an error or a log line. A 403 (the API's "quota exceeded"
//! answer among others) is [`ProviderError::Forbidden`].
//!
//! Charging rules, kept from v2's quota tests:
//!
//! - Quota exhaustion, a disabled/misconfigured client and, for a person's
//!   search, an API resting after a rate limit fail before any HTTP,
//!   leaving the file untouched.
//! - A reservation that never dispatches (disabled mid-flight, limit
//!   lowered) is refunded.
//! - A dispatched search stays charged whatever the outcome: transport
//!   failures, upstream errors, and bad payloads are charged, never cached.
//! - A successful empty result (`items: []`) caches the absence, so the UI
//!   stops asking for the same missing video.
//!
//! Shared infrastructure note: the optional-work budget (`reserve_optional_operation`
//! / `check_optional_dispatch`) and provider call counters live in shared
//! infra and are not ported here; wiring should gate background searches
//! before calling [YouTubeClient::search_video] / [YouTubeClient::search_track].
//! Answers live in the provider cache under the `youtube` source, so a
//! preview found before a restart costs nothing after it. Concurrent
//! identical searches share one HTTP call and one reservation through the
//! cache's one fetch per key, and share its outcome, a failure included.
//! Quota getters are async because the quota state sits behind an async
//! lock. Wire the quota path to `<cache_dir>/youtube_quota.json`, and build
//! one client per quota path: clones share its budget and settings.
//! There is no process-wide registry; the composition root owns the client.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::RequestPriority;
use crate::cache::{CacheKey, Fetched, KEEP, Policy, ProviderCacheStore, Source};
use crate::client::{decode, parse_base, payload, unusable_base};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{
    Backoff, Executor, Request, Spec, USER_COOLDOWN_WAIT, Url, standard_classify,
};

/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "youtube";
/// API host.
pub const YOUTUBE_API_HOST: &str = "https://www.googleapis.com";
/// Default daily quota budget, kept from v2.
pub const DEFAULT_DAILY_QUOTA_LIMIT: u32 = 80;
/// Largest accepted daily budget (v2 validates 1..=10000).
pub const MAX_DAILY_QUOTA_LIMIT: u32 = 10_000;
/// Largest search answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// YouTube on the wire: the standard rules, v2's 10 second deadline.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(10),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

/// The YouTube Data API on the executor, built once with the providers.
/// The quota-keeping [`YouTubeClient`] sends through it.
#[derive(Debug, Clone)]
pub struct YouTubeApi {
    executor: Executor,
    base: Option<Url>,
}

impl YouTubeApi {
    /// The API at `base_url`, sending through `executor`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        Self {
            executor,
            base: parse_base(base_url, SOURCE),
        }
    }

    /// The executor's answer for a user-facing call while the API rests
    /// longer than such a call waits: checked before a unit is reserved, so
    /// a search that would never be sent spends nothing.
    fn resting(&self, priority: RequestPriority) -> Option<ProviderError> {
        let cooling = self.executor.limiter().cooling_for();
        (priority.is_user_facing() && cooling > USER_COOLDOWN_WAIT).then_some(
            ProviderError::RateLimited {
                provider: SOURCE,
                retry_after: Some(cooling),
            },
        )
    }

    /// One search for a single video. Sent once: the caller has paid one
    /// unit of the daily budget for it.
    async fn search(
        &self,
        key: &str,
        query: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(
            base,
            &["youtube", "v3", "search"],
            &[
                ("part", "id"),
                ("type", "video"),
                ("maxResults", "1"),
                ("q", query),
                ("key", key),
            ],
        )
        .map_err(|_| unusable_base(SOURCE))?
        .idempotent(false);
        let reply = self.executor.send(request, priority).await?;
        let decoded: YouTubeSearchResponse = decode(&reply)?;
        match decoded.items.into_iter().next() {
            None => Ok(None),
            Some(item) => match item.id.and_then(|id| id.video_id) {
                Some(video_id) => Ok(Some(video_id)),
                None => Err(payload(
                    SOURCE,
                    PayloadKind::Contract,
                    "a search result without a video id",
                )),
            },
        }
    }

    /// Check that `key` may read the API: one `videos.list` for a known
    /// video, sent once, outside the daily search budget.
    ///
    /// # Errors
    ///
    /// The typed failure of the call: a 403 is a key that is invalid or
    /// lacks the YouTube Data API.
    pub async fn check_key(
        &self,
        key: &str,
        priority: RequestPriority,
    ) -> Result<(), ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let request = Request::get(
            base,
            &["youtube", "v3", "videos"],
            &[("part", "id"), ("id", "dQw4w9WgXcQ"), ("key", key)],
        )
        .map_err(|_| unusable_base(SOURCE))?
        .idempotent(false);
        self.executor.send(request, priority).await.map(drop)
    }
}

/// What kind of preview a search is after. Album and track searches are
/// separate cache identities: the same artist+title pair can hold two
/// different video ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchKind {
    /// Full-album preview; the query gains a `full album` suffix.
    Album,
    /// Single-track preview; the query is bare.
    Track,
}

impl SearchKind {
    const fn key_part(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Track => "track",
        }
    }
}

/// Cache identity: the kind plus the artist/title pair, case folded.
fn preview_key(kind: SearchKind, artist: &str, title: &str) -> CacheKey {
    CacheKey::hashed(Source::YouTube, kind.key_part(), &[artist, title])
}

/// How long a preview answer is kept. Each search spends a unit of the
/// daily quota and a found video id stays valid, so it is kept as long as
/// the cache keeps anything; "none found" is asked again after a week, by
/// when an upload may have appeared.
const PREVIEWS: Policy = Policy {
    fresh: KEEP,
    miss: Duration::from_secs(7 * 86_400),
};

/// Connection settings. Wiring pushes fresh settings via
/// [YouTubeClient::update_settings]; v2's live settings-getter becomes a
/// push at this seam.
#[derive(Clone, PartialEq)]
pub struct YouTubeSettings {
    /// API key.
    pub api_key: String,
    /// Master switch for the feature.
    pub enabled: bool,
    /// Switch for API-backed search specifically.
    pub api_enabled: bool,
    /// Daily quota budget, 1..=[MAX_DAILY_QUOTA_LIMIT].
    pub daily_quota_limit: u32,
}

impl std::fmt::Debug for YouTubeSettings {
    // The key never prints.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeSettings")
            .field("enabled", &self.enabled)
            .field("api_enabled", &self.api_enabled)
            .field("daily_quota_limit", &self.daily_quota_limit)
            .finish_non_exhaustive()
    }
}

impl YouTubeSettings {
    /// Validate the budget range, mirroring v2's settings validation.
    pub fn validate(&self) -> Result<(), YoutubeError> {
        if self.daily_quota_limit < 1 || self.daily_quota_limit > MAX_DAILY_QUOTA_LIMIT {
            return Err(YoutubeError::NotConfigured(format!(
                "daily quota limit must be between 1 and {MAX_DAILY_QUOTA_LIMIT}"
            )));
        }
        Ok(())
    }

    /// The usable key, when the feature is switched on and a key is set.
    fn usable_key(&self) -> Option<String> {
        if self.enabled && self.api_enabled && !self.api_key.trim().is_empty() {
            Some(self.api_key.trim().to_owned())
        } else {
            None
        }
    }
}

/// Quota status snapshot, the `YouTubeQuotaResponse` shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaStatus {
    /// Units spent today.
    pub used: u32,
    /// Today's budget.
    pub limit: u32,
    /// Budget left, never negative.
    pub remaining: u32,
    /// UTC date the usage belongs to, YYYY-MM-DD.
    pub date: String,
}

/// What can go wrong on a YouTube call. Configuration, the budget and the
/// quota file are local; everything the API answers is a [`ProviderError`].
#[derive(Debug, Clone, Error)]
pub enum YoutubeError {
    /// Search is disabled, unconfigured, or the settings are invalid.
    #[error("youtube search is not configured: {0}")]
    NotConfigured(String),
    /// Today's budget is spent.
    #[error("youtube daily quota exceeded")]
    QuotaExhausted,
    /// The quota file could not be loaded or persisted.
    #[error("youtube quota store failed: {0}")]
    QuotaStore(String),
    /// The search was sent and failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Quota file name inside the cache directory. It keeps the day's count
/// across restarts.
pub const QUOTA_FILE_NAME: &str = "youtube_quota.json";

/// Durable quota state: the UTC date it belongs to plus units spent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct QuotaState {
    #[serde(default)]
    date: String,
    #[serde(default)]
    count: u32,
}

/// State shared by every clone of one client: the quota file guard and
/// the settings snapshot.
#[derive(Debug)]
struct ClientState {
    quota_path: PathBuf,
    quota: tokio::sync::Mutex<QuotaState>,
    settings: Mutex<YouTubeSettings>,
}

/// The YouTube client. Cheap to clone: clones share the quota file, the
/// cache, and the settings of their path. Construct once per quota path at
/// wiring time.
#[derive(Clone)]
pub struct YouTubeClient {
    api: YouTubeApi,
    clock: Arc<dyn Clock>,
    cache: ProviderCacheStore,
    state: Arc<ClientState>,
}

impl std::fmt::Debug for YouTubeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeClient")
            .field("quota_path", &self.state.quota_path)
            .finish_non_exhaustive()
    }
}

impl YouTubeClient {
    /// Client over `api` with `quota_path` as its durable budget file (wire
    /// `<cache_dir>/youtube_quota.json`), dating the budget by `clock` and
    /// keeping its answers in `cache`. Fails when the settings are invalid
    /// or a present quota file cannot be decoded.
    pub fn new(
        api: YouTubeApi,
        quota_path: PathBuf,
        settings: YouTubeSettings,
        clock: Arc<dyn Clock>,
        cache: ProviderCacheStore,
    ) -> Result<Self, YoutubeError> {
        settings.validate()?;
        let loaded = load_quota_file(&quota_path)?;
        let state = Arc::new(ClientState {
            quota_path,
            quota: tokio::sync::Mutex::new(loaded),
            settings: Mutex::new(settings),
        });
        Ok(Self {
            api,
            clock,
            cache,
            state,
        })
    }

    /// Replace the whole settings snapshot after validating it.
    pub fn update_settings(&self, settings: YouTubeSettings) -> Result<(), YoutubeError> {
        settings.validate()?;
        let mut current = self.lock_settings();
        *current = settings;
        Ok(())
    }

    /// True when search is switched on and a non-blank key is set.
    pub fn is_configured(&self) -> bool {
        self.lock_settings().usable_key().is_some()
    }

    /// Units of budget left today, never negative.
    pub async fn quota_remaining(&self) -> u32 {
        let status = self.quota_status().await;
        status.remaining
    }

    /// True when search is configured and budget remains.
    pub async fn search_available(&self) -> bool {
        self.is_configured() && self.quota_remaining().await > 0
    }

    /// Quota status snapshot for the UI.
    pub async fn get_quota_status(&self) -> QuotaStatus {
        self.quota_status().await
    }

    /// True when the pair already has a cached preview (or cached absence).
    pub async fn is_cached(&self, artist: &str, title: &str, kind: SearchKind) -> bool {
        self.cache
            .peek::<String>(&preview_key(kind, artist, title))
            .await
            .is_some()
    }

    /// Cached flags for track pairs, keyed `artist|track` lowercased.
    pub async fn are_cached(&self, pairs: &[(&str, &str)]) -> HashMap<String, bool> {
        let mut cached = HashMap::new();
        for (artist, track) in pairs {
            let key = format!("{}|{}", artist.to_lowercase(), track.to_lowercase());
            cached.insert(key, self.is_cached(artist, track, SearchKind::Track).await);
        }
        cached
    }

    /// Album preview search: the query gains a `full album` suffix.
    pub async fn search_video(
        &self,
        artist: &str,
        album: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, YoutubeError> {
        self.search(SearchKind::Album, artist, album, priority)
            .await
    }

    /// Track preview search: the query is the bare artist + title.
    pub async fn search_track(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, YoutubeError> {
        self.search(SearchKind::Track, artist, track, priority)
            .await
    }

    /// One search through the provider cache: an answer kept from before
    /// (a restart included) costs nothing, and concurrent identical searches
    /// share one dispatch, so they cost one HTTP call and one quota unit.
    async fn search(
        &self,
        kind: SearchKind,
        artist: &str,
        title: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, YoutubeError> {
        let client = self.clone();
        let (artist, title) = (artist.to_owned(), title.to_owned());
        self.cache
            .get_or_fetch(
                &preview_key(kind, &artist, &title),
                PREVIEWS,
                priority,
                move |priority| async move {
                    let found = client.dispatch(kind, &artist, &title, priority).await?;
                    Ok(found.map_or(Fetched::Missing, Fetched::Found))
                },
            )
            .await
    }

    /// Reserve-then-fetch. The reservation lands in the file before any HTTP;
    /// anything that stops the dispatch refunds it, while a dispatched call
    /// stays charged whatever the outcome.
    async fn dispatch(
        &self,
        kind: SearchKind,
        artist: &str,
        title: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, YoutubeError> {
        let settings = self.snapshot_settings();
        if settings.usable_key().is_none() {
            return Err(YoutubeError::NotConfigured(
                "youtube API search is disabled or not configured".to_owned(),
            ));
        }
        if let Some(resting) = self.api.resting(priority) {
            return Err(resting.into());
        }
        let mut date = self.quota_reserve(settings.daily_quota_limit).await?;
        loop {
            if date == self.today() {
                break;
            }
            // Midnight struck between reserve and dispatch: hand the stale
            // unit back and reserve against the new day, kept from v2.
            self.quota_refund(&date).await?;
            let settings = self.snapshot_settings();
            if settings.usable_key().is_none() {
                return Err(YoutubeError::NotConfigured(
                    "youtube API search is disabled or not configured".to_owned(),
                ));
            }
            date = self.quota_reserve(settings.daily_quota_limit).await?;
        }
        let settings = self.snapshot_settings();
        let api_key = match settings.usable_key() {
            Some(key) => key,
            None => {
                self.quota_refund(&date).await?;
                return Err(YoutubeError::NotConfigured(
                    "youtube API search is disabled or not configured".to_owned(),
                ));
            }
        };
        if self.quota_count().await > settings.daily_quota_limit {
            // The budget shrank under the reservation: refund and refuse.
            self.quota_refund(&date).await?;
            return Err(YoutubeError::QuotaExhausted);
        }
        let query = match kind {
            SearchKind::Album => format!("{artist} {title} full album"),
            SearchKind::Track => format!("{artist} {title}"),
        };
        Ok(self.api.search(&api_key, &query, priority).await?)
    }

    /// Reserve one unit, persisting before returning. Fails closed when the
    /// file cannot be written: no reservation, no HTTP.
    async fn quota_reserve(&self, limit: u32) -> Result<String, YoutubeError> {
        let mut state = self.state.quota.lock().await;
        let today = self.today();
        let count = if state.date == today { state.count } else { 0 };
        if count >= limit {
            return Err(YoutubeError::QuotaExhausted);
        }
        let next = QuotaState {
            date: today.clone(),
            count: count + 1,
        };
        persist_quota_file(&self.state.quota_path, &next).await?;
        *state = next;
        Ok(today)
    }

    /// Hand back one unit previously reserved for `date`.
    async fn quota_refund(&self, date: &str) -> Result<(), YoutubeError> {
        let mut state = self.state.quota.lock().await;
        if state.date == date {
            let next = QuotaState {
                date: date.to_owned(),
                count: state.count.saturating_sub(1),
            };
            persist_quota_file(&self.state.quota_path, &next).await?;
            *state = next;
        }
        Ok(())
    }

    /// Units spent today (a stale-dated file reads as zero).
    async fn quota_count(&self) -> u32 {
        let state = self.state.quota.lock().await;
        if state.date == self.today() {
            state.count
        } else {
            0
        }
    }

    /// Status snapshot: used/limit/remaining/date.
    async fn quota_status(&self) -> QuotaStatus {
        let limit = self.snapshot_settings().daily_quota_limit;
        let used = self.quota_count().await;
        QuotaStatus {
            used,
            limit,
            remaining: limit.saturating_sub(used),
            date: self.today(),
        }
    }

    /// Today's UTC date by the client's clock.
    fn today(&self) -> String {
        utc_date(self.clock.now_unix())
    }

    fn snapshot_settings(&self) -> YouTubeSettings {
        self.lock_settings().clone()
    }

    fn lock_settings(&self) -> std::sync::MutexGuard<'_, YouTubeSettings> {
        self.state
            .settings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Load the quota file. Missing means "nothing spent yet"; unreadable or
/// undecodable means a store error, kept from v2.
fn load_quota_file(path: &Path) -> Result<QuotaState, YoutubeError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| YoutubeError::QuotaStore("could not load youtube quota".to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(QuotaState::default()),
        Err(_) => Err(YoutubeError::QuotaStore(
            "could not load youtube quota".to_owned(),
        )),
    }
}

/// Persist quota state off the async runtime. The write itself is atomic
/// (temp file, fsync, rename, directory fsync), kept from v2.
async fn persist_quota_file(path: &Path, state: &QuotaState) -> Result<(), YoutubeError> {
    let bytes = serde_json::to_vec(state)
        .map_err(|_| YoutubeError::QuotaStore("could not persist youtube quota".to_owned()))?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_quota_file_atomic(&path, &bytes))
        .await
        .map_err(|_| YoutubeError::QuotaStore("could not persist youtube quota".to_owned()))?
}

/// Atomic quota write: temp file beside the target, fsync, rename over the
/// target, fsync the directory. Failures leave the old file in place.
fn write_quota_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), YoutubeError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let store_error = || YoutubeError::QuotaStore("could not persist youtube quota".to_owned());
    let parent = path.parent().ok_or_else(store_error)?;
    std::fs::create_dir_all(parent).map_err(|_| store_error())?;
    // The process id and a counter keep concurrent writers apart.
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".youtube-quota-{}-{seq}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        std::fs::write(&temp, bytes)?;
        let file = std::fs::File::open(&temp)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        let dir = std::fs::File::open(parent)?;
        dir.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err(store_error());
    }
    Ok(())
}

/// The UTC date of `unix_secs` as YYYY-MM-DD, the quota file's day stamp.
#[must_use]
pub fn utc_date(unix_secs: i64) -> String {
    let date = time::OffsetDateTime::from_unix_timestamp(unix_secs)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// Search answer id block.
#[derive(Debug, Clone, Deserialize)]
struct YouTubeSearchId {
    #[serde(default, rename = "videoId")]
    video_id: Option<String>,
}

/// One search answer item.
#[derive(Debug, Clone, Deserialize)]
struct YouTubeSearchItem {
    #[serde(default)]
    id: Option<YouTubeSearchId>,
}

/// Search answer. `items` is required: a body without it is an invalid
/// payload, kept from v2's required field.
#[derive(Debug, Clone, Deserialize)]
struct YouTubeSearchResponse {
    items: Vec<YouTubeSearchItem>,
}
