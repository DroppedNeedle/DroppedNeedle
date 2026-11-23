//! The discover queue deck.
//!
//! Each user has at most one background build at a time. A finished deck
//! is a row of the page cache, its only copy, so it survives a restart;
//! it reads as stale once older than a day, and a library change leaves
//! it alone. The warm scheduler may build decks too, but any request from
//! the user replaces a warm build that is still running. What the user
//! told Discover ([`feedback`](crate::discover::feedback)) is durable:
//! every later build skips what it hides, and a deck is served without
//! it, so one built before the answer never shows the album again.
//!
//! A build that met a failed read and found fewer cards of its own than
//! the deck it would replace keeps that deck, and the next build may start
//! five minutes later. The status says why a deck is short (`reason`).
//!
//! The pieces: [`build`] assembles a deck from the listening reads,
//! [`select`] holds the pure picking logic, [`cards`] the per-card
//! details and [`store`] the tables.

pub mod build;
pub mod cards;
pub mod seeds;
pub mod select;
pub mod store;

use droppedneedle_providers::RequestPriority;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::discover::cache::{PageKind, SaveOutcome, Saved, Timing, UserPageCache, Variant};
use crate::discover::feedback::{self, Feedback, FeedbackError, FeedbackTarget};
use crate::discover::health::{DeckHealth, deck_health, log_deck_health};
use crate::discover::listening::{Ask, ListeningData};
use crate::discover::models::{
    DiscoverFeedbackEntry, DiscoverFeedbackRequest, DiscoverQueuePreview,
    DiscoverQueueStatusResponse, QueueAction, QueueBuildStatus, QueueEnrichment,
    QueueGenerateResponse, QueueItemLight, SectionCondition, ShelfSource,
};
use crate::discover::ports::{BoxFuture, ProviderFailure, QueueDeck, QueueTrigger};
use crate::discover::record::{self, MadeFrom, Offer};
use crate::discover::scheduler::{Feature, Warm};
use crate::discover::youtube::LiveYouTube;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};

use crate::cache_hold::CacheHold;

use build::{BuildContext, build_deck};
use store::QueueDb;

/// The queue settings one call works with, read fresh each time.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSettings {
    /// Cards per deck.
    pub queue_size: usize,
    /// Seed artists per deck.
    pub seed_artists: usize,
    /// Trending wildcards per deck.
    pub wildcard_slots: usize,
    /// Similar artists read per seed.
    pub similar_artists_limit: usize,
    /// Albums read per similar artist.
    pub albums_per_similar: usize,
    /// MusicBrainz lookups one Last.fm album batch may spend.
    pub lastfm_mbid_max_lookups: usize,
    /// Whether the warm cycle may build decks.
    pub warm_cycle_build: bool,
    /// Whether a user with Last.fm and ListenBrainz both linked gets seed
    /// and similar artists from both.
    pub blend_services: bool,
    /// Days a hide carried from before typed answers is kept, at most
    /// its year.
    pub ignored_retention_days: i64,
    /// How long between prunes of lapsed answers, ms.
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
/// How long after a build that kept the deck before it the next may
/// start, ms.
const KEPT_RETRY_MS: i64 = 300_000;
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
            seed_artists: bounded(advanced.discover_queue_seed_artists, 1, 10),
            wildcard_slots: bounded(advanced.discover_queue_wildcard_slots, 0, 10),
            similar_artists_limit: bounded(advanced.discover_queue_similar_artists_limit, 5, 50),
            albums_per_similar: bounded(advanced.discover_queue_albums_per_similar, 1, 20),
            lastfm_mbid_max_lookups: bounded(
                advanced.discover_queue_lastfm_mbid_max_lookups,
                1,
                50,
            ),
            warm_cycle_build: advanced.discover_queue_warm_cycle_build,
            blend_services: advanced.discover_blend_listening_services,
            ignored_retention_days: advanced.ignored_releases_retention_days.max(1),
            prune_interval_ms: advanced.store_prune_interval_hours.max(1) * 3_600_000,
        }
    }
}

impl QueueSettings {
    /// The queue settings in the advanced settings now. `None` when they
    /// cannot be read: the work they steer waits until they are saved
    /// again, and [`Live::read`] logs it once.
    pub fn read(advanced: &Live<AdvancedSettings>) -> Option<Self> {
        advanced
            .read()
            .map(|advanced| Self::from_advanced(&advanced))
    }
}

/// What a person who asks for Discover while its settings cannot be read
/// is told.
pub const SETTINGS_UNREADABLE: &str =
    "Discover's settings cannot be read. Save Settings > Advanced again to resume Discover.";

/// One deck as saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deck {
    /// Build id.
    queue_id: String,
    /// Deck cards in order.
    items: Vec<QueueItemLight>,
    /// Its health numbers.
    #[serde(default)]
    health: Option<DeckHealth>,
    /// Why it is short, when it is.
    #[serde(default)]
    reason: Option<SectionCondition>,
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
    /// When the last build met a failed read and kept the deck before it:
    /// the time the next build may start, ms.
    kept_until: Option<i64>,
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
            kept_until: None,
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

