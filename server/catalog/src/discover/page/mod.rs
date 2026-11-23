//! The discover page: shelves built in the background, served from the
//! last good build, kept fresh while people use it.
//!
//! A build makes the sections the user has switched on; a section
//! switched off costs no reads. A visit leaves out what is switched off
//! now, and a page built while a section the user has since switched on
//! was off is rebuilt behind the visit.
//!
//! A request never waits for a build. It gets the user's saved page from
//! the page cache, however old, with ownership marked from the library as
//! it is now. When the page is missing or more than an hour old, a
//! rebuild starts in the background and the answer says so
//! (`refreshing`), so the page polls until the new one lands. With no
//! page at all the answer is an empty, `loading` page. An empty or failed
//! build never replaces a good page, and the next one waits five minutes,
//! so the page settles instead of rebuilding on every poll. A page built
//! while a read failed or a shelf ran out of time is kept, but only for
//! those five minutes, so the missing shelf is tried again soon.
//!
//! A section whose own build failed keeps the one the page it replaces
//! showed, built under a week ago, so a bad hour at a provider takes
//! nothing away. Every section switched on that is not plainly built says
//! how it stands (`section_states`): kept, its source down, waiting for a
//! linked account, thin, empty, or not built yet.
//!
//! Every serve, the home teaser's included, leaves out of the sections
//! about the person what they told Discover to hide ([`Feedback`]), so an
//! answer shows at once.
//!
//! While the advanced settings cannot be read no build starts: visits get
//! the saved page, and builds resume once the settings are saved again.
//!
//! The warm scheduler keeps the pages people use fresh through
//! [`DiscoverPages`]'s registration. Each kept page queues a background
//! warm of its covers and artist images.

pub mod build;
pub mod library;
pub mod memo;
pub mod picks;
pub mod shelves;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::artwork::warm::{CoverWarm, WarmTargets};
use crate::discover::cache::{
    ForgetCounts, Generation, PageKind, Saved, Timing, UserPageCache, Variant,
};
use crate::discover::content::IntegrationSettings;
use crate::discover::feedback::Feedback;
use crate::discover::health::{BuildFacts, PageHealth, log_page_health, page_health};
use crate::discover::listening::{Ask, ListeningData};
use crate::discover::models::{
    ChartSection, DiscoverActivityResponse, DiscoverPreview, DiscoverResponse, IntegrationStatus,
    ListenSource, PageSection, PromptService, SectionCondition, SectionContent, SectionItem,
    SectionKey, SectionState, ServiceState,
};
use crate::discover::ports::{BoxFuture, ProviderFailure};
use crate::discover::queue::QueueSettings;
use crate::discover::queue::store::QueueDb;
use crate::discover::scheduler::{ArtistDetail, Feature, Warm, WarmScheduler};
use crate::discover::sections::{self, Page, SectionDef};
use build::{Builder, Outcome, PageSettings, mark_ownership, on_page};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::{Live, secret_sections::AdvancedSettings};
use library::LibraryReads;
use memo::BuildMemo;

/// A page older than an hour is rebuilt behind the next visit: its reads
/// are cached for hours, so a sooner rebuild repeats the same answers. A
/// failed or empty build is tried again after five minutes.
const TIMING: Timing = Timing {
    fresh_ms: 3_600_000,
    retry_ms: 300_000,
};
/// How long a section is carried past failed builds at most, ms: a week.
/// Past it the section is left out and says why.
const CARRY_MS: i64 = 7 * 86_400_000;
/// The memos age by their own rules; their cache never builds.
const MEMO_TIMING: Timing = Timing {
    fresh_ms: 0,
    retry_ms: 0,
};

/// Genre artwork schema the frontend expects. Quirk (v2): the tag keeps v2's
/// name.
pub const GENRE_ARTWORK_SCHEMA: &str = "v2";

/// A page with no shelves.
pub fn empty_page() -> DiscoverResponse {
    DiscoverResponse {
        sections: Vec::new(),
        section_states: Vec::new(),
        discover_queue_enabled: true,
        integration_status: None,
        service_prompts: Vec::new(),
        genre_artwork: HashMap::new(),
        genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
        generated_at: None,
        refresh_started_at: None,
        refreshing: false,
        service_status: None,
    }
}

