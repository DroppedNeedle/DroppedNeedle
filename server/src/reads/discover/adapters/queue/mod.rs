//! The discover queue deck (v2 `DiscoverQueueManager` and the
//! `services/discover` queue services).
//!
//! Each user has at most one background build at a time. A finished deck
//! is kept in memory and saved to `discovery_snapshots`, so it survives a
//! restart; it reads as stale once older than the queue TTL or after a
//! library change. The warm cycle may build decks too, but any request
//! from the user replaces a warm-cycle build that is still running.
//! Ignores are durable and every later deck skips them.
//!
//! The pieces: [`sources`] is the provider boundary, [`build`] assembles
//! a deck from it, [`select`] holds the pure picking logic, [`cards`] the
//! per-card details, [`store`] the tables, and [`live_sources`] the
//! production provider reads.

pub mod build;
pub mod cards;
pub mod live_sources;
pub mod select;
pub mod sources;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::ProviderCache;
use crate::providers::singleflight::Singleflight;
use crate::reads::discover::models::{
    DiscoverQueuePreview, DiscoverQueueStatusResponse, IgnoredRelease, QueueEnrichment,
    QueueGenerateResponse, QueueIgnoreRequest, QueueItem, QueueItemLight,
};
use crate::reads::discover::ports::{
    BoxFuture, ProviderFailure, QueueDeck, QueueStore, QueueTrigger, YouTubeSource,
};
use crate::runtime_config::secret_sections::AdvancedSettings;

use build::{BuildContext, build_deck};
use sources::QueueSources;
use store::QueueDb;

/// The queue settings one call works with, read fresh each time.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSettings {
    /// Cards per deck.
    pub queue_size: usize,
    /// Seconds before a deck reads as stale.
    pub ttl_secs: f64,
    /// Seed artists per deck.
    pub seed_artists: usize,
    /// Trending wildcards per deck.
    pub wildcard_slots: usize,
    /// Similar artists read per seed.
    pub similar_artists_limit: usize,
    /// Albums read per similar artist.
    pub albums_per_similar: usize,
    /// How long card details stay cached.
    pub enrich_ttl: Duration,
    /// MusicBrainz lookups one Last.fm album batch may spend.
    pub lastfm_mbid_max_lookups: usize,
    /// Whether the warm cycle may build decks.
    pub warm_cycle_build: bool,
    /// Days an ignore is kept.
    pub ignored_retention_days: i64,
    /// Seconds between ignore-ledger prunes.
    pub prune_interval_secs: f64,
}

fn bounded(value: i64, min: i64, max: i64) -> usize {
    value.clamp(min, max) as usize
}

impl QueueSettings {
    /// The queue's view of the advanced settings, clamped to the ranges
    /// the settings form allows.
    pub fn from_advanced(advanced: &AdvancedSettings) -> Self {
        Self {
            queue_size: bounded(advanced.discover_queue_size, 1, 20),
            ttl_secs: advanced.discover_queue_ttl.max(60) as f64,
            seed_artists: bounded(advanced.discover_queue_seed_artists, 1, 10),
            wildcard_slots: bounded(advanced.discover_queue_wildcard_slots, 0, 10),
            similar_artists_limit: bounded(advanced.discover_queue_similar_artists_limit, 5, 50),
            albums_per_similar: bounded(advanced.discover_queue_albums_per_similar, 1, 20),
            enrich_ttl: Duration::from_secs(
                advanced.discover_queue_enrich_ttl.clamp(60, 604_800) as u64
            ),
            lastfm_mbid_max_lookups: bounded(
                advanced.discover_queue_lastfm_mbid_max_lookups,
                1,
                50,
            ),
            warm_cycle_build: advanced.discover_queue_warm_cycle_build,
            ignored_retention_days: advanced.ignored_releases_retention_days.max(1),
            prune_interval_secs: (advanced.store_prune_interval_hours.max(1) * 3600) as f64,
        }
    }
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self::from_advanced(&AdvancedSettings::default())
    }
}

/// Reads the queue settings per call.
pub type SettingsFn = Arc<dyn Fn() -> QueueSettings + Send + Sync>;

/// The deck as saved, in v2's snapshot shape so migrated decks load.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedQueue {
    queue: SavedCards,
    built_at: f64,
    /// The MusicBrainz source the deck was built against. v2 decks carry
    /// none and are accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedCards {
    #[serde(default)]
    items: Vec<QueueItemLight>,
    #[serde(default)]
    queue_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Building,
    Ready,
    Error,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Building => "building",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

/// One user's queue state.
struct UserState {
    source: String,
    phase: Phase,
    deck: Option<SavedQueue>,
    saved_stale: bool,
    error: Option<String>,
    scheduled: bool,
    run: u64,
    task: Option<tokio::task::AbortHandle>,
    loaded: bool,
}

