//! The discover page: shelves built in the background, served from the
//! last good build, kept fresh while people use it.
//!
//! A request never waits for a build. It gets the newest page there is
//! (memory first, then the snapshot saved in SQLite, which survives a
//! restart) and, when that page is more than five minutes old, starts a
//! rebuild in the background and says so (`refreshing`), so the page
//! polls until the new one lands. With no page at all it starts the first
//! build and answers with an empty, `loading` page. An empty or failed
//! build never replaces a good page; it is remembered for five minutes
//! so the page settles instead of rebuilding on every poll.
//!
//! The warm cycle keeps the pages people actually use fresh: the page
//! records which features a user opened (`discovery_activity`), and the
//! demand tick rebuilds those (the discover page and the queue deck)
//! every six hours while they stay in use, retrying failures sooner.
//! Each published page queues a background warm of its covers and artist
//! images.

pub mod build;
pub mod library;
pub mod live_sources;
pub mod memo;
pub mod picks;
pub mod shelves;
pub mod sources;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::reads::discover::adapters::content::IntegrationSettings;
use crate::reads::discover::adapters::queue::QueueSettings;
use crate::reads::discover::adapters::queue::store::QueueDb;
use crate::reads::discover::models::{
    ChartSection, DiscoverActivityResponse, DiscoverResponse, IntegrationStatus, QueueBuildStatus,
    SectionItem,
};
use crate::reads::discover::ports::{BoxFuture, ProviderFailure, QueueStore, QueueTrigger};
use crate::reads::platform::artwork::warm::{CoverWarm, WarmTargets};
use build::{Builder, PageSettings};
use droppedneedle_kernel::runtime_config::{Live, Plain, secret_sections::AdvancedSettings};
use library::LibraryReads;
use memo::BuildMemo;
use sources::PageSources;
use store::{ActivityRow, PageDb, snapshot_key};

/// How long a page in memory is served before it is reloaded.
const MEMORY_SECS: f64 = 43_200.0;
/// A page older than this is rebuilt in the background on the next visit.
const STALE_SECS: f64 = 300.0;
/// Bound on one warm-cycle feature.
const FEATURE_TIMEOUT: Duration = Duration::from_secs(300);
/// When a warmed feature is due again.
const WARM_AGAIN_SECS: f64 = 6.0 * 3600.0;
/// Retry after a feature that did not finish.
const RETRY_SECS: f64 = 90.0;
/// Retry after a feature that failed outright.
const FAILED_RETRY_SECS: f64 = 900.0;
/// How often the queue's build is checked while the warm cycle waits.
const QUEUE_POLL: Duration = Duration::from_secs(2);

/// Genre artwork schema the frontend expects (v2 default).
pub const GENRE_ARTWORK_SCHEMA: &str = "v2";

