//! The discover page: shelves built in the background, served from the
//! last good build, kept fresh while people use it.
//!
//! A request never waits for a build. It gets the user's saved page from
//! the page cache, however old, with ownership marked from the library as
//! it is now. When the page is missing or more than five minutes old, a
//! rebuild starts in the background and the answer says so
//! (`refreshing`), so the page polls until the new one lands. With no
//! page at all the answer is an empty, `loading` page. An empty or failed
//! build never replaces a good page, and the next one waits five minutes,
//! so the page settles instead of rebuilding on every poll.
//!
//! The warm scheduler keeps the pages people use fresh through
//! [`DiscoverPages`]'s registration. Each kept page queues a background
//! warm of its covers and artist images.

pub mod build;
pub mod library;
pub mod memo;
pub mod picks;
pub mod shelves;

use std::collections::HashMap;
use std::sync::Arc;

use crate::reads::discover::cache::{PageKind, Timing, UserPageCache, Variant};
use crate::reads::discover::content::IntegrationSettings;
use crate::reads::discover::listening::{Ask, ListeningData};
use crate::reads::discover::models::{
    ChartSection, DiscoverActivityResponse, DiscoverResponse, IntegrationStatus, ListenSource,
    SectionItem, ServiceState,
};
use crate::reads::discover::ports::{BoxFuture, ProviderFailure};
use crate::reads::discover::queue::QueueSettings;
use crate::reads::discover::queue::store::QueueDb;
use crate::reads::discover::scheduler::{ArtistDetail, Feature, Warm, WarmScheduler};
use crate::reads::platform::artwork::warm::{CoverWarm, WarmTargets};
use build::{Builder, PageSettings, mark_ownership};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::{Live, Plain, secret_sections::AdvancedSettings};
use library::LibraryReads;
use memo::BuildMemo;

/// A page older than this is rebuilt behind the next visit, and a failed
/// build is not retried sooner.
const TIMING: Timing = Timing {
    fresh_ms: 300_000,
    retry_ms: 300_000,
};
/// The memos age by their own rules; their cache never builds.
const MEMO_TIMING: Timing = Timing {
    fresh_ms: 0,
    retry_ms: 0,
};

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
    /// Where a published page queues its image warm, when anywhere.
    pub warm: Option<CoverWarm>,
}

struct Inner {
    listening: ListeningData,
    queue_db: QueueDb,
    library: LibraryReads,
    cache: UserPageCache<DiscoverResponse>,
    memo: BuildMemo,
    advanced: Live<AdvancedSettings>,
    integrations: IntegrationSettings,
    warm: Option<CoverWarm>,
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
            ),
            picks: UserPageCache::new(
                inputs.db.clone(),
                inputs.clock.clone(),
                PageKind::TopPicks,
                MEMO_TIMING,
            ),
        };
        Self {
            inner: Arc::new(Inner {
                library: LibraryReads::new(inputs.db.sqlx().clone()),
                cache: UserPageCache::new(
                    inputs.db.clone(),
                    inputs.clock.clone(),
                    PageKind::Discover,
                    TIMING,
                ),
                listening: inputs.listening,
                queue_db: inputs.queue_db,
                memo,
                advanced: inputs.advanced,
                integrations: inputs.integrations,
                warm: inputs.warm,
            }),
        }
    }

    /// The user's last built page, without starting a build (home's
    /// discover teaser).
    pub async fn cached_page(&self, user_id: &str) -> Option<DiscoverResponse> {
        let variant = Variant::for_user(&self.inner.listening, user_id).await;
        self.inner
            .cache
            .load(user_id, &variant)
            .await
            .map(|saved| saved.value)
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

    /// Start a background build for the user unless one is running.
    async fn start(&self, user_id: &str, variant: Variant) {
        let Some(claim) = self.inner.cache.claim(user_id).await else {
            return;
        };
        let pages = self.clone();
        let user_id = user_id.to_owned();
        tokio::spawn(async move {
            let build = pages.build(&user_id, &variant, claim.generation());
            pages.inner.cache.run(claim, &variant, build).await;
        });
    }

    /// Build the user's page now and keep it when it has anything to show.
    /// Returns whether a new page was kept.
    pub async fn warm(&self, user_id: &str) -> bool {
        let variant = Variant::for_user(&self.inner.listening, user_id).await;
        let Some(claim) = self.inner.cache.claim(user_id).await else {
            return false;
        };
        let build = self.build(user_id, &variant, claim.generation());
        self.inner.cache.run(claim, &variant, build).await
    }

    /// One build. `None` when it found nothing to show or the MusicBrainz
    /// source changed while it ran.
    async fn build(
        &self,
        user_id: &str,
        variant: &Variant,
        generation: i64,
    ) -> Option<DiscoverResponse> {
        let source = self.inner.listening.source();
        let download_client = self.inner.integrations.download_client_ready();
        let now = self.inner.cache.now_ms() as f64 / 1000.0;
        let (today, year) = today(now);
        let builder = Builder {
            listening: &self.inner.listening,
            ask: Ask::build(droppedneedle_providers::RequestPriority::BackgroundSync),
            queue_db: &self.inner.queue_db,
            library: &self.inner.library,
            memo: &self.inner.memo,
            variant,
            generation,
            settings: self.settings(download_client),
            today,
            year,
            now,
        };
        let built = builder.build(user_id).await;
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
        page.generated_at = Some(self.inner.cache.now_ms() / 1000);
        page.refresh_started_at = None;
        page.section_status = HashMap::new();
        page.refreshing = false;
        if !shelves::has_content(&page) {
            tracing::warn!(user = %user_id, "discover build found nothing to show; keeping the previous page");
            return None;
        }
        if let Some(warm) = &self.inner.warm {
            warm.queue("discover", warm_targets(&page));
        }
        Some(page)
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
    /// more than five minutes old.
    pub async fn page(&self, user_id: &str, status: IntegrationStatus) -> DiscoverResponse {
        let inner = &self.pages.inner;
        let variant = Variant::for_user(&inner.listening, user_id).await;
        let saved = inner.cache.load(user_id, &variant).await;
        if inner.cache.due(user_id, saved.as_ref()) {
            self.pages.start(user_id, variant).await;
        }
        let started = inner.cache.building(user_id);
        let building = started.is_some();
        let mut page = match saved {
            Some(saved) => {
                let mut page = saved.value;
                mark_ownership(inner.library.pool(), &mut page).await;
                page.section_status = shelves::section_status(Some(&page), building);
                page
            }
            None => {
                let mut page = empty_page();
                page.service_prompts = shelves::service_prompts(
                    status.listenbrainz,
                    status.jellyfin,
                    status.download_client,
                    status.lastfm,
                );
                // With nothing building, a build just failed: the page has
                // settled empty rather than still loading.
                page.section_status = shelves::section_status((!building).then_some(&page), false);
                page
            }
        };
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

/// The covers and artist images a discover page shows, top to bottom.
fn warm_targets(page: &DiscoverResponse) -> WarmTargets {
    let mut targets = WarmTargets::default();
    if let Some(picks) = &page.top_picks {
        for pick in &picks.items {
            targets.cover(pick.album.musicbrainz_id.as_deref());
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