impl UserState {
    fn new(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            phase: Phase::Idle,
            deck: None,
            saved_stale: false,
            error: None,
            scheduled: false,
            run: 0,
            task: None,
            loaded: false,
        }
    }

    fn stop_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    fn is_stale(&self, settings: &QueueSettings, now: f64) -> bool {
        match &self.deck {
            None => true,
            Some(deck) => self.saved_stale || now - deck.built_at > settings.ttl_secs,
        }
    }

    fn status(&self, settings: &QueueSettings, now: f64) -> DiscoverQueueStatusResponse {
        let mut status = DiscoverQueueStatusResponse {
            status: self.phase.as_str().to_owned(),
            queue_id: None,
            item_count: None,
            built_at: None,
            stale: None,
            error: None,
        };
        match (self.phase, &self.deck) {
            (Phase::Ready, Some(deck)) => {
                status.queue_id = Some(deck.queue.queue_id.clone());
                status.item_count = Some(deck.queue.items.len() as i64);
                status.built_at = Some(deck.built_at as i64);
                status.stale = Some(self.is_stale(settings, now));
            }
            (Phase::Error, _) => status.error = self.error.clone(),
            _ => {}
        }
        status
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

fn to_deck(saved: &SavedQueue) -> QueueDeck {
    QueueDeck {
        queue_id: saved.queue.queue_id.clone(),
        items: saved
            .queue
            .items
            .iter()
            .cloned()
            .map(QueueItem::Light)
            .collect(),
    }
}

fn generate_response(action: &str, status: DiscoverQueueStatusResponse) -> QueueGenerateResponse {
    QueueGenerateResponse {
        action: action.to_owned(),
        status: status.status,
        queue_id: status.queue_id,
        item_count: status.item_count,
        built_at: status.built_at,
        stale: status.stale,
        error: status.error,
    }
}

struct Inner {
    sources: Arc<dyn QueueSources>,
    db: QueueDb,
    youtube: Arc<dyn YouTubeSource>,
    cache: Arc<dyn ProviderCache>,
    settings: SettingsFn,
    states: Mutex<HashMap<String, UserState>>,
    last_prune: Mutex<f64>,
    enrichments: Singleflight<QueueEnrichment, String>,
}

/// The live queue deck.
#[derive(Clone)]
pub struct LiveQueue {
    inner: Arc<Inner>,
}

impl LiveQueue {
    /// A queue over `sources`, storing in `db`, previewing through
    /// `youtube`, caching card details in `cache`, and reading its
    /// settings through `settings` on every call.
    pub fn new(
        sources: Arc<dyn QueueSources>,
        db: QueueDb,
        youtube: Arc<dyn YouTubeSource>,
        cache: Arc<dyn ProviderCache>,
        settings: SettingsFn,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                sources,
                db,
                youtube,
                cache,
                settings,
                states: Mutex::new(HashMap::new()),
                last_prune: Mutex::new(0.0),
                enrichments: Singleflight::new(),
            }),
        }
    }
}