/// A page with no shelves.
pub fn empty_page() -> DiscoverResponse {
    DiscoverResponse {
        because_you_listen_to: Vec::new(),
        discover_queue_enabled: true,
        fresh_releases: None,
        missing_essentials: None,
        rediscover: None,
        artists_you_might_like: None,
        popular_in_your_genres: None,
        genre_list: None,
        globally_trending: None,
        weekly_exploration: None,
        integration_status: None,
        service_prompts: Vec::new(),
        genre_artwork: HashMap::new(),
        genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
        lastfm_weekly_artist_chart: None,
        lastfm_weekly_album_chart: None,
        lastfm_recent_scrobbles: None,
        daily_mixes: Vec::new(),
        radio_sections: Vec::new(),
        top_picks: None,
        listeners_like_you: None,
        anniversaries: None,
        new_from_followed: None,
        unexplored_genres: None,
        generated_at: None,
        refresh_started_at: None,
        section_status: HashMap::new(),
        refreshing: false,
        service_status: None,
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// Decode a saved page. v2 saved its timestamps as floats; they are
/// rounded to whole seconds.
fn decode(payload: &[u8]) -> Option<DiscoverResponse> {
    let mut value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    if let Some(object) = value.as_object_mut() {
        for key in ["generated_at", "refresh_started_at"] {
            if let Some(seconds) = object.get(key).and_then(serde_json::Value::as_f64) {
                object.insert(key.to_owned(), serde_json::json!(seconds as i64));
            }
        }
    }
    match serde_json::from_value(value) {
        Ok(page) => Some(page),
        Err(error) => {
            tracing::warn!(%error, "ignoring a saved discover page that does not decode");
            None
        }
    }
}

/// One page in memory.
#[derive(Clone)]
struct Cached {
    page: DiscoverResponse,
    expires: f64,
    meaningful: bool,
}

/// Another page the warm cycle keeps fresh (home). It is handed the
/// discover page, for what it shows from it.
pub trait FeatureWarmer: Send + Sync {
    /// Build the user's page now; true when a new one was published.
    fn warm<'a>(&'a self, user_id: &'a str, discover: &'a LiveDiscover) -> BoxFuture<'a, bool>;
}

/// What the page keeps between requests.
#[derive(Default)]
struct State {
    pages: HashMap<String, Cached>,
    /// When each key was last built (or a build was last attempted).
    built_at: HashMap<String, f64>,
    /// Users with a build running.
    building: HashSet<String>,
    /// When the running or requested rebuild started, per user.
    refresh_started: HashMap<String, f64>,
    /// Keys whose saved page was marked stale by an invalidation.
    stale_keys: HashSet<String>,
    /// Users the warm cycle is working on.
    warming: HashSet<String>,
}

struct Inner {
    sources: Arc<dyn PageSources>,
    queue_db: QueueDb,
    library: LibraryReads,
    db: PageDb,
    memo: BuildMemo,
    advanced: Live<AdvancedSettings>,
    integrations: IntegrationSettings,
    queues: Arc<dyn QueueStore>,
    warmer_enabled: bool,
    warm: Option<CoverWarm>,
    home: Option<Arc<dyn FeatureWarmer>>,
    state: Mutex<State>,
}

/// The live discover page.
#[derive(Clone)]
pub struct LiveDiscover {
    inner: Arc<Inner>,
}

/// Everything the page reads from.
pub struct PageInputs {
    /// Provider reads.
    pub sources: Arc<dyn PageSources>,
    /// The queue tables.
    pub queue_db: QueueDb,
    /// The page tables.
    pub db: PageDb,
    /// The discover tuning, read per build.
    pub advanced: Live<AdvancedSettings>,
    /// The integration flags, read per build.
    pub integrations: IntegrationSettings,
    /// The queue deck, which the warm cycle also rebuilds.
    pub queues: Arc<dyn QueueStore>,
    /// The deployment kill switch for the warm cycle.
    pub warmer_enabled: bool,
    /// Where a published page queues its image warm, when anywhere.
    pub warm: Option<CoverWarm>,
}

/// Clears a user's "building" mark however the build ends.
struct BuildGuard {
    inner: Arc<Inner>,
    user_id: String,
    key: String,
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.building.remove(&self.user_id);
            state.refresh_started.remove(&self.user_id);
            state.stale_keys.remove(&self.key);
            state.built_at.insert(self.key.clone(), now_secs());
        }
    }
}

/// Clears a user's warm-cycle mark however the cycle ends.
struct WarmGuard {
    inner: Arc<Inner>,
    user_id: String,
}

impl Drop for WarmGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.warming.remove(&self.user_id);
        }
    }
}

impl LiveDiscover {
    /// Build the page over its inputs.
    pub fn new(inputs: PageInputs) -> Self {
        Self::over(inputs, None)
    }

    /// Build the page over its inputs; its warm cycle rebuilds `home` too.
    pub fn with_home(inputs: PageInputs, home: Arc<dyn FeatureWarmer>) -> Self {
        Self::over(inputs, Some(home))
    }

