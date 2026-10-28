//! The discover queue deck (v2 `DiscoverQueueManager` and the
//! `services/discover` queue services).
//!
//! Each user has at most one background build at a time. A finished deck
//! is a row of the page cache, its only copy, so it survives a restart;
//! it reads as stale once older than a day, and a library change leaves
//! it alone. The warm scheduler may build decks too, but any request from
//! the user replaces a warm build that is still running. Ignores are
//! durable and every later deck skips them.
//!
//! The pieces: [`build`] assembles a deck from the listening reads,
//! [`select`] holds the pure picking logic, [`cards`] the per-card
//! details and [`store`] the tables.

pub mod build;
pub mod cards;
pub mod select;
pub mod store;

use droppedneedle_providers::RequestPriority;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::reads::discover::cache::{PageKind, Saved, Timing, UserPageCache, Variant};
use crate::reads::discover::listening::{Ask, ListeningData};
use crate::reads::discover::models::{
    DiscoverQueuePreview, DiscoverQueueStatusResponse, IgnoredRelease, QueueAction,
    QueueBuildStatus, QueueEnrichment, QueueGenerateResponse, QueueIgnoreRequest, QueueItemLight,
};
use crate::reads::discover::ports::{BoxFuture, ProviderFailure, QueueDeck, QueueTrigger};
use crate::reads::discover::scheduler::{Feature, Warm};
use crate::reads::discover::youtube::LiveYouTube;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};

use crate::reads::cache_hold::CacheHold;

use build::{BuildContext, build_deck};
use store::QueueDb;

/// The queue settings one call works with, read fresh each time.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSettings {
    /// Cards per deck.
    pub queue_size: usize,
    /// How long before a deck reads as stale, ms.
    pub ttl_ms: i64,
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
    /// How long between ignore-ledger prunes, ms.
    pub prune_interval_ms: i64,
}

/// How long a built queue deck is served, ms: a day. Fixed, not a
/// setting, like every cache lifetime.
const QUEUE_TTL_MS: i64 = 86_400_000;
/// The deck's cache rows: the queue runs its own builds, so the cache's
/// timing is not used.
const DECK_TIMING: Timing = Timing {
    fresh_ms: QUEUE_TTL_MS,
    retry_ms: 0,
};
/// How long a queue card's details are reused: a day.
const QUEUE_ENRICH_TTL: Duration = Duration::from_secs(86400);

fn bounded(value: i64, min: i64, max: i64) -> usize {
    value.clamp(min, max) as usize
}

impl QueueSettings {
    /// The queue's view of the advanced settings, clamped to the ranges
    /// the settings form allows.
    pub fn from_advanced(advanced: &AdvancedSettings) -> Self {
        Self {
            queue_size: bounded(advanced.discover_queue_size, 1, 20),
            ttl_ms: QUEUE_TTL_MS,
            seed_artists: bounded(advanced.discover_queue_seed_artists, 1, 10),
            wildcard_slots: bounded(advanced.discover_queue_wildcard_slots, 0, 10),
            similar_artists_limit: bounded(advanced.discover_queue_similar_artists_limit, 5, 50),
            albums_per_similar: bounded(advanced.discover_queue_albums_per_similar, 1, 20),
            enrich_ttl: QUEUE_ENRICH_TTL,
            lastfm_mbid_max_lookups: bounded(
                advanced.discover_queue_lastfm_mbid_max_lookups,
                1,
                50,
            ),
            warm_cycle_build: advanced.discover_queue_warm_cycle_build,
            ignored_retention_days: advanced.ignored_releases_retention_days.max(1),
            prune_interval_ms: advanced.store_prune_interval_hours.max(1) * 3_600_000,
        }
    }
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self::from_advanced(&AdvancedSettings::default())
    }
}

impl QueueSettings {
    /// The queue settings in the advanced settings now, or the defaults
    /// when they cannot be read.
    pub fn read(advanced: &Live<AdvancedSettings>) -> Self {
        match advanced.get() {
            Ok(advanced) => Self::from_advanced(&advanced),
            Err(error) => {
                tracing::warn!(%error, "cannot read advanced settings; the queue uses defaults");
                Self::default()
            }
        }
    }
}