impl Inner {
    fn states(&self) -> MutexGuard<'_, HashMap<String, UserState>> {
        // A panic while the lock was held cannot leave a state half-written
        // in a way later calls trip over, so a poisoned lock is reused.
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The user's state for the current MusicBrainz source. A source
    /// switch starts the user over, as v2 did.
    fn state<'m>(
        states: &'m mut HashMap<String, UserState>,
        user_id: &str,
        source: &str,
    ) -> &'m mut UserState {
        let state = states
            .entry(user_id.to_owned())
            .or_insert_with(|| UserState::new(source));
        if state.source != source {
            state.stop_task();
            *state = UserState::new(source);
        }
        state
    }

    /// Load the user's saved deck once per source.
    async fn ensure_loaded(&self, user_id: &str) {
        let source = self.sources.source_key();
        {
            let mut states = self.states();
            if Self::state(&mut states, user_id, &source).loaded {
                return;
            }
        }
        let saved = self.db.load_deck(user_id).await;
        let mut states = self.states();
        let state = Self::state(&mut states, user_id, &source);
        if state.loaded {
            return;
        }
        state.loaded = true;
        let saved = match saved {
            Ok(Some(saved)) => saved,
            Ok(None) => return,
            Err(cause) => {
                tracing::warn!(%cause, "saved queue deck unreadable; building a new one");
                return;
            }
        };
        let deck: SavedQueue = match serde_json::from_slice(&saved.payload) {
            Ok(deck) => deck,
            Err(error) => {
                tracing::warn!(%error, "ignoring a saved queue deck that does not decode");
                return;
            }
        };
        if deck.source.as_deref().is_some_and(|built| built != source) {
            return;
        }
        if state.deck.is_none() && state.phase != Phase::Building {
            state.deck = Some(deck);
            state.saved_stale = saved.stale;
            state.phase = Phase::Ready;
        }
    }

    async fn status(&self, user_id: &str) -> DiscoverQueueStatusResponse {
        self.ensure_loaded(user_id).await;
        let settings = (self.settings)();
        let source = self.sources.source_key();
        let mut states = self.states();
        Self::state(&mut states, user_id, &source).status(&settings, now_secs())
    }

    async fn current(&self, user_id: &str) -> Option<QueueDeck> {
        self.ensure_loaded(user_id).await;
        let source = self.sources.source_key();
        let mut states = self.states();
        Self::state(&mut states, user_id, &source)
            .deck
            .as_ref()
            .map(to_deck)
    }

    async fn start_build(
        self: &Arc<Self>,
        user_id: &str,
        trigger: QueueTrigger,
    ) -> QueueGenerateResponse {
        self.ensure_loaded(user_id).await;
        let settings = (self.settings)();
        let source = self.sources.source_key();
        let scheduled = trigger == QueueTrigger::Scheduled;
        let force = trigger == QueueTrigger::Request { force: true };
        let now = now_secs();
        let mut states = self.states();
        let state = Self::state(&mut states, user_id, &source);
        if scheduled && !settings.warm_cycle_build {
            return generate_response("disabled", state.status(&settings, now));
        }
        if state.phase == Phase::Building && state.scheduled && !scheduled {
            // The person asking wins over the warm cycle.
            state.stop_task();
            state.phase = if state.deck.is_some() {
                Phase::Ready
            } else {
                Phase::Idle
            };
        }
        if state.phase == Phase::Building {
            return generate_response("already_building", state.status(&settings, now));
        }
        if !force && state.phase == Phase::Ready && !state.is_stale(&settings, now) {
            return generate_response("already_ready", state.status(&settings, now));
        }
        state.stop_task();
        state.phase = Phase::Building;
        state.error = None;
        state.scheduled = scheduled;
        state.run += 1;
        let run = state.run;
        let inner = Arc::clone(self);
        let user = user_id.to_owned();
        let built_for = source.clone();
        let task = tokio::spawn(async move { inner.run_build(user, built_for, run).await });
        state.task = Some(task.abort_handle());
        generate_response("started", state.status(&settings, now))
    }

    /// One background build. Its result lands only if no newer build or
    /// source switch replaced it meanwhile.
    async fn run_build(self: Arc<Self>, user_id: String, source: String, run: u64) {
        let settings = (self.settings)();
        self.prune_if_due(&settings).await;
        let ctx = BuildContext {
            sources: self.sources.as_ref(),
            db: &self.db,
            settings: &settings,
        };
        let outcome = build_deck(&ctx, &user_id, settings.queue_size).await;
        let saved = {
            let mut states = self.states();
            let Some(state) = states.get_mut(&user_id) else {
                return;
            };
            if state.run != run || state.source != source {
                return;
            }
            state.task = None;
            if state.scheduled && !(self.settings)().warm_cycle_build {
                // The warm cycle was switched off while this ran.
                state.phase = if state.deck.is_some() {
                    Phase::Ready
                } else {
                    Phase::Idle
                };
                return;
            }
            match outcome {
                Ok(items) => {
                    let deck = SavedQueue {
                        queue: SavedCards {
                            items,
                            queue_id: uuid::Uuid::new_v4().to_string(),
                        },
                        built_at: now_secs(),
                        source: Some(source),
                    };
                    state.deck = Some(deck.clone());
                    state.saved_stale = false;
                    state.phase = Phase::Ready;
                    deck
                }
                Err(cause) => {
                    tracing::error!(%cause, "discover queue build failed");
                    state.phase = Phase::Error;
                    state.error =
                        Some("The queue could not be built. Try again in a moment.".to_owned());
                    return;
                }
            }
        };
        match serde_json::to_vec(&saved) {
            Ok(payload) => {
                if let Err(cause) = self.db.save_deck(&user_id, payload, saved.built_at).await {
                    tracing::warn!(%cause, "queue deck not saved; it lasts until restart");
                }
            }
            Err(error) => tracing::warn!(%error, "queue deck not encodable; not saved"),
        }
        // Fetch every card's listen count in one call now, so opening the
        // cards does not pay one paced ListenBrainz call each.
        let ids: Vec<String> = saved
            .queue
            .items
            .iter()
            .map(|item| item.release_group_mbid.clone())
            .collect();
        if let Err(cause) = self
            .sources
            .listenbrainz_listen_counts(&user_id, &ids)
            .await
        {
            tracing::debug!(%cause, "queue listen counts not prefetched");
        }
    }

    /// Drop ignores past their retention, at most once per prune interval.
    async fn prune_if_due(&self, settings: &QueueSettings) {
        let now = now_secs();
        let due = {
            let mut last = self
                .last_prune
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if now - *last < settings.prune_interval_secs {
                false
            } else {
                *last = now;
                true
            }
        };
        if !due {
            return;
        }
        let cutoff = now - (settings.ignored_retention_days as f64) * 86_400.0;
        match self.db.prune_ignored(cutoff).await {
            Ok(0) => {}
            Ok(dropped) => tracing::info!(dropped, "expired queue ignores removed"),
            Err(cause) => tracing::warn!(%cause, "queue ignores not pruned; trying next build"),
        }
    }

    async fn build_now(
        &self,
        user_id: &str,
        count: Option<usize>,
    ) -> Result<QueueDeck, ProviderFailure> {
        let settings = (self.settings)();
        let ctx = BuildContext {
            sources: self.sources.as_ref(),
            db: &self.db,
            settings: &settings,
        };
        let items = build_deck(&ctx, user_id, count.unwrap_or(settings.queue_size))
            .await
            .map_err(ProviderFailure::failed)?;
        Ok(QueueDeck {
            queue_id: uuid::Uuid::new_v4().to_string(),
            items: items.into_iter().map(QueueItem::Light).collect(),
        })
    }

    async fn enrich(
        self: &Arc<Self>,
        user_id: &str,
        release_group_mbid: &str,
    ) -> Result<QueueEnrichment, ProviderFailure> {
        let group = release_group_mbid.trim().to_ascii_lowercase();
        let key = format!(
            "discover_queue_enrich:{}:{group}",
            self.sources.source_key()
        );
        if let Some(hit) = self.cache.get_bytes(&key).await
            && let Ok(enrichment) = serde_json::from_slice::<QueueEnrichment>(&hit)
        {
            return Ok(enrichment);
        }
        let inner = Arc::clone(self);
        let user = user_id.to_owned();
        let flight_key = key.clone();
        let result = self
            .enrichments
            .run(&key, move || async move {
                let enrichment = cards::enrich(
                    inner.sources.as_ref(),
                    inner.youtube.as_ref(),
                    &user,
                    &group,
                )
                .await;
                let ttl = (inner.settings)().enrich_ttl;
                match serde_json::to_vec(&enrichment) {
                    Ok(bytes) => inner.cache.set_bytes(&flight_key, bytes, ttl).await,
                    Err(error) => tracing::debug!(%error, "queue card details not cached"),
                }
                Ok::<_, String>(enrichment)
            })
            .await;
        result
            .map(|enrichment| (*enrichment).clone())
            .map_err(|cause| ProviderFailure::failed(cause.to_string()))
    }
}

