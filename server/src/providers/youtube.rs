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
//! HTTP leaves the box, so two client instances (two users) cannot spend the
//! last slot twice, and a restart never forgets what today already spent.
//! Rollover is by UTC date: a file stamped yesterday reads as zero. The
//! write path is atomic (temp file, fsync, rename, directory fsync), kept
//! from v2.
//!
//! Charging rules, kept from v2's quota tests:
//!
//! - Quota exhaustion and a disabled/misconfigured client fail before any
//!   HTTP, leaving the file untouched.
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
//! Concurrent identical searches share one HTTP call and one reservation via
//! a per-key mutex; on a failed dispatch the waiter retries once itself
//! rather than sharing the owner's error (v2 joins waiters onto the owner's
//! outcome instead). Also note the sync-to-async shifts: quota getters are
//! async here because the quota state sits behind an async lock.
//! Wire the quota path to `<cache_dir>/youtube_quota.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// API host. Tests point elsewhere via [YouTubeClient::with_base_url].
pub const YOUTUBE_API_HOST: &str = "https://www.googleapis.com";
/// Search endpoint path.
pub const YOUTUBE_SEARCH_PATH: &str = "/youtube/v3/search";
/// Key-verification endpoint path.
pub const YOUTUBE_VERIFY_PATH: &str = "/youtube/v3/videos";
/// Known video used to probe a key, kept from v2.
pub const VERIFY_VIDEO_ID: &str = "dQw4w9WgXcQ";
/// Default daily quota budget, kept from v2.
pub const DEFAULT_DAILY_QUOTA_LIMIT: u32 = 80;
/// Largest accepted daily budget (v2 validates 1..=10000).
pub const MAX_DAILY_QUOTA_LIMIT: u32 = 10_000;
/// Preview cache size; oldest entries evict first.
pub const PREVIEW_CACHE_MAX: usize = 100;
/// Per-request timeout, kept from v2's 10s client calls.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Cache identity: the kind plus the lowercased artist/title pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SearchKey {
    kind: SearchKind,
    artist: String,
    title: String,
}

/// Connection settings. Wiring pushes fresh settings via
/// [YouTubeClient::update_settings]; v2's live settings-getter becomes a
/// push at this seam.
#[derive(Debug, Clone, PartialEq)]
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

/// What can go wrong on a YouTube call. Quota exhaustion and configuration
/// are deterministic for the call and must not be retried; transport and
/// upstream rate limiting may be.
#[derive(Debug, Error)]
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
    /// The request never completed (DNS, TLS, connect, timeout).
    #[error("youtube search transport failed: {0}")]
    Transport(String),
    /// The provider answered 429.
    #[error("youtube search rate limited upstream")]
    UpstreamRateLimited,
    /// The provider answered outside 200..300 (v2 matches the 2xx range
    /// here, unlike the strict-200 events clients).
    #[error("youtube search failed with HTTP {status}")]
    Api {
        /// The status the provider sent back.
        status: u16,
    },
    /// The body was not the documented shape, or carried items without a
    /// usable video id.
    #[error("youtube search returned an invalid payload: {0}")]
    InvalidPayload(String),
}

/// Durable quota state: the UTC date it belongs to plus units spent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct QuotaState {
    #[serde(default)]
    date: String,
    #[serde(default)]
    count: u32,
}

/// One cache entry: the found video id, or the cached absence.
#[derive(Debug, Clone)]
struct CacheEntry {
    value: Option<String>,
    touched: u64,
}

/// Small LRU over the preview cache. Absences (`None`) are cached too.
#[derive(Debug)]
struct PreviewCache {
    entries: HashMap<SearchKey, CacheEntry>,
    clock: u64,
}

impl PreviewCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
        }
    }

    fn get(&mut self, key: &SearchKey) -> Option<Option<String>> {
        let entry = self.entries.get_mut(key)?;
        self.clock = self.clock.wrapping_add(1);
        entry.touched = self.clock;
        Some(entry.value.clone())
    }

    fn contains(&self, key: &SearchKey) -> bool {
        self.entries.contains_key(key)
    }

    fn put(&mut self, key: &SearchKey, value: Option<String>) {
        self.clock = self.clock.wrapping_add(1);
        let touched = self.clock;
        self.entries
            .insert(key.clone(), CacheEntry { value, touched });
        while self.entries.len() > PREVIEW_CACHE_MAX {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => {
                    self.entries.remove(&key);
                }
                None => break,
            }
        }
    }
}

/// State shared by every client instance on one quota path: the quota file
/// guard, the preview cache, in-flight searches, and the settings snapshot.
#[derive(Debug)]
struct ClientState {
    quota_path: PathBuf,
    quota: tokio::sync::Mutex<QuotaState>,
    today_override: Mutex<Option<String>>,
    cache: Mutex<PreviewCache>,
    inflight: tokio::sync::Mutex<HashMap<SearchKey, Arc<tokio::sync::Mutex<()>>>>,
    settings: Mutex<YouTubeSettings>,
}