/// A built page as the cache keeps it, with the sections the user had
/// switched off when it was built, whether a read failed or a shelf ran
/// out of time while it was, its health numbers, and the MusicBrainz
/// source it was built from (a later build carries its sections only
/// from the same source).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltPage {
    page: DiscoverResponse,
    skipped: Vec<SectionKey>,
    #[serde(default)]
    incomplete: bool,
    #[serde(default)]
    health: Option<PageHealth>,
    #[serde(default)]
    source: Option<String>,
}

/// Everything the page reads from.
pub struct PageInputs {
    /// Provider reads.
    pub listening: ListeningData,
    /// The queue tables.
    pub queue_db: QueueDb,
    /// The database: library reads and the page cache.
    pub db: Db,
    /// The kernel clock the pages age by.
    pub clock: Arc<dyn Clock>,
    /// The discover tuning, read per build.
    pub advanced: Live<AdvancedSettings>,
    /// The integration flags, read per build.
    pub integrations: IntegrationSettings,
    /// Where a published page queues its image warm.
    pub warm: CoverWarm,
    /// Each person's forget count, shared with every page cache and the
    /// forget.
    pub forgets: ForgetCounts,
}

struct Inner {
    listening: ListeningData,
    queue_db: QueueDb,
    library: LibraryReads,
    cache: UserPageCache<BuiltPage>,
    memo: BuildMemo,
    advanced: Live<AdvancedSettings>,
    integrations: IntegrationSettings,
    warm: CoverWarm,
    forgets: ForgetCounts,
}

/// The discover pages: their cache and their build.
#[derive(Clone)]
pub struct DiscoverPages {
    inner: Arc<Inner>,
}

impl DiscoverPages {
    /// The pages over their inputs.
    pub fn new(inputs: PageInputs) -> Self {
        let memo = BuildMemo {
            mixes: UserPageCache::new(
                inputs.db.clone(),
                inputs.clock.clone(),
                PageKind::DailyMixes,
                MEMO_TIMING,
                inputs.forgets.clone(),
            ),
            picks: UserPageCache::new(
                inputs.db.clone(),
                inputs.clock.clone(),
                PageKind::TopPicks,
                MEMO_TIMING,
                inputs.forgets.clone(),
            ),
        };
        Self {
            inner: Arc::new(Inner {
                library: LibraryReads::new(inputs.db.clone()),
                cache: UserPageCache::new(
                    inputs.db.clone(),
                    inputs.clock.clone(),
                    PageKind::Discover,
                    TIMING,
                    inputs.forgets.clone(),
                ),
                listening: inputs.listening,
                queue_db: inputs.queue_db,
                memo,
                advanced: inputs.advanced,
                integrations: inputs.integrations,
                warm: inputs.warm,
                forgets: inputs.forgets,
            }),
        }
    }

    /// Home's discover teaser: the first "because you listen to" shelf of
    /// the user's last built page, without what `feedback` hides and
    /// without starting a build.
    pub async fn teaser(&self, user_id: &str, feedback: &Feedback) -> Option<DiscoverPreview> {
        let variant = Variant::for_user(&self.inner.listening, user_id).await;
        let mut saved = self.inner.cache.load(user_id, &variant).await?.value;
        feedback.filter_sections(Page::Discover, &mut saved.page.sections);
        let entry = saved.page.sections.into_iter().find_map(|section| {
            match (section.key, section.content) {
                (SectionKey::BecauseYouListenTo, SectionContent::Because { entries }) => {
                    entries.into_iter().next()
                }
                _ => None,
            }
        })?;
        Some(DiscoverPreview {
            seed_artist: entry.seed_artist,
            seed_artist_mbid: entry.seed_artist_mbid,
            items: entry
                .section
                .items
                .into_iter()
                .take(shelves::SHELF_SIZE)
                .filter_map(|item| match item {
                    SectionItem::Artist(artist) => Some(artist),
                    _ => None,
                })
                .collect(),
        })
    }

    /// What the person told Discover, as it stands now.
    async fn feedback(&self, user_id: &str, surface: &str) -> Feedback {
        Feedback::load_or_none(
            self.inner.queue_db.db(),
            user_id,
            self.inner.cache.now_ms(),
            surface,
        )
        .await
    }