impl QueueStore for LiveQueue {
    fn current<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<QueueDeck>> {
        Box::pin(self.inner.current(user_id))
    }

    fn build_now<'a>(
        &'a self,
        user_id: &'a str,
        count: Option<usize>,
    ) -> BoxFuture<'a, Result<QueueDeck, ProviderFailure>> {
        Box::pin(self.inner.build_now(user_id, count))
    }

    fn status<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, DiscoverQueueStatusResponse> {
        Box::pin(self.inner.status(user_id))
    }

    fn start_build<'a>(
        &'a self,
        user_id: &'a str,
        trigger: QueueTrigger,
    ) -> BoxFuture<'a, QueueGenerateResponse> {
        Box::pin(self.inner.start_build(user_id, trigger))
    }

    fn ignore_release<'a>(
        &'a self,
        user_id: &'a str,
        release: &'a QueueIgnoreRequest,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async move {
            self.inner
                .db
                .ignore(user_id, release, now_secs())
                .await
                .map_err(ProviderFailure::failed)
        })
    }

    fn ignored<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<IgnoredRelease>, ProviderFailure>> {
        Box::pin(async move {
            self.inner
                .db
                .ignored(user_id)
                .await
                .map_err(ProviderFailure::failed)
        })
    }

    fn enrich<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<QueueEnrichment, ProviderFailure>> {
        Box::pin(self.inner.enrich(user_id, release_group_mbid))
    }

    fn preview<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverQueuePreview, ProviderFailure>> {
        Box::pin(async move {
            cards::preview(
                self.inner.sources.as_ref(),
                self.inner.youtube.as_ref(),
                release_group_mbid,
            )
            .await
        })
    }

    fn validate<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderFailure>> {
        Box::pin(async move {
            let owned = self
                .inner
                .db
                .owned(mbids)
                .await
                .map_err(ProviderFailure::failed)?;
            Ok(mbids
                .iter()
                .filter(|mbid| owned.contains(&mbid.trim().to_ascii_lowercase()))
                .cloned()
                .collect())
        })
    }
}