/// Per-path shared state, mirroring v2's `_states` weak registry so two
/// client instances on one quota file share one budget. Weak refs let idle
/// paths drop out; the registry lock is only ever held in the constructor,
/// never during searches.
static STATES: OnceLock<Mutex<HashMap<PathBuf, Weak<ClientState>>>> = OnceLock::new();

/// The YouTube client. Cheap to clone: clones share the quota file, the
/// cache, and the settings of their path. Construct once per quota path at
/// wiring time.
#[derive(Debug, Clone)]
pub struct YouTubeClient {
    http: reqwest::Client,
    base_url: String,
    state: Arc<ClientState>,
}

impl YouTubeClient {
    /// Client against the production host with `quota_path` as its durable
    /// budget file (wire `<cache_dir>/youtube_quota.json`). Fails when the
    /// settings are invalid or a present quota file cannot be decoded.
    pub fn new(
        http: reqwest::Client,
        quota_path: PathBuf,
        settings: YouTubeSettings,
    ) -> Result<Self, YoutubeError> {
        Self::with_base_url(http, quota_path, settings, YOUTUBE_API_HOST)
    }

    /// Client against an override host (scripted fakes in tests).
    pub fn with_base_url(
        http: reqwest::Client,
        quota_path: PathBuf,
        settings: YouTubeSettings,
        base_url: impl Into<String>,
    ) -> Result<Self, YoutubeError> {
        settings.validate()?;
        let key = registry_key(&quota_path);
        let state = {
            let registry = STATES.get_or_init(|| Mutex::new(HashMap::new()));
            let mut guard = registry.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(shared) = guard.get(&key).and_then(Weak::upgrade) {
                shared
            } else {
                let loaded = load_quota_file(&quota_path)?;
                let shared = Arc::new(ClientState {
                    quota_path,
                    quota: tokio::sync::Mutex::new(loaded),
                    today_override: Mutex::new(None),
                    cache: Mutex::new(PreviewCache::new()),
                    inflight: tokio::sync::Mutex::new(HashMap::new()),
                    settings: Mutex::new(settings),
                });
                guard.insert(key, Arc::downgrade(&shared));
                shared
            }
        };
        Ok(Self {
            http,
            base_url: base_url.into(),
            state,
        })
    }