    /// The settings a build reads now. `None` while the advanced settings
    /// cannot be read: no build starts until they are saved again.
    fn settings(&self) -> Option<PageSettings> {
        let advanced = self.inner.advanced.read()?;
        Some(PageSettings {
            picks_count: advanced.discover_picks_count.clamp(4, 30) as usize,
            genre_weight: advanced
                .discover_picks_genre_affinity_weight
                .clamp(0.0, 1.0)
                / 2.0,
            queue: QueueSettings::from_advanced(&advanced),
            download_client: self.inner.integrations.download_client_ready(),
            lastfm_on: self.inner.integrations.status().lastfm,
        })
    }

    /// Start a background build for the user unless one is running or the
    /// settings cannot be read.
    async fn start(&self, user_id: &str, variant: Variant) {
        let Some(settings) = self.settings() else {
            return;
        };
        let Some(claim) = self.inner.cache.claim(user_id).await else {
            return;
        };
        let pages = self.clone();
        let user_id = user_id.to_owned();
        tokio::spawn(async move {
            let build = pages.build(&user_id, &variant, claim.generation(), settings);
            pages.inner.cache.run(claim, &variant, build).await;
        });
    }

    /// Build the user's page now and keep it when it has anything to show.
    /// Returns whether a new page was kept.
    pub async fn warm(&self, user_id: &str) -> bool {
        let Some(settings) = self.settings() else {
            return false;
        };
        let variant = Variant::for_user(&self.inner.listening, user_id).await;
        let Some(claim) = self.inner.cache.claim(user_id).await else {
            return false;
        };
        let build = self.build(user_id, &variant, claim.generation(), settings);
        self.inner.cache.run(claim, &variant, build).await
    }

    /// One build. `None` when it found nothing to show or the MusicBrainz
    /// source changed while it ran.
    async fn build(
        &self,
        user_id: &str,
        variant: &Variant,
        generation: Generation,
        settings: PageSettings,
    ) -> Option<BuiltPage> {
        let started = tokio::time::Instant::now();
        let off = sections::switched_off(self.inner.library.db(), user_id, Page::Discover).await;
        // The page this build replaces, which the health's repeats count.
        let previous = self.inner.cache.load(user_id, variant).await;
        let source = self.inner.listening.source();
        let now = self.inner.cache.now_ms() as f64 / 1000.0;
        let (today, year) = today(now);
        let feedback = self.feedback(user_id, "discover build").await;
        let builder = Builder {
            listening: &self.inner.listening,
            ask: Ask::build(droppedneedle_providers::RequestPriority::BackgroundSync),
            queue_db: &self.inner.queue_db,
            feedback: &feedback,
            library: &self.inner.library,
            memo: &self.inner.memo,
            variant,
            generation,
            forgets: &self.inner.forgets,
            settings,
            today,
            year,
            now,
        };
        let wanted = sections::wanted(Page::Discover, &off);
        let built = builder.build(user_id, &wanted).await;
        if self.inner.listening.source() != source {
            tracing::info!("MusicBrainz source changed during a discover build; dropping it");
            return None;
        }
        let mut page = built.page;
        let mut degraded = built.degraded;
        if self.inner.listening.popularity_down() {
            degraded
                .entry(ListenSource::Listenbrainz)
                .or_insert(ServiceState::Degraded);
        }
        page.service_status = (!degraded.is_empty()).then_some(degraded);
        let now_ms = self.inner.cache.now_ms();
        page.generated_at = Some(now_ms / 1000);
        page.refresh_started_at = None;
        page.refreshing = false;
        let replaced = previous
            .as_ref()
            .filter(|saved| saved.value.source.as_deref() == Some(variant.source.as_str()));
        settle(&mut page, &built.outcomes, replaced, now_ms);
        // Counted as the page would be served now, ownership marked.
        let mut served = page.clone();
        mark_ownership(self.inner.library.db(), &mut served).await;
        let health = page_health(
            &served,
            previous.as_ref().map(|saved| &saved.value.page),
            &BuildFacts {
                wanted: &wanted,
                seeds_used: built.seeds_used,
                seeds_found: built.seeds_found,
                incomplete: built.incomplete,
                listenbrainz: built.listenbrainz,
                lastfm: built.lastfm,
            },
        );
        log_page_health(user_id, started.elapsed().as_secs_f64(), &health);
        if page.sections.is_empty() {
            tracing::warn!(user = %user_id, "discover build found nothing to show; keeping the previous page");
            return None;
        }
        self.inner.warm.queue("discover", warm_targets(&page));
        Some(BuiltPage {
            page,
            skipped: sections::sorted(&off),
            incomplete: built.incomplete,
            health: Some(health),
            source: Some(variant.source.clone()),
        })
    }
}