/// One deck as saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deck {
    /// Build id.
    queue_id: String,
    /// Deck cards in order.
    items: Vec<QueueItemLight>,
}

/// Where one user's build stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No build running; the saved deck, if any, is the answer.
    Settled,
    /// A build is running.
    Building,
    /// The last build failed.
    Failed,
}

/// One user's queue state.
struct UserState {
    source: String,
    phase: Phase,
    error: Option<String>,
    scheduled: bool,
    run: u64,
    task: Option<tokio::task::AbortHandle>,
    /// A finished deck until its row is written, so the build reads as
    /// done the moment it is.
    unsaved: Option<Saved<Deck>>,
}

impl UserState {
    fn new(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            phase: Phase::Settled,
            error: None,
            scheduled: false,
            run: 0,
            task: None,
            unsaved: None,
        }
    }

    fn stop_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    fn settled(&mut self) {
        self.phase = Phase::Settled;
    }
}

fn is_stale(deck: Option<&Saved<Deck>>, settings: &QueueSettings, now: i64) -> bool {
    deck.is_none_or(|deck| now - deck.built_at > settings.ttl_ms)
}

fn status_of(
    phase: Phase,
    error: Option<String>,
    deck: Option<&Saved<Deck>>,
    settings: &QueueSettings,
    now: i64,
) -> DiscoverQueueStatusResponse {
    let mut status = DiscoverQueueStatusResponse {
        status: QueueBuildStatus::Idle,
        queue_id: None,
        item_count: None,
        built_at: None,
        stale: None,
        error: None,
    };
    match (phase, deck) {
        (Phase::Building, _) => status.status = QueueBuildStatus::Building,
        (Phase::Failed, _) => {
            status.status = QueueBuildStatus::Error;
            status.error = error;
        }
        (Phase::Settled, Some(deck)) => {
            status.status = QueueBuildStatus::Ready;
            status.queue_id = Some(deck.value.queue_id.clone());
            status.item_count = Some(deck.value.items.len() as i64);
            status.built_at = Some(deck.built_at / 1000);
            status.stale = Some(is_stale(Some(deck), settings, now));
        }
        (Phase::Settled, None) => {}
    }
    status
}

fn to_deck(saved: &Saved<Deck>) -> QueueDeck {
    QueueDeck {
        queue_id: saved.value.queue_id.clone(),
        items: saved.value.items.clone(),
    }
}

fn generate_response(
    action: QueueAction,
    status: DiscoverQueueStatusResponse,
) -> QueueGenerateResponse {
    QueueGenerateResponse {
        action,
        status: status.status,
        queue_id: status.queue_id,
        item_count: status.item_count,
        built_at: status.built_at,
        stale: status.stale,
        error: status.error,
    }
}

struct Inner {
    listening: ListeningData,
    db: QueueDb,
    decks: UserPageCache<Deck>,
    youtube: Arc<LiveYouTube>,
    cache: ProviderCacheStore,
    settings: Live<AdvancedSettings>,
    states: Mutex<HashMap<String, UserState>>,
    last_prune: Mutex<i64>,
}

/// The live queue deck.
#[derive(Clone)]
pub struct LiveQueue {
    inner: Arc<Inner>,
}

/// What the queue is built over.
pub struct QueueInputs {
    /// Provider reads.
    pub listening: ListeningData,
    /// The database: the queue tables and the deck rows.
    pub db: Db,
    /// The kernel clock decks and ignores are dated by.
    pub clock: Arc<dyn Clock>,
    /// Previews behind the cards.
    pub youtube: Arc<LiveYouTube>,
    /// Where card details are cached.
    pub cache: ProviderCacheStore,
    /// The advanced settings, read on every call.
    pub settings: Live<AdvancedSettings>,
}