    /// Replace the API key, keeping the rest of the settings.
    pub fn configure(&self, api_key: &str) {
        let mut settings = self.lock_settings();
        settings.api_key = api_key.to_owned();
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
    pub fn is_cached(&self, artist: &str, title: &str, kind: SearchKind) -> bool {
        let key = SearchKey {
            kind,
            artist: artist.to_lowercase(),
            title: title.to_lowercase(),
        };
        self.lock_cache().contains(&key)
    }

    /// Cached flags for track pairs, keyed `artist|track` lowercased, kept
    /// from v2's `are_cached`.
    pub fn are_cached(&self, pairs: &[(&str, &str)]) -> HashMap<String, bool> {
        pairs
            .iter()
            .map(|(artist, track)| {
                let key = format!("{}|{}", artist.to_lowercase(), track.to_lowercase());
                (key, self.is_cached(artist, track, SearchKind::Track))
            })
            .collect()
    }

    /// Album preview search: the query gains a `full album` suffix.
    pub async fn search_video(
        &self,
        artist: &str,
        album: &str,
    ) -> Result<Option<String>, YoutubeError> {
        self.search(SearchKind::Album, artist, album).await
    }

    /// Track preview search: the query is the bare artist + title.
    pub async fn search_track(
        &self,
        artist: &str,
        track: &str,
    ) -> Result<Option<String>, YoutubeError> {
        self.search(SearchKind::Track, artist, track).await
    }

    /// Probe an API key against a known video. Never touches the quota: it
    /// returns a plain verdict pair, kept from v2.
    pub async fn verify_api_key(&self, api_key: &str) -> (bool, String) {
        let url = join(&self.base_url, YOUTUBE_VERIFY_PATH);
        let params = [
            ("part", "id".to_owned()),
            ("id", VERIFY_VIDEO_ID.to_owned()),
            ("key", api_key.to_owned()),
        ];
        let response = match self
            .http
            .get(url)
            .query(&params)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return (
                    false,
                    format!("Connection error: {}", transport_kind(&error)),
                );
            }
        };
        match response.status().as_u16() {
            200 => (true, "YouTube API key is valid".to_owned()),
            403 => (
                false,
                "API key is invalid or YouTube Data API is not enabled".to_owned(),
            ),
            status => (false, format!("Unexpected response: {status}")),
        }
    }

    /// Test seam: pin the client's UTC date (rollover tests). `None`
    /// restores the real clock. Applies to every instance on this path.
    pub fn set_today_override(&self, today: Option<String>) {
        let mut guard = self
            .state
            .today_override
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *guard = today;
    }

    /// One search with singleflight: concurrent identical searches share one
    /// dispatch, so they cost one HTTP call and one quota unit.
    async fn search(
        &self,
        kind: SearchKind,
        artist: &str,
        title: &str,
    ) -> Result<Option<String>, YoutubeError> {
        let key = SearchKey {
            kind,
            artist: artist.to_lowercase(),
            title: title.to_lowercase(),
        };
        if let Some(hit) = self.cache_get(&key) {
            return Ok(hit);
        }
        let slot = {
            let mut inflight = self.state.inflight.lock().await;
            if let Some(slot) = inflight.get(&key) {
                Arc::clone(slot)
            } else {
                let slot = Arc::new(tokio::sync::Mutex::new(()));
                inflight.insert(key.clone(), Arc::clone(&slot));
                slot
            }
        };
        let _guard = slot.lock().await;
        if let Some(hit) = self.cache_get(&key) {
            self.release_slot(&key, &slot).await;
            return Ok(hit);
        }
        let outcome = self.dispatch(&key, artist, title).await;
        self.release_slot(&key, &slot).await;
        outcome
    }

    /// Reserve-then-fetch. The reservation lands in the file before any HTTP;
    /// anything that stops the dispatch refunds it, while a dispatched call
    /// stays charged whatever the outcome.
    async fn dispatch(
        &self,
        key: &SearchKey,
        artist: &str,
        title: &str,
    ) -> Result<Option<String>, YoutubeError> {
        let settings = self.snapshot_settings();
        if settings.usable_key().is_none() {
            return Err(YoutubeError::NotConfigured(
                "youtube API search is disabled or not configured".to_owned(),
            ));
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
        let query = match key.kind {
            SearchKind::Album => format!("{artist} {title} full album"),
            SearchKind::Track => format!("{artist} {title}"),
        };
        let url = join(&self.base_url, YOUTUBE_SEARCH_PATH);
        let params = [
            ("part", "id".to_owned()),
            ("type", "video".to_owned()),
            ("maxResults", "1".to_owned()),
            ("q", query),
            ("key", api_key),
        ];
        let response = self
            .http
            .get(url)
            .query(&params)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| YoutubeError::Transport(transport_kind(&error).to_owned()))?;
        let status = response.status().as_u16();
        if status == 429 {
            return Err(YoutubeError::UpstreamRateLimited);
        }
        if !(200..300).contains(&status) {
            return Err(YoutubeError::Api { status });
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| YoutubeError::Transport(transport_kind(&error).to_owned()))?;
        let video_id = match serde_json::from_slice::<YouTubeSearchResponse>(&body) {
            Ok(decoded) => match decoded.items.into_iter().next() {
                None => None,
                Some(item) => match item.id.and_then(|id| id.video_id) {
                    Some(video_id) => Some(video_id),
                    None => {
                        return Err(YoutubeError::InvalidPayload(
                            "youtube search returned an invalid video".to_owned(),
                        ));
                    }
                },
            },
            Err(error) => {
                return Err(YoutubeError::InvalidPayload(format!(
                    "youtube search returned an invalid payload: {error}"
                )));
            }
        };
        self.cache_put(key, video_id.clone());
        Ok(video_id)
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

    /// Today's UTC date, or the pinned test date when set.
    fn today(&self) -> String {
        self.state
            .today_override
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
            .unwrap_or_else(utc_today)
    }

    /// Drop a finished in-flight slot, but only when it is still ours (a
    /// newer search may already have installed its own).
    async fn release_slot(&self, key: &SearchKey, slot: &Arc<tokio::sync::Mutex<()>>) {
        let mut inflight = self.state.inflight.lock().await;
        let owned = inflight
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, slot));
        if owned {
            inflight.remove(key);
        }
    }

    fn cache_get(&self, key: &SearchKey) -> Option<Option<String>> {
        self.lock_cache().get(key)
    }

    fn cache_put(&self, key: &SearchKey, value: Option<String>) {
        self.lock_cache().put(key, value);
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

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, PreviewCache> {
        self.state
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Registry key for a quota path: absolute when the working directory is
/// known, as-given otherwise. Mirrors v2's resolved-path sharing.
fn registry_key(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
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
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".youtube-quota-{}-{stamp}-{seq}.tmp",
        std::process::id()
    ));
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

/// Today's UTC date as YYYY-MM-DD, without a date dependency: unix days to a
/// civil date via Howard Hinnant's algorithm.
pub fn utc_today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since the unix epoch to a (year, month, day) civil date.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 - day_of_era / 36_524 + day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as u32;
    let year = (if month <= 2 { year + 1 } else { year }) as i32;
    (year, month, day)
}

/// Join a base URL and a path without doubling the slash.
fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// Short transport failure kind. Only the kind is kept, never the URL, the
/// same shape as v2's exception-type-only message.
fn transport_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    }
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