impl Warm for DiscoverPages {
    fn feature(&self) -> Feature {
        Feature::Discover
    }

    fn warm<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(DiscoverPages::warm(self, user_id))
    }
}

/// The live discover page and the warm scheduler behind it.
#[derive(Clone)]
pub struct LiveDiscover {
    pages: DiscoverPages,
    scheduler: Arc<WarmScheduler>,
}

impl LiveDiscover {
    /// The page over `pages`, kept warm by `scheduler`.
    pub fn new(pages: DiscoverPages, scheduler: Arc<WarmScheduler>) -> Self {
        Self { pages, scheduler }
    }

    /// The warm scheduler.
    pub fn scheduler(&self) -> &Arc<WarmScheduler> {
        &self.scheduler
    }

    /// The user's saved page with ownership marked as the library has it
    /// now, starting a background rebuild when there is none or it is
    /// more than an hour old.
    pub async fn page(&self, user_id: &str, status: IntegrationStatus) -> DiscoverResponse {
        let inner = &self.pages.inner;
        let off = sections::switched_off(inner.library.db(), user_id, Page::Discover).await;
        let variant = Variant::for_user(&inner.listening, user_id).await;
        // The ListenBrainz card asks the person to link their own account,
        // so it shows only while reads are on and they have no link.
        let listenbrainz_settled = !status.listenbrainz || variant.listenbrainz;
        let lastfm_readable = variant.lastfm;
        let saved = inner.cache.load(user_id, &variant).await;
        // A page that left out a section switched on since, or one built
        // past a failed read more than the retry wait ago, is rebuilt as
        // if it were missing.
        let now = inner.cache.now_ms();
        let current = saved.as_ref().filter(|saved| {
            saved.value.skipped.iter().all(|key| off.contains(key))
                && !(saved.value.incomplete && now - saved.built_at > TIMING.retry_ms)
        });
        if inner.cache.due(user_id, current) {
            self.pages.start(user_id, variant).await;
        }
        let started = inner.cache.building(user_id);
        let building = started.is_some();
        let wanted = sections::wanted(Page::Discover, &off);
        let mut page = match saved {
            Some(saved) => {
                let mut page = saved.value.page;
                page.sections.retain(|section| !off.contains(&section.key));
                self.pages
                    .feedback(user_id, "discover page")
                    .await
                    .filter_sections(Page::Discover, &mut page.sections);
                // A section switched on since the page was built is being
                // built while a build runs.
                let unbuilt: HashSet<SectionKey> = saved.value.skipped.iter().copied().collect();
                page.section_states =
                    serve_states(std::mem::take(&mut page.section_states), &wanted, |key| {
                        building && unbuilt.contains(&key)
                    });
                if !status.lastfm {
                    page.service_prompts
                        .retain(|prompt| prompt.service != PromptService::Lastfm);
                }
                mark_ownership(inner.library.db(), &mut page).await;
                page
            }
            None => {
                let mut page = empty_page();
                page.service_prompts = shelves::service_prompts(
                    listenbrainz_settled,
                    status.jellyfin,
                    status.download_client,
                    status.lastfm,
                    lastfm_readable,
                );
                page.section_states = serve_states(Vec::new(), &wanted, |_| building);
                page
            }
        };
        page.discover_queue_enabled = !off.contains(&SectionKey::DiscoverQueue);
        page.refreshing = building;
        page.refresh_started_at = started.map(|at| at / 1000);
        page.integration_status = Some(status);
        page
    }

    /// Ask for a fresh page now: forget the memoised shelves and rebuild.
    pub async fn refresh(&self, user_id: &str) {
        let inner = &self.pages.inner;
        if inner.cache.building(user_id).is_some() {
            return;
        }
        inner.memo.clear_user(user_id).await;
        inner.cache.retry_now(user_id);
        let variant = Variant::for_user(&inner.listening, user_id).await;
        self.pages.start(user_id, variant).await;
    }