    fn over(inputs: PageInputs, home: Option<Arc<dyn FeatureWarmer>>) -> Self {
        let library = LibraryReads::new(inputs.db.pool().clone());
        Self {
            inner: Arc::new(Inner {
                sources: inputs.sources,
                queue_db: inputs.queue_db,
                library,
                db: inputs.db,
                memo: BuildMemo::default(),
                advanced: inputs.advanced,
                integrations: inputs.integrations,
                queues: inputs.queues,
                warmer_enabled: inputs.warmer_enabled,
                warm: inputs.warm,
                home,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// The user's last built page, from memory or the saved snapshot,
    /// without starting a build (home's discover teaser).
    pub async fn cached_page(&self, user_id: &str) -> Option<DiscoverResponse> {
        let key = self.key_for(user_id).await;
        let now = now_secs();
        match self.memory(&key, now) {
            Some(page) => Some(page),
            None => self.load_saved(&key, now).await,
        }
    }

    async fn key_for(&self, user_id: &str) -> String {
        let user = self.inner.sources.user_music(user_id).await;
        snapshot_key(user_id, user.listenbrainz.is_some(), user.lastfm)
    }

    /// The newest page for the user, starting a background rebuild when
    /// there is none or it is more than five minutes old.
    pub async fn page(&self, user_id: &str, status: IntegrationStatus) -> DiscoverResponse {
        let key = self.key_for(user_id).await;
        let now = now_secs();
        let mut cached = self.memory(&key, now);
        if cached.is_none() {
            cached = self.load_saved(&key, now).await;
        }
        let (building, started) = self.building(user_id);
        let mut building = building;
        let (page, attempted_at) = {
            let state = self.lock();
            (
                cached,
                state.as_ref().and_then(|s| s.built_at.get(&key).copied()),
            )
        };
        let mut page = match page {
            Some(page) => {
                let built_at = attempted_at
                    .or(page.generated_at.map(|at| at as f64))
                    .unwrap_or(0.0);
                let stale = self.lock().is_some_and(|s| s.stale_keys.contains(&key));
                if !building && (stale || now - built_at > STALE_SECS) {
                    self.trigger_warm(user_id);
                    building = true;
                }
                let mut page = page;
                page.section_status = shelves::section_status(Some(&page), building);
                page
            }
            None => {
                let recently = attempted_at.is_some_and(|at| now - at <= STALE_SECS);
                if !building && !recently {
                    self.trigger_warm(user_id);
                    building = true;
                }
                let mut page = empty_page();
                page.service_prompts = shelves::service_prompts(
                    status.listenbrainz,
                    status.jellyfin,
                    status.download_client,
                    status.lastfm,
                );
                page.section_status = shelves::section_status(None, building);
                page
            }
        };
        page.refreshing = building;
        page.refresh_started_at = if building {
            self.building(user_id).1.or(started).map(|at| at as i64)
        } else {
            None
        };
        page.integration_status = Some(status);
        page
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, State>> {
        match self.inner.state.lock() {
            Ok(state) => Some(state),
            Err(_) => {
                tracing::error!("discover page state lock poisoned");
                None
            }
        }
    }

    fn building(&self, user_id: &str) -> (bool, Option<f64>) {
        self.lock().map_or((false, None), |state| {
            let started = state.refresh_started.get(user_id).copied();
            (
                state.building.contains(user_id) || started.is_some(),
                started,
            )
        })
    }

    fn memory(&self, key: &str, now: f64) -> Option<DiscoverResponse> {
        let state = self.lock()?;
        state
            .pages
            .get(key)
            .filter(|cached| cached.expires > now)
            .map(|cached| cached.page.clone())
    }

    async fn load_saved(&self, key: &str, now: f64) -> Option<DiscoverResponse> {
        let saved = match self.inner.db.load(key).await {
            Ok(saved) => saved?,
            Err(cause) => {
                tracing::warn!(%cause, "saved discover page unreadable; building a new one");
                return None;
            }
        };
        let page = decode(&saved.payload)?;
        let mut state = self.lock()?;
        if saved.stale {
            state.stale_keys.insert(key.to_owned());
        }
        state
            .built_at
            .entry(key.to_owned())
            .or_insert(saved.saved_at);
        state.pages.insert(
            key.to_owned(),
            Cached {
                page: page.clone(),
                expires: now + MEMORY_SECS,
                meaningful: true,
            },
        );
        Some(page)
    }

    /// Start a background rebuild for the user unless one is running.
    fn trigger_warm(&self, user_id: &str) {
        {
            let Some(mut state) = self.lock() else {
                return;
            };
            if state.building.contains(user_id) {
                return;
            }
            state
                .refresh_started
                .entry(user_id.to_owned())
                .or_insert_with(now_secs);
        }
        let page = self.clone();
        let user_id = user_id.to_owned();
        tokio::spawn(async move {
            page.warm(&user_id).await;
            if let Some(mut state) = page.lock()
                && !state.building.contains(&user_id)
            {
                state.refresh_started.remove(&user_id);
            }
        });
    }

    /// Ask for a fresh page now: forget the memoised shelves and rebuild.
    pub async fn refresh(&self, user_id: &str) {
        let key = self.key_for(user_id).await;
        {
            let Some(mut state) = self.lock() else {
                return;
            };
            if state.building.contains(user_id) {
                return;
            }
            state.built_at.remove(&key);
        }
        self.inner.memo.clear_user(user_id);
        self.trigger_warm(user_id);
    }

    fn settings(&self, download_client: bool) -> PageSettings {
        let advanced = self.inner.advanced.get().unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot read advanced settings; discover uses defaults");
            Arc::new(Plain::default())
        });
        PageSettings {
            picks_count: advanced.discover_picks_count.clamp(4, 30) as usize,
            genre_weight: advanced
                .discover_picks_genre_affinity_weight
                .clamp(0.0, 1.0)
                / 2.0,
            queue: QueueSettings::from_advanced(&advanced),
            download_client,
        }
    }

    /// Build the user's page now and keep it when it has anything to show.
    /// Returns whether a new page was published.
    pub async fn warm(&self, user_id: &str) -> bool {
        let key = self.key_for(user_id).await;
        {
            let Some(mut state) = self.lock() else {
                return false;
            };
            if !state.building.insert(user_id.to_owned()) {
                return false;
            }
            state
                .refresh_started
                .entry(user_id.to_owned())
                .or_insert_with(now_secs);
        }
        let _guard = BuildGuard {
            inner: self.inner.clone(),
            user_id: user_id.to_owned(),
            key: key.clone(),
        };
        let source = self.inner.sources.source_context();
        let download_client = self.inner.integrations.download_client_ready();
        let now = now_secs();
        let (today, year) = today(now);
        let builder = Builder {
            sources: self.inner.sources.as_ref(),
            queue_db: &self.inner.queue_db,
            library: &self.inner.library,
            memo: &self.inner.memo,
            settings: self.settings(download_client),
            today,
            year,
            now,
        };
        let built = builder.build(user_id).await;
        if self.inner.sources.source_context() != source {
            tracing::info!("MusicBrainz source changed during a discover build; dropping it");
            return false;
        }
        let mut page = built.page;
        let mut degraded = built.degraded;
        if self.inner.sources.listenbrainz_popularity_down() {
            degraded
                .entry("listenbrainz".to_owned())
                .or_insert_with(|| "degraded".to_owned());
        }
        let generated_at = now_secs();
        page.service_status = (!degraded.is_empty()).then_some(degraded);
        page.generated_at = Some(generated_at as i64);
        page.refresh_started_at = None;
        page.section_status = shelves::section_status(Some(&page), false);
        page.refreshing = false;
        if !shelves::has_content(&page) {
            tracing::warn!(user = %user_id, "discover build found nothing to show; keeping the previous page");
            if let Some(mut state) = self.lock() {
                let keep = state
                    .pages
                    .get(&key)
                    .is_some_and(|cached| cached.meaningful && cached.expires > generated_at);
                if !keep {
                    state.pages.insert(
                        key.clone(),
                        Cached {
                            page,
                            expires: generated_at + STALE_SECS,
                            meaningful: false,
                        },
                    );
                }
            }
            return false;
        }
        if let Some(mut state) = self.lock() {
            state.pages.insert(
                key.clone(),
                Cached {
                    page: page.clone(),
                    expires: generated_at + MEMORY_SECS,
                    meaningful: true,
                },
            );
        }
        match serde_json::to_vec(&page) {
            Ok(payload) => {
                if let Err(cause) = self
                    .inner
                    .db
                    .save(&key, user_id, payload, generated_at)
                    .await
                {
                    tracing::warn!(%cause, "could not save the discover page; the memory copy stays");
                }
            }
            Err(error) => tracing::warn!(%error, "could not encode the discover page for saving"),
        }
        if let Some(warm) = &self.inner.warm {
            warm.queue("discover", warm_targets(&page));
        }
        true
    }

    /// Record that the user used a discover feature and start warming
    /// what is due for them.
    pub async fn record_activity(
        &self,
        user_id: &str,
        feature: &str,
        artist_mbid: Option<&str>,
        section: Option<&str>,
        provider: Option<&str>,
    ) -> Result<DiscoverActivityResponse, ProviderFailure> {
        let source = self.inner.sources.source_context();
        self.inner
            .db
            .record_activity(
                ActivityRow {
                    user_id: user_id.to_owned(),
                    feature: feature.to_owned(),
                    artist_mbid: artist_mbid.unwrap_or_default().to_owned(),
                    section: section.unwrap_or_default().to_owned(),
                    provider: provider.unwrap_or_default().to_owned(),
                    source: source.key(),
                },
                now_secs(),
            )
            .await
            .map_err(ProviderFailure::failed)?;
        let page = self.clone();
        let user_id = user_id.to_owned();
        tokio::spawn(async move { page.run_due(Some(&user_id)).await });
        Ok(DiscoverActivityResponse {
            source_mode: source.mode,
            source_id: source.id,
            generation: source.generation,
        })
    }

    /// One warm-cycle pass (v2 `run_due_tick`): take the first user with
    /// activity due under the current source and warm their features.
    pub async fn run_due(&self, only_user: Option<&str>) {
        if !self.inner.warmer_enabled {
            return;
        }
        let source = self.inner.sources.source_context();
        let rows = match self
            .inner
            .db
            .due_activity(&source.key(), now_secs(), only_user)
            .await
        {
            Ok(rows) => rows,
            Err(cause) => {
                tracing::warn!(%cause, "discover activity unreadable; skipping this warm pass");
                return;
            }
        };
        let Some(user_id) = rows.first().map(|row| row.user_id.clone()) else {
            return;
        };
        {
            let Some(mut state) = self.lock() else {
                return;
            };
            if !state.warming.insert(user_id.clone()) {
                return;
            }
        }
        let _guard = WarmGuard {
            inner: self.inner.clone(),
            user_id: user_id.clone(),
        };
        match self.inner.db.user_exists(&user_id).await {
            Ok(true) => {}
            Ok(false) => {
                if let Err(cause) = self.inner.db.forget_user(&user_id).await {
                    tracing::warn!(%cause, "could not forget a deleted user's discover rows");
                }
                return;
            }
            Err(cause) => {
                tracing::warn!(%cause, "user check failed; skipping this warm pass");
                return;
            }
        }
        for row in rows.iter().filter(|row| row.user_id == user_id) {
            if self.inner.sources.source_context() != source {
                break;
            }
            let (success, retry) = self.warm_feature(row).await;
            if let Err(cause) = self
                .inner
                .db
                .finish_activity(row, now_secs(), success, retry)
                .await
            {
                tracing::warn!(%cause, "could not reschedule discover activity");
            }
        }
    }

    /// Warm one feature; returns (published, seconds until next try).
    async fn warm_feature(&self, row: &ActivityRow) -> (bool, f64) {
        let user_id = row.user_id.as_str();
        let outcome = match row.feature.as_str() {
            "discover" => tokio::time::timeout(FEATURE_TIMEOUT, self.warm(user_id)).await,
            "queue" => {
                if !self.settings(false).queue.warm_cycle_build {
                    return (false, RETRY_SECS);
                }
                tokio::time::timeout(FEATURE_TIMEOUT, self.warm_queue(user_id)).await
            }
            "home" => match &self.inner.home {
                Some(home) => tokio::time::timeout(FEATURE_TIMEOUT, home.warm(user_id, self)).await,
                None => return (false, WARM_AGAIN_SECS),
            },
            // Artist pages have no background warmer in this version;
            // check back in six hours.
            other => {
                tracing::debug!(feature = other, "no warmer for this discover feature yet");
                return (false, WARM_AGAIN_SECS);
            }
        };
        match outcome {
            Ok(true) => (true, WARM_AGAIN_SECS),
            Ok(false) => (false, RETRY_SECS),
            Err(_) => {
                tracing::warn!(feature = %row.feature, "discover warm timed out");
                (false, FAILED_RETRY_SECS)
            }
        }
    }

    /// Ask the queue for a scheduled build and wait for it to finish.
    async fn warm_queue(&self, user_id: &str) -> bool {
        let started = self
            .inner
            .queues
            .start_build(user_id, QueueTrigger::Scheduled)
            .await;
        match started.action.as_str() {
            "disabled" => return false,
            "already_ready" => return true,
            _ => {}
        }
        loop {
            tokio::time::sleep(QUEUE_POLL).await;
            let status = self.inner.queues.status(user_id).await;
            match status.status {
                QueueBuildStatus::Building => continue,
                QueueBuildStatus::Ready => return true,
                QueueBuildStatus::Idle | QueueBuildStatus::Error => return false,
            }
        }
    }
}

/// The covers and artist images a discover page shows, top to bottom.
fn warm_targets(page: &DiscoverResponse) -> WarmTargets {
    let mut targets = WarmTargets::default();
    if let Some(picks) = &page.top_picks {
        for pick in &picks.items {
            targets.cover(pick.album.mbid.as_deref());
        }
    }
    if let Some(lounge) = &page.listeners_like_you {
        add_section(&mut targets, lounge);
    }
    if let Some(weekly) = &page.weekly_exploration {
        for track in &weekly.tracks {
            targets.cover(track.release_group_mbid.as_deref());
        }
    }
    let made = page.daily_mixes.iter().chain(&page.radio_sections);
    let because = page
        .because_you_listen_to
        .iter()
        .map(|shelf| &shelf.section);
    let rest = [
        &page.fresh_releases,
        &page.missing_essentials,
        &page.rediscover,
        &page.anniversaries,
        &page.new_from_followed,
        &page.lastfm_weekly_album_chart,
        &page.lastfm_recent_scrobbles,
        &page.artists_you_might_like,
        &page.popular_in_your_genres,
        &page.globally_trending,
        &page.lastfm_weekly_artist_chart,
    ];
    for section in made.chain(because).chain(rest.into_iter().flatten()) {
        add_section(&mut targets, section);
    }
    targets
}

/// A shelf's album covers and artist images.
pub(crate) fn add_section(targets: &mut WarmTargets, section: &ChartSection) {
    for item in &section.items {
        match item {
            SectionItem::Album(album) => targets.cover(album.mbid.as_deref()),
            SectionItem::Artist(artist) => targets.artist(artist.mbid.as_deref()),
            SectionItem::Track(_) | SectionItem::Genre(_) => {}
        }
    }
}

/// Today's date (`YYYY-MM-DD`) and year in UTC.
fn today(now: f64) -> (String, i64) {
    match time::OffsetDateTime::from_unix_timestamp(now as i64) {
        Ok(at) => {
            let date = at.date();
            (
                format!(
                    "{:04}-{:02}-{:02}",
                    date.year(),
                    u8::from(date.month()),
                    date.day()
                ),
                i64::from(date.year()),
            )
        }
        Err(_) => ("1970-01-01".to_owned(), 1970),
    }
}