impl LiveQueue {
    /// A queue over its inputs.
    pub fn new(inputs: QueueInputs) -> Self {
        Self {
            inner: Arc::new(Inner {
                decks: UserPageCache::new(
                    inputs.db.clone(),
                    inputs.clock,
                    PageKind::Queue,
                    DECK_TIMING,
                ),
                db: QueueDb::new(inputs.db),
                listening: inputs.listening,
                youtube: inputs.youtube,
                cache: inputs.cache,
                settings: inputs.settings,
                states: Mutex::new(HashMap::new()),
                last_prune: Mutex::new(0),
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

    /// The user's deck: one just built and not yet written, else the
    /// saved row.
    async fn deck(&self, user_id: &str, variant: &Variant) -> Option<Saved<Deck>> {
        {
            let mut states = self.states();
            let state = Self::state(&mut states, user_id, &variant.source);
            if let Some(unsaved) = &state.unsaved {
                return Some(unsaved.clone());
            }
        }
        self.decks.load(user_id, variant).await
    }

    async fn status(&self, user_id: &str) -> DiscoverQueueStatusResponse {
        let variant = Variant::for_user(&self.listening, user_id).await;
        let deck = self.deck(user_id, &variant).await;
        let settings = QueueSettings::read(&self.settings);
        let mut states = self.states();
        let state = Self::state(&mut states, user_id, &variant.source);
        status_of(
            state.phase,
            state.error.clone(),
            deck.as_ref(),
            &settings,
            self.decks.now_ms(),
        )
    }

    async fn current(&self, user_id: &str) -> Option<QueueDeck> {
        let variant = Variant::for_user(&self.listening, user_id).await;
        self.deck(user_id, &variant).await.as_ref().map(to_deck)
    }

    /// Start a build, or say why none started. The handle is the build's
    /// task, for a caller that waits on it.
    async fn start_build(
        self: &Arc<Self>,
        user_id: &str,
        trigger: QueueTrigger,
    ) -> (QueueGenerateResponse, Option<tokio::task::JoinHandle<()>>) {
        let variant = Variant::for_user(&self.listening, user_id).await;
        let deck = self.deck(user_id, &variant).await;
        // The deck is saved under the clear generation of its start, so a
        // clear while it builds keeps nothing.
        let generation = self.decks.generation().await;
        let settings = QueueSettings::read(&self.settings);
        let scheduled = trigger == QueueTrigger::Scheduled;
        let force = trigger == QueueTrigger::Request { force: true };
        let now = self.decks.now_ms();
        let mut states = self.states();
        let state = Self::state(&mut states, user_id, &variant.source);
        let answer = |action: QueueAction, state: &UserState| {
            generate_response(
                action,
                status_of(
                    state.phase,
                    state.error.clone(),
                    deck.as_ref(),
                    &settings,
                    now,
                ),
            )
        };
        if scheduled && !settings.warm_cycle_build {
            return (answer(QueueAction::Disabled, state), None);
        }
        if state.phase == Phase::Building && state.scheduled && !scheduled {
            // The person asking wins over the warm cycle.
            state.stop_task();
            state.settled();
        }
        if state.phase == Phase::Building {
            return (answer(QueueAction::AlreadyBuilding, state), None);
        }
        if !force && state.phase == Phase::Settled && !is_stale(deck.as_ref(), &settings, now) {
            return (answer(QueueAction::AlreadyReady, state), None);
        }
        state.stop_task();
        state.phase = Phase::Building;
        state.error = None;
        state.scheduled = scheduled;
        state.run += 1;
        let run = state.run;
        let inner = Arc::clone(self);
        let user = user_id.to_owned();
        let built_for = variant.clone();
        // Someone who asked waits for the deck; the warm cycle does not.
        let priority = if scheduled {
            RequestPriority::BackgroundSync
        } else {
            RequestPriority::UserInitiated
        };
        let task = tokio::spawn(async move {
            inner
                .run_build(user, built_for, generation, run, priority)
                .await
        });
        state.task = Some(task.abort_handle());
        (answer(QueueAction::Started, state), Some(task))
    }

    /// One background build. Its result lands only if no newer build or
    /// source switch replaced it meanwhile.
    async fn run_build(
        self: Arc<Self>,
        user_id: String,
        variant: Variant,
        generation: Option<i64>,
        run: u64,
        priority: RequestPriority,
    ) {
        let settings = QueueSettings::read(&self.settings);
        self.prune_if_due(&settings).await;
        let ask = Ask::build(priority);
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.db,
            settings: &settings,
            ask: ask.clone(),
        };
        let outcome = build_deck(&ctx, &user_id, settings.queue_size).await;
        let built = {
            let mut states = self.states();
            let Some(state) = states.get_mut(&user_id) else {
                return;
            };
            if state.run != run || state.source != variant.source {
                return;
            }
            state.task = None;
            if state.scheduled && !QueueSettings::read(&self.settings).warm_cycle_build {
                // The warm cycle was switched off while this ran.
                state.settled();
                return;
            }
            match outcome {
                Ok(items) => {
                    let deck = Saved {
                        value: Deck {
                            items,
                            queue_id: uuid::Uuid::new_v4().to_string(),
                        },
                        built_at: self.decks.now_ms(),
                    };
                    state.unsaved = Some(deck.clone());
                    state.settled();
                    deck
                }
                Err(cause) => {
                    tracing::error!(%cause, "discover queue build failed");
                    state.phase = Phase::Failed;
                    state.error =
                        Some("The queue could not be built. Try again in a moment.".to_owned());
                    return;
                }
            }
        };
        let saved = match generation {
            Some(generation) => {
                self.decks
                    .save(&user_id, &variant, &built.value, generation)
                    .await
            }
            None => false,
        };
        {
            let mut states = self.states();
            if let Some(state) = states.get_mut(&user_id)
                && saved
                && state
                    .unsaved
                    .as_ref()
                    .is_some_and(|unsaved| unsaved.value.queue_id == built.value.queue_id)
            {
                state.unsaved = None;
            }
        }
        // Fetch every card's listen count in one call now, so opening the
        // cards does not pay one paced ListenBrainz call each.
        let ids: Vec<String> = built
            .value
            .items
            .iter()
            .map(|item| item.musicbrainz_id.clone())
            .collect();
        if let Err(cause) = self.listening.listen_counts(&user_id, &ids, &ask).await {
            tracing::debug!(%cause, "queue listen counts not prefetched");
        }
    }

    /// Drop ignores past their retention, at most once per prune interval.
    async fn prune_if_due(&self, settings: &QueueSettings) {
        let now = self.decks.now_ms();
        let due = {
            let mut last = self
                .last_prune
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if now - *last < settings.prune_interval_ms {
                false
            } else {
                *last = now;
                true
            }
        };
        if !due {
            return;
        }
        let cutoff = now - settings.ignored_retention_days * 86_400_000;
        match self.db.prune_ignored(cutoff).await {
            Ok(0) => {}
            Ok(dropped) => tracing::info!(dropped, "expired queue ignores removed"),
            Err(cause) => tracing::warn!(%cause, "queue ignores not pruned; trying next build"),
        }
    }

    /// A scheduled build, waited for; true when it left a deck.
    async fn warm(self: &Arc<Self>, user_id: &str) -> bool {
        let (started, task) = self.start_build(user_id, QueueTrigger::Scheduled).await;
        match started.action {
            QueueAction::AlreadyReady => return true,
            QueueAction::Started => {}
            QueueAction::AlreadyBuilding | QueueAction::Disabled => return false,
        }
        let Some(task) = task else {
            return false;
        };
        if task.await.is_err() {
            // Replaced by a request, or the build panicked.
            return false;
        }
        let source = self.listening.namespace();
        let mut states = self.states();
        let state = Self::state(&mut states, user_id, &source);
        state.phase == Phase::Settled
    }

    async fn build_now(
        &self,
        user_id: &str,
        count: Option<usize>,
    ) -> Result<QueueDeck, ProviderFailure> {
        let settings = QueueSettings::read(&self.settings);
        // The request waits for this deck.
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.db,
            settings: &settings,
            ask: Ask::build(RequestPriority::UserInitiated),
        };
        let items = build_deck(&ctx, user_id, count.unwrap_or(settings.queue_size))
            .await
            .map_err(ProviderFailure::failed)?;
        Ok(QueueDeck {
            queue_id: uuid::Uuid::new_v4().to_string(),
            items,
        })
    }

    async fn enrich(
        self: &Arc<Self>,
        user_id: &str,
        release_group_mbid: &str,
    ) -> Result<QueueEnrichment, ProviderFailure> {
        let group = release_group_mbid.trim().to_ascii_lowercase();
        // Only the facts every user shares are kept under this key; the
        // user's own parts are added below.
        let key = CacheKey::new(
            Source::MusicBrainz,
            &[&self.listening.namespace(), "queue_card_facts", &group],
        );
        let lifetime = QueueSettings::read(&self.settings).enrich_ttl;
        let policy = Policy {
            fresh: lifetime,
            miss: lifetime,
        };
        let inner = Arc::clone(self);
        let fetched_group = group.clone();
        let found = self
            .cache
            .get_or_fetch(
                &key,
                policy,
                RequestPriority::UserInitiated,
                move |_| async move {
                    let (facts, hold) = cards::facts(&inner.listening, &fetched_group).await;
                    // Facts built past a failed read are shown, not kept.
                    Ok::<_, String>(if hold == CacheHold::default() {
                        Fetched::Found(facts)
                    } else {
                        Fetched::Partial(facts)
                    })
                },
            )
            .await;
        let facts = found
            .map_err(ProviderFailure::failed)?
            .ok_or_else(|| ProviderFailure::failed("queue card details missing"))?;
        Ok(cards::for_user(
            &self.listening,
            self.youtube.as_ref(),
            user_id,
            &group,
            facts,
        )
        .await)
    }
}

impl LiveQueue {
    /// The user's last built deck, stale or not, when there is one.
    pub fn current<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<QueueDeck>> {
        Box::pin(self.inner.current(user_id))
    }

    /// Build a deck now, in the request, without keeping it. `count`
    /// overrides the configured queue size.
    pub fn build_now<'a>(
        &'a self,
        user_id: &'a str,
        count: Option<usize>,
    ) -> BoxFuture<'a, Result<QueueDeck, ProviderFailure>> {
        Box::pin(self.inner.build_now(user_id, count))
    }