    /// Record that the user used a discover feature and start warming
    /// what is due for them.
    pub async fn record_activity(
        &self,
        user_id: &str,
        feature: Feature,
        detail: ArtistDetail<'_>,
    ) -> Result<DiscoverActivityResponse, ProviderFailure> {
        let (source, written) = self
            .scheduler
            .record(user_id, feature, detail)
            .await
            .map_err(|error| {
                ProviderFailure::failed(format!("discover activity write: {error}"))
            })?;
        if written {
            let scheduler = Arc::clone(&self.scheduler);
            let user_id = user_id.to_owned();
            tokio::spawn(async move { scheduler.run_due(Some(&user_id)).await });
        }
        Ok(DiscoverActivityResponse {
            source_mode: source.mode,
            source_id: source.id,
            generation: source.generation,
        })
    }
}

/// Settle a build's sections against the page it replaces. A section whose
/// build failed keeps the one that page showed, while that was built (not
/// itself left short by a failure) under [`CARRY_MS`] ago, less any row a
/// freshly built section shows; one that built, or built empty, replaces
/// it. Every section not plainly built gets its state, in page order.
fn settle(
    page: &mut DiscoverResponse,
    outcomes: &[(SectionKey, Outcome)],
    replaced: Option<&Saved<BuiltPage>>,
    now_ms: i64,
) {
    // The good section the replaced page showed, and when it was built. A
    // section that page showed under `source_down` holds a failed build's
    // rows and is never carried, so a failure never restarts the week.
    let before = |key: SectionKey| {
        let saved = replaced?;
        let shown = &saved.value.page;
        let state = shown.section_states.iter().find(|state| state.key == key);
        if state.is_some_and(|state| state.state == SectionCondition::SourceDown) {
            return None;
        }
        let section = shown.sections.iter().find(|section| section.key == key)?;
        let since = state
            .filter(|state| state.state == SectionCondition::Kept)
            .and_then(|state| state.since)
            .unwrap_or(saved.built_at / 1000);
        Some((section, since))
    };
    let carried: HashMap<SectionKey, (&PageSection, i64)> = outcomes
        .iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Failed(_)))
        .filter_map(|(key, _)| Some((*key, before(*key)?)))
        .filter(|(_, (_, since))| now_ms - since * 1000 <= CARRY_MS)
        .collect();
    // What this build's own sections show, which a carried one leaves out.
    let fresh: HashSet<String> = page
        .sections
        .iter()
        .filter(|section| !carried.contains_key(&section.key))
        .flat_map(|section| row_ids(&section.content))
        .collect();
    let mut states = Vec::new();
    for (key, outcome) in outcomes {
        let key = *key;
        let Some(def) = sections::find(Page::Discover, key) else {
            continue;
        };
        let state = |state, source, since| SectionState {
            key,
            zone: def.zone,
            state,
            source,
            since,
        };
        let settled = match *outcome {
            Outcome::Built => {
                let now = page.sections.iter().find(|section| section.key == key);
                let shrank = match (now, before(key)) {
                    (Some(now), Some((before, _))) => {
                        size(&now.content) * 2 < size(&before.content)
                    }
                    _ => false,
                };
                shrank.then(|| state(SectionCondition::Thin, None, None))
            }
            Outcome::Empty => Some(state(SectionCondition::Empty, None, None)),
            Outcome::NeedsLink(service) => {
                Some(state(SectionCondition::NeedsLink, Some(service), None))
            }
            Outcome::Failed(source) => {
                let kept = carried.get(&key).and_then(|(section, since)| {
                    let mut section = (*section).clone();
                    drop_rows(&mut section.content, &fresh);
                    (!section.content.is_empty()).then_some((section, *since))
                });
                match kept {
                    Some((section, since)) => {
                        page.sections.retain(|shown| shown.key != key);
                        page.sections.push(section);
                        Some(state(SectionCondition::Kept, source, Some(since)))
                    }
                    // Rows the failed build did get stay, under this state.
                    None => Some(state(SectionCondition::SourceDown, source, None)),
                }
            }
        };
        states.extend(settled);
    }
    let order: HashMap<SectionKey, usize> = outcomes
        .iter()
        .enumerate()
        .map(|(at, (key, _))| (*key, at))
        .collect();
    page.sections
        .sort_by_key(|section| order.get(&section.key).copied().unwrap_or(usize::MAX));
    page.section_states = states;
}