/// Whether a new deck is due: there is none, it is past its age, or the
/// build that kept it is past its wait (`kept_until`).
fn is_stale(deck: Option<&Saved<Deck>>, kept_until: Option<i64>, now: i64) -> bool {
    deck.is_none_or(|deck| now - deck.built_at > QUEUE_TTL_MS)
        || kept_until.is_some_and(|until| now >= until)
}

/// Why `items`, built for a deck of `count`, is short: a failed read,
/// nothing at all, or under half the cards. `None` when it is not short.
fn short_reason(items: &[QueueItemLight], count: usize, failed: bool) -> Option<SectionCondition> {
    if items.len() >= count {
        None
    } else if failed {
        Some(SectionCondition::SourceDown)
    } else if items.is_empty() {
        Some(SectionCondition::Empty)
    } else if items.len() * 2 < count {
        Some(SectionCondition::Thin)
    } else {
        None
    }
}

/// The cards of a deck that are not trending wildcards.
fn own_cards(items: &[QueueItemLight]) -> usize {
    items.iter().filter(|item| !item.is_wildcard).count()
}

fn status_of(
    phase: Phase,
    error: Option<String>,
    deck: Option<&Saved<Deck>>,
    kept_until: Option<i64>,
    now: i64,
) -> DiscoverQueueStatusResponse {
    let mut status = DiscoverQueueStatusResponse {
        status: QueueBuildStatus::Idle,
        queue_id: None,
        item_count: None,
        built_at: None,
        stale: None,
        error: None,
        reason: None,
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
            status.stale = Some(is_stale(Some(deck), kept_until, now));
            status.reason = if kept_until.is_some() {
                Some(SectionCondition::SourceDown)
            } else {
                deck.value.reason
            };
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
        reason: status.reason,
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
    forgets: crate::discover::cache::ForgetCounts,
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
    /// The kernel clock decks and answers are dated by.
    pub clock: Arc<dyn Clock>,
    /// Previews behind the cards.
    pub youtube: Arc<LiveYouTube>,
    /// Where card details are cached.
    pub cache: ProviderCacheStore,
    /// The advanced settings, read on every call.
    pub settings: Live<AdvancedSettings>,
    /// Each person's forget count, shared with every page cache and the
    /// forget.
    pub forgets: crate::discover::cache::ForgetCounts,
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
                    inputs.forgets.clone(),
                ),
                db: QueueDb::new(inputs.db),
                listening: inputs.listening,
                youtube: inputs.youtube,
                cache: inputs.cache,
                settings: inputs.settings,
                states: Mutex::new(HashMap::new()),
                last_prune: Mutex::new(0),
                forgets: inputs.forgets.clone(),
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
    /// switch starts the user over.
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
        // The phase is read before the deck, so a build that settles in
        // between never reads as ready with the deck it replaced.
        let (phase, error, kept_until) = {
            let mut states = self.states();
            let state = Self::state(&mut states, user_id, &variant.source);
            (state.phase, state.error.clone(), state.kept_until)
        };
        let deck = match phase {
            Phase::Settled => self.deck(user_id, &variant).await,
            Phase::Building | Phase::Failed => None,
        };
        status_of(phase, error, deck.as_ref(), kept_until, self.decks.now_ms())
    }

    /// The user's deck without what they hid: a deck built, or still
    /// building, when an answer lands can hold the album.
    async fn current(&self, user_id: &str) -> Option<QueueDeck> {
        let variant = Variant::for_user(&self.listening, user_id).await;
        let mut deck = to_deck(&self.deck(user_id, &variant).await?);
        Feedback::load_or_none(self.db.db(), user_id, self.decks.now_ms(), "deck")
            .await
            .keep_cards(&mut deck.items);
        Some(deck)
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
        // The deck is saved under the generation of its start, so a clear
        // or the person's forget while it builds keeps nothing.
        let generation = self.decks.generation(user_id).await;
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
                    state.kept_until,
                    now,
                ),
            )
        };
        // Unreadable settings hold every build until they are saved again;
        // a person who asked is told why.
        let Some(settings) = settings else {
            if !scheduled && state.phase != Phase::Building {
                state.phase = Phase::Failed;
                state.error = Some(SETTINGS_UNREADABLE.to_owned());
            }
            return (answer(QueueAction::Disabled, state), None);
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
        if !force
            && state.phase == Phase::Settled
            && !is_stale(deck.as_ref(), state.kept_until, now)
        {
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
                .run_build(user, built_for, settings, generation, run, priority)
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
        settings: QueueSettings,
        generation: Option<crate::discover::cache::Generation>,
        run: u64,
        priority: RequestPriority,
    ) {
        self.prune_if_due(&settings).await;
        let ask = Ask::build(priority);
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.db,
            settings: &settings,
            ask: ask.clone(),
            now_ms: self.decks.now_ms(),
        };
        let started = tokio::time::Instant::now();
        let outcome = build_deck(&ctx, &user_id, settings.queue_size).await;
        // The deck this build would replace, read before the state is
        // locked.
        let replaced = match &outcome {
            Ok(_) if ask.met_failure() => self.deck(&user_id, &variant).await,
            _ => None,
        };
        let built = {
            let mut states = self.states();
            let Some(state) = states.get_mut(&user_id) else {
                return;
            };
            if state.run != run || state.source != variant.source {
                return;
            }
            state.task = None;
            if state.scheduled
                && QueueSettings::read(&self.settings).is_none_or(|now| !now.warm_cycle_build)
            {
                // The warm cycle was switched off, or its settings stopped
                // reading, while this ran.
                state.settled();
                return;
            }
            match outcome {
                Ok(items)
                    if replaced.as_ref().is_some_and(|before| {
                        own_cards(&items) < own_cards(&before.value.items)
                    }) =>
                {
                    tracing::warn!(
                        cards = items.len(),
                        "discover queue build met a failed read; keeping the deck before it"
                    );
                    state.kept_until = Some(self.decks.now_ms() + KEPT_RETRY_MS);
                    state.settled();
                    return;
                }
                Ok(items) => {
                    let deck = Saved {
                        value: Deck {
                            health: Some(deck_health(&items)),
                            reason: short_reason(&items, settings.queue_size, ask.met_failure()),
                            items,
                            queue_id: uuid::Uuid::new_v4().to_string(),
                        },
                        built_at: self.decks.now_ms(),
                    };
                    state.unsaved = Some(deck.clone());
                    state.kept_until = None;
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
        if let Some(health) = &built.value.health {
            log_deck_health(&user_id, started.elapsed().as_secs_f64(), health);
        }
        let outcome = match generation {
            Some(generation) => {
                self.decks
                    .save(&user_id, &variant, &built.value, generation)
                    .await
            }
            None => SaveOutcome::Failed,
        };
        if let Some(generation) = generation
            && outcome.kept()
        {
            let policy = self.listening.policy(&user_id).await;
            let service = if seeds::links_listening(policy.user()) {
                ShelfSource::from(policy.listening())
            } else {
                ShelfSource::Local
            };
            let offers = built
                .value
                .items
                .iter()
                .map(|card| Offer::from_card(card, Some(service)))
                .collect();
            record::offered(
                self.db.db(),
                &user_id,
                MadeFrom::Deck,
                offers,
                built.built_at,
                record::Built {
                    generation,
                    forgets: &self.forgets,
                },
            )
            .await;
        }
        {
            let mut states = self.states();
            // A dropped deck was built from before a clear or a forget.
            if let Some(state) = states.get_mut(&user_id)
                && outcome != SaveOutcome::Failed
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

    /// Drop answers past their expiry, and carried hides past their
    /// retention, at most once per prune interval.
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
        match feedback::prune(self.db.db(), now, cutoff).await {
            Ok(0) => {}
            Ok(dropped) => tracing::info!(dropped, "lapsed discover answers removed"),
            Err(cause) => tracing::warn!(%cause, "discover answers not pruned; trying next build"),
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
        let settings = QueueSettings::read(&self.settings)
            .ok_or_else(|| ProviderFailure::not_configured(SETTINGS_UNREADABLE))?;
        // The request waits for this deck.
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.db,
            settings: &settings,
            ask: Ask::build(RequestPriority::UserInitiated),
            now_ms: self.decks.now_ms(),
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
        let policy = Policy {
            fresh: QUEUE_ENRICH_TTL,
            miss: QUEUE_ENRICH_TTL,
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
    /// Drop the person's built deck not yet written (their "Forget my
    /// history"), so it is never served.
    pub fn forget_unsaved(&self, user_id: &str) {
        if let Some(state) = self.inner.states().get_mut(user_id) {
            state.unsaved = None;
        }
    }

    /// The user's last built deck, stale or not, when there is one,
    /// without what they hid.
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

    /// Record one answer for the user and return it as stored. Later
    /// decks skip what it hides.
    pub fn save_feedback<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a DiscoverFeedbackRequest,
    ) -> BoxFuture<'a, Result<DiscoverFeedbackEntry, FeedbackError>> {
        Box::pin(feedback::save(
            self.inner.db.db(),
            user_id,
            request,
            self.inner.decks.now_ms(),
        ))
    }

    /// Take back one of the user's answers. Later decks may hold what it
    /// hid.
    pub fn undo_feedback<'a>(
        &'a self,
        user_id: &'a str,
        target: FeedbackTarget,
        mbid_lower: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async move {
            feedback::undo(self.inner.db.db(), user_id, target, mbid_lower)
                .await
                .map_err(|error| ProviderFailure::failed(format!("feedback undo: {error}")))
        })
    }

    /// The user's answers in effect, newest first.
    pub fn feedback<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<DiscoverFeedbackEntry>, ProviderFailure>> {
        Box::pin(async move {
            feedback::list(self.inner.db.db(), user_id, self.inner.decks.now_ms())
                .await
                .map_err(|error| ProviderFailure::failed(format!("feedback read: {error}")))
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