    /// Where the user's background build stands.
    pub fn status<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, DiscoverQueueStatusResponse> {
        Box::pin(self.inner.status(user_id))
    }

    /// Start a background build, or report why none started.
    pub fn start_build<'a>(
        &'a self,
        user_id: &'a str,
        trigger: QueueTrigger,
    ) -> BoxFuture<'a, QueueGenerateResponse> {
        Box::pin(async move { self.inner.start_build(user_id, trigger).await.0 })
    }

    /// Record one ignored release for the user. Later decks skip it.
    pub fn ignore_release<'a>(
        &'a self,
        user_id: &'a str,
        release: &'a QueueIgnoreRequest,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async move {
            self.inner
                .db
                .ignore(user_id, release, self.inner.decks.now_ms())
                .await
                .map_err(ProviderFailure::failed)
        })
    }

    /// The user's ignore ledger, newest first.
    pub fn ignored<'a>(
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

    /// Details behind one card: tags, date, country, bio, listens, video.
    pub fn enrich<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<QueueEnrichment, ProviderFailure>> {
        Box::pin(self.inner.enrich(user_id, release_group_mbid))
    }

    /// An on-demand video preview behind one card.
    pub fn preview<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverQueuePreview, ProviderFailure>> {
        Box::pin(async move {
            cards::preview(
                &self.inner.listening,
                self.inner.youtube.as_ref(),
                release_group_mbid,
            )
            .await
        })
    }

    /// The given release groups that the library already holds.
    pub fn validate<'a>(
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

impl Warm for LiveQueue {
    fn feature(&self) -> Feature {
        Feature::Queue
    }

    fn warm<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(self.inner.warm(user_id))
    }
}