/// The album and artist ids a section's rows show, lowercased.
fn row_ids(content: &SectionContent) -> Vec<String> {
    let mut ids = Vec::new();
    match content {
        SectionContent::Picks { picks } => {
            ids.extend(
                picks
                    .items
                    .iter()
                    .filter_map(|pick| pick.album.musicbrainz_id.clone()),
            );
        }
        SectionContent::Weekly { .. } => {}
        _ => {
            for shelf in content.shelves() {
                ids.extend(shelf.items.iter().filter_map(row_id));
            }
        }
    }
    ids.iter().map(|id| id.to_lowercase()).collect()
}

/// An album or artist row's id.
fn row_id(item: &SectionItem) -> Option<String> {
    match item {
        SectionItem::Album(album) => album.musicbrainz_id.clone(),
        SectionItem::Artist(artist) => artist.musicbrainz_id.clone(),
        SectionItem::Track(_) | SectionItem::Genre(_) => None,
    }
}

/// Drop the album and artist rows whose id is in `shown`, and any shelf of
/// several left with none.
fn drop_rows(content: &mut SectionContent, shown: &HashSet<String>) {
    let unseen = |id: Option<String>| id.is_none_or(|id| !shown.contains(&id.to_lowercase()));
    match content {
        SectionContent::Picks { picks } => picks
            .items
            .retain(|pick| unseen(pick.album.musicbrainz_id.clone())),
        SectionContent::Weekly { .. } => {}
        SectionContent::Because { entries } => {
            for entry in entries.iter_mut() {
                entry.section.items.retain(|item| unseen(row_id(item)));
            }
            entries.retain(|entry| !entry.section.items.is_empty());
        }
        SectionContent::Shelves { shelves } => {
            for shelf in shelves.iter_mut() {
                shelf.items.retain(|item| unseen(row_id(item)));
            }
            shelves.retain(|shelf| !shelf.items.is_empty());
        }
        SectionContent::Shelf { shelf } => shelf.items.retain(|item| unseen(row_id(item))),
    }
}

/// How much a section holds, to tell a thin one: its rows, or its shelves
/// for a section of several.
fn size(content: &SectionContent) -> usize {
    match content {
        SectionContent::Shelf { shelf } => shelf.items.len(),
        SectionContent::Shelves { shelves } => shelves.len(),
        SectionContent::Because { entries } => entries.len(),
        SectionContent::Picks { picks } => picks.items.len(),
        SectionContent::Weekly { weekly } => weekly.tracks.len(),
    }
}

/// The states a page is served with, in page order: the saved ones of
/// sections still `wanted`, and `building` for each wanted section the
/// page has not built yet while a build runs (`unbuilt`).
fn serve_states(
    saved: Vec<SectionState>,
    wanted: &[&'static SectionDef],
    unbuilt: impl Fn(SectionKey) -> bool,
) -> Vec<SectionState> {
    let mut saved: HashMap<SectionKey, SectionState> =
        saved.into_iter().map(|state| (state.key, state)).collect();
    wanted
        .iter()
        .filter(|def| on_page(def.key))
        .filter_map(|def| {
            saved.remove(&def.key).or_else(|| {
                unbuilt(def.key).then_some(SectionState {
                    key: def.key,
                    zone: def.zone,
                    state: SectionCondition::Building,
                    source: None,
                    since: None,
                })
            })
        })
        .collect()
}

/// The covers and artist images a discover page shows, top to bottom.
fn warm_targets(page: &DiscoverResponse) -> WarmTargets {
    let mut targets = WarmTargets::default();
    for section in &page.sections {
        section.content.warm(&mut targets);
    }
    targets
}

/// A shelf's album covers and artist images.
pub(crate) fn add_section(targets: &mut WarmTargets, section: &ChartSection) {
    for item in &section.items {
        match item {
            SectionItem::Album(album) => targets.cover(album.musicbrainz_id.as_deref()),
            SectionItem::Artist(artist) => targets.artist(artist.musicbrainz_id.as_deref()),
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

#[cfg(test)]
mod tests {
    use super::{BuiltPage, CARRY_MS, Outcome, empty_page, settle};
    use crate::discover::cache::Saved;
    use crate::discover::models::{
        DiscoverResponse, ListenSource, PageSection, SectionCondition, SectionContent, SectionItem,
        SectionKey, SectionKind, SectionState, SectionZone,
    };
    use crate::discover::page::shelves::{album, shelf};

    const DAY_MS: i64 = 86_400_000;
    const DOWN: Outcome = Outcome::Failed(Some(ListenSource::Listenbrainz));

    fn albums(key: SectionKey, ids: &[&str]) -> PageSection {
        let items = ids
            .iter()
            .map(|id| SectionItem::Album(album(Some(id), id, None, None, None)))
            .collect();
        PageSection {
            key,
            zone: SectionZone::NewAndFresh,
            content: SectionContent::Shelf {
                shelf: shelf("Shelf", SectionKind::Albums, items, None).expect("rows"),
            },
        }
    }

    fn page(sections: Vec<PageSection>, states: Vec<SectionState>) -> DiscoverResponse {
        DiscoverResponse {
            sections,
            section_states: states,
            ..empty_page()
        }
    }

    fn saved(page: DiscoverResponse, built_at: i64) -> Saved<BuiltPage> {
        Saved {
            value: BuiltPage {
                page,
                skipped: Vec::new(),
                incomplete: false,
                health: None,
                source: Some("source".to_owned()),
            },
            built_at,
        }
    }

    fn ids(page: &DiscoverResponse, key: SectionKey) -> Vec<String> {
        page.sections
            .iter()
            .filter(|section| section.key == key)
            .flat_map(|section| section.content.shelves())
            .flat_map(|shelf| shelf.items.iter())
            .filter_map(|item| match item {
                SectionItem::Album(row) => row.musicbrainz_id.clone(),
                _ => None,
            })
            .collect()
    }

    fn state(page: &DiscoverResponse, key: SectionKey) -> Option<(SectionCondition, Option<i64>)> {
        page.section_states
            .iter()
            .find(|state| state.key == key)
            .map(|state| (state.state, state.since))
    }

    /// Rows a failed build got are shown under `source_down` and never
    /// carried, so a second failure shows its own rows, and a kept section
    /// keeps the week counted from the build that worked.
    #[test]
    fn a_failed_build_never_carries_rows_another_failure_left() {
        let key = SectionKey::FreshReleases;
        let outcomes = [(key, DOWN)];

        let first = saved(
            page(
                vec![albums(key, &["a"])],
                vec![SectionState {
                    key,
                    zone: SectionZone::NewAndFresh,
                    state: SectionCondition::SourceDown,
                    source: Some(ListenSource::Listenbrainz),
                    since: None,
                }],
            ),
            DAY_MS,
        );
        let mut second = page(vec![albums(key, &["a", "b"])], Vec::new());
        settle(&mut second, &outcomes, Some(&first), 2 * DAY_MS);
        assert_eq!(
            ids(&second, key),
            ["a", "b"],
            "the second failure's own rows"
        );
        assert_eq!(
            state(&second, key),
            Some((SectionCondition::SourceDown, None))
        );

        // Good at day 0, kept since then by a failure on day 1.
        let good = saved(page(vec![albums(key, &["a", "b", "c"])], Vec::new()), 0);
        let mut kept = page(vec![albums(key, &["a"])], Vec::new());
        settle(&mut kept, &outcomes, Some(&good), DAY_MS);
        assert_eq!(ids(&kept, key), ["a", "b", "c"]);
        assert_eq!(state(&kept, key), Some((SectionCondition::Kept, Some(0))));

        // Past the week counted from day 0, the failure's own rows show.
        let kept = saved(kept, DAY_MS);
        let mut late = page(vec![albums(key, &["d"])], Vec::new());
        settle(&mut late, &outcomes, Some(&kept), CARRY_MS + 1);
        assert_eq!(ids(&late, key), ["d"]);
        assert_eq!(
            state(&late, key),
            Some((SectionCondition::SourceDown, None))
        );
    }

    /// A carried section leaves out the albums a freshly built one shows.
    #[test]
    fn a_kept_section_repeats_nothing_a_fresh_one_shows() {
        let kept = SectionKey::FreshReleases;
        let fresh = SectionKey::NewFromFollowed;
        let before = saved(page(vec![albums(kept, &["a", "b"])], Vec::new()), 0);
        let mut now = page(vec![albums(fresh, &["A"])], Vec::new());
        settle(
            &mut now,
            &[(kept, DOWN), (fresh, Outcome::Built)],
            Some(&before),
            DAY_MS,
        );
        assert_eq!(ids(&now, kept), ["b"]);
        assert_eq!(ids(&now, fresh), ["A"]);
        assert_eq!(state(&now, kept), Some((SectionCondition::Kept, Some(0))));
    }
}
