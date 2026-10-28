//! The home page: library shelves read on every visit, the shelves the
//! user's listening services fill built in the background and kept fresh
//! while people use it (v2 `HomeService`).
//!
//! A visit never waits on a provider. The local shelves (recently added,
//! your albums and artists, recently played, the discover teaser) come
//! straight from SQLite on every visit. The provider shelves come from
//! the user's row in the page cache, however old, which survives a
//! restart; when it is missing or more than five minutes old a rebuild
//! starts behind the visit (`refreshing`, which the page polls). A build
//! that fails or finds nothing keeps the old shelves and is not retried
//! for five minutes, so a dead provider does not cost every poll a build.
//!
//! The provider shelves are what the user's listening services know:
//! trending artists and popular albums (ListenBrainz, or Last.fm when
//! that is the user's primary source), the user's top albums and loved
//! tracks, their genres and the weekly exploration playlist. Ownership is
//! marked and genre tiles get artwork from the collection on every
//! answer, so a library change shows at once. Each kept build queues a
//! background warm of its covers and artist images.
//!
//! The sections come from the section list ([`sections`]), in its order.
//! A section the user switched off is neither read nor built, and a build
//! made while a section switched on since was off is redone behind the
//! visit.

pub mod library;
pub mod shelves;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::cache::{PageKind, Timing, UserPageCache, Variant};
use super::content::IntegrationSettings;
use super::listening::{Ask, ListenSource, ListenedAlbum, ListeningData, StatsRange, UserMusic};
use super::ownership;
use super::page::build::{genre_list, weekly_exploration};
use super::page::shelves::{self as page_shelves, RowMut, one};
use super::page::{DiscoverPages, GENRE_ARTWORK_SCHEMA};
use super::scheduler::{Feature, Warm};
use crate::reads::discover::genre::{self, artwork::GenreArt};
use crate::reads::discover::models::{
    AlbumRef, ArtistRef, ChartSection, HomeResponse, IntegrationStatus, PageSection,
    SectionContent, SectionItem, SectionKey,
};
use crate::reads::discover::ports::BoxFuture;
use crate::reads::discover::sections::{self, Page};
use crate::reads::platform::artwork::warm::{CoverWarm, WarmTargets};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use library::HomeLibrary;

/// Shelves older than this are rebuilt behind the next visit, and a
/// failed build is not retried sooner.
const TIMING: Timing = Timing {
    fresh_ms: 300_000,
    retry_ms: 300_000,
};
/// Rows a library read fetches.
const LIBRARY_ROWS: i64 = 15;
/// Rows a provider read asks for (v2).
const PROVIDER_ROWS: u32 = 20;
/// Bound on one provider read in a full build.
const READ_TIMEOUT: Duration = Duration::from_secs(25);
/// Genres whose tiles get artwork.
const ART_GENRES: usize = 20;

/// Run one read under the bound; a failure empties its shelf with a log
/// line.
async fn bounded<T: Default, E: std::fmt::Display>(
    what: &str,
    read: impl std::future::Future<Output = Result<T, E>>,
) -> T {
    match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(Ok(value)) => value,
        Ok(Err(cause)) => {
            tracing::warn!(%cause, read = what, "home read failed; its shelf stays empty");
            T::default()
        }
        Err(_) => {
            tracing::warn!(read = what, "home read timed out; its shelf stays empty");
            T::default()
        }
    }
}

/// Mark which albums and artists the library holds.
pub(super) async fn mark_owned(
    pool: &SqlitePool,
    albums: Vec<&mut AlbumRef>,
    artists: Vec<&mut ArtistRef>,
) {
    let album_ids: Vec<&str> = albums
        .iter()
        .filter_map(|row| row.musicbrainz_id.as_deref())
        .collect();
    let artist_ids: Vec<&str> = artists
        .iter()
        .filter_map(|row| row.musicbrainz_id.as_deref())
        .collect();
    let owned_albums = if album_ids.is_empty() {
        HashMap::new()
    } else {
        ownership::owned_albums(pool, &album_ids)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "album ownership unreadable; marking none owned");
                HashMap::new()
            })
    };
    let owned_artists = if artist_ids.is_empty() {
        HashMap::new()
    } else {
        ownership::owned_artists(pool, &artist_ids)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "artist ownership unreadable; marking none owned");
                HashMap::new()
            })
    };
    for row in albums {
        if let Some(local) = row
            .musicbrainz_id
            .as_deref()
            .and_then(|mbid| owned_albums.get(&mbid.to_ascii_lowercase()))
        {
            row.in_library = true;
            row.local_id.get_or_insert_with(|| local.clone());
        }
    }
    for row in artists {
        if let Some(local) = row
            .musicbrainz_id
            .as_deref()
            .and_then(|mbid| owned_artists.get(&mbid.to_ascii_lowercase()))
        {
            row.in_library = true;
            row.local_id.get_or_insert_with(|| local.clone());
        }
    }
}

/// Mark ownership on every album and artist row of a page.
async fn mark_page(pool: &SqlitePool, page: &mut HomeResponse) {
    let mut albums: Vec<&mut AlbumRef> = Vec::new();
    let mut artists: Vec<&mut ArtistRef> = Vec::new();
    for section in &mut page.sections {
        for row in section.content.rows_mut() {
            match row {
                RowMut::Album(row) => albums.push(row),
                RowMut::Artist(row) => artists.push(row),
            }
        }
    }
    if let Some(preview) = page.discover_preview.as_mut() {
        artists.extend(preview.items.iter_mut());
    }
    mark_owned(pool, albums, artists).await;
}

/// The covers and artist images a home page shows, top to bottom.
fn warm_targets(page: &HomeResponse) -> WarmTargets {
    let mut targets = WarmTargets::default();
    for section in &page.sections {
        section.content.warm(&mut targets);
    }
    if let Some(preview) = &page.discover_preview {
        for artist in &preview.items {
            targets.artist(artist.musicbrainz_id.as_deref());
        }
    }
    targets
}

/// A page with no shelves.
pub fn empty_page(status: IntegrationStatus, refreshing: bool) -> HomeResponse {
    HomeResponse {
        sections: Vec::new(),
        service_prompts: Vec::new(),
        integration_status: Some(status),
        genre_artwork: HashMap::new(),
        genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
        discover_preview: None,
        refreshing,
    }
}

/// The shelves the user's listening services fill: what a home build
/// makes and the page cache keeps, with the sections the user had
/// switched off when it was built.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderShelves {
    sections: Vec<PageSection>,
    skipped: Vec<SectionKey>,
}

impl ProviderShelves {
    /// Genres alone are not worth keeping over an older build.
    fn has_content(&self) -> bool {
        self.sections
            .iter()
            .any(|section| section.key != SectionKey::GenreList)
    }
}

struct Inner {
    listening: ListeningData,
    library: HomeLibrary,
    pool: SqlitePool,
    art: Arc<GenreArt>,
    integrations: IntegrationSettings,
    warm: Option<CoverWarm>,
    cache: UserPageCache<ProviderShelves>,
    discover: Option<DiscoverPages>,
}

/// The live home page.
#[derive(Clone)]
pub struct LiveHome {
    inner: Arc<Inner>,
}

/// What home is built over.
pub struct HomeInputs {
    /// Provider reads.
    pub listening: ListeningData,
    /// The database: library reads and the page cache.
    pub db: Db,
    /// The kernel clock the shelves age by.
    pub clock: Arc<dyn Clock>,
    /// The integration flags, read per visit.
    pub integrations: IntegrationSettings,
    /// Genre tile artwork.
    pub art: Arc<GenreArt>,
    /// Where a kept build queues its image warm, when anywhere.
    pub warm: Option<CoverWarm>,
    /// The discover pages, for the teaser, when there are any.
    pub discover: Option<DiscoverPages>,
}

/// Sitewide or user album rows that map to a release group.
fn ranked(rows: Vec<ListenedAlbum>) -> Vec<super::listening::RankedAlbum> {
    rows.into_iter().filter_map(ListenedAlbum::ranked).collect()
}

impl LiveHome {
    /// Home over its inputs.
    pub fn new(inputs: HomeInputs) -> Self {
        let pool = inputs.db.sqlx().clone();
        Self {
            inner: Arc::new(Inner {
                cache: UserPageCache::new(inputs.db, inputs.clock, PageKind::Home, TIMING),
                listening: inputs.listening,
                library: HomeLibrary::new(pool.clone()),
                pool,
                art: inputs.art,
                integrations: inputs.integrations,
                warm: inputs.warm,
                discover: inputs.discover,
            }),
        }
    }

    /// The user's home page: the local shelves now, the provider shelves
    /// from their last build.
    pub async fn page(&self, user_id: &str) -> HomeResponse {
        let off = sections::switched_off(&self.inner.pool, user_id, Page::Home).await;
        let music = self.inner.listening.accounts(user_id).await;
        let variant = Variant::of(&self.inner.listening, &music);
        let saved = self.inner.cache.load(user_id, &variant).await;
        // Shelves that left out a section switched on since are rebuilt as
        // if they were missing.
        let current = saved
            .as_ref()
            .filter(|saved| saved.value.skipped.iter().all(|key| off.contains(key)));
        let started = self.inner.cache.due(user_id, current) && self.start(user_id, variant).await;
        let refreshing = started || self.inner.cache.building(user_id).is_some();
        let status = self.inner.integrations.status();
        let mut page = self.local(user_id, &music, status, &off).await;
        if let Some(saved) = saved {
            let mut built = saved.value.sections;
            page.sections = sections::wanted(Page::Home, &off)
                .into_iter()
                .filter_map(|def| {
                    let from = |list: &mut Vec<PageSection>| {
                        let at = list.iter().position(|section| section.key == def.key)?;
                        Some(list.remove(at))
                    };
                    from(&mut built).or_else(|| from(&mut page.sections))
                })
                .collect();
        }
        mark_page(&self.inner.pool, &mut page).await;
        page.refreshing = refreshing;
        self.apply_artwork(&mut page).await;
        page
    }

    /// Start a background build for the user unless one is running.
    /// True when one started.
    async fn start(&self, user_id: &str, variant: Variant) -> bool {
        let Some(claim) = self.inner.cache.claim(user_id).await else {
            return false;
        };
        let home = self.clone();
        let user_id = user_id.to_owned();
        tokio::spawn(async move {
            let build = home.build(&user_id);
            home.inner.cache.run(claim, &variant, build).await;
        });
        true
    }

    /// Build the user's provider shelves now and keep them. Returns
    /// whether they were kept.
    pub async fn warm(&self, user_id: &str) -> bool {
        let variant = Variant::for_user(&self.inner.listening, user_id).await;
        let Some(claim) = self.inner.cache.claim(user_id).await else {
            return false;
        };
        let build = self.build(user_id);
        self.inner.cache.run(claim, &variant, build).await
    }

    /// One build. `None` when it found nothing or the MusicBrainz source
    /// changed while it ran.
    async fn build(&self, user_id: &str) -> Option<ProviderShelves> {
        let off = sections::switched_off(&self.inner.pool, user_id, Page::Home).await;
        let source = self.inner.listening.namespace();
        let music = self.inner.listening.accounts(user_id).await;
        let shelves = self.build_shelves(user_id, &music, &off).await;
        if self.inner.listening.namespace() != source {
            tracing::info!("MusicBrainz source changed during a home build; dropping it");
            return None;
        }
        if !shelves.has_content() {
            tracing::warn!(user = %user_id, "home build found nothing to show; keeping the previous shelves");
            return None;
        }
        if let Some(warm) = &self.inner.warm {
            let mut page = self
                .local(user_id, &music, self.inner.integrations.status(), &off)
                .await;
            page.sections.extend(shelves.sections.iter().cloned());
            warm.queue("home", warm_targets(&page));
        }
        Some(shelves)
    }

    /// The local shelves: SQLite only.
    async fn local(
        &self,
        user_id: &str,
        music: &UserMusic,
        status: IntegrationStatus,
        off: &HashSet<SectionKey>,
    ) -> HomeResponse {
        let status = user_status(status, music);
        let library = &self.inner.library;
        let (recent, artists, plays) = tokio::join!(
            bounded("recent albums", library.recent_albums(LIBRARY_ROWS)),
            bounded("library artists", library.top_artists(LIBRARY_ROWS)),
            bounded("play history", library.recent_plays(user_id, LIBRARY_ROWS)),
        );
        let mut page = empty_page(status.clone(), false);
        page.sections = sections::wanted(Page::Home, off)
            .into_iter()
            .filter_map(|def| {
                let content = match def.key {
                    SectionKey::RecentlyAdded => one(shelves::recently_added(&recent)),
                    SectionKey::LibraryAlbums => one(shelves::your_albums(&recent)),
                    SectionKey::LibraryArtists => one(shelves::your_artists(&artists)),
                    SectionKey::RecentlyPlayed => one(shelves::recently_played(&plays)),
                    SectionKey::GenreList => one(genre_list(None)),
                    _ => None,
                }?;
                page_shelves::section(Page::Home, def.key, content)
            })
            .collect();
        page.service_prompts =
            shelves::service_prompts(status.listenbrainz, status.download_client, status.lastfm);
        page.discover_preview = self.discover_preview(user_id).await;
        page
    }

    /// The shelves what the user's listening services know fill, for the
    /// sections not switched off.
    async fn build_shelves(
        &self,
        user_id: &str,
        music: &UserMusic,
        off: &HashSet<SectionKey>,
    ) -> ProviderShelves {
        let wanted = sections::wanted(Page::Home, off);
        let built = futures_util::future::join_all(wanted.iter().map(|def| async move {
            let content = self.provider_section(def.key, user_id, music).await?;
            page_shelves::section(Page::Home, def.key, content)
        }))
        .await;
        ProviderShelves {
            sections: built.into_iter().flatten().collect(),
            skipped: sections::sorted(off),
        }
    }

    /// One section a listening service fills; `None` for the local ones.
    async fn provider_section(
        &self,
        key: SectionKey,
        user_id: &str,
        music: &UserMusic,
    ) -> Option<SectionContent> {
        let listening = &self.inner.listening;
        let ask = Ask::build(droppedneedle_providers::RequestPriority::BackgroundSync);
        let ask = &ask;
        let lastfm_primary = listening.policy_for(music).listening() == ListenSource::Lastfm;
        let lb_user = music.listenbrainz.as_deref();
        let lfm_user = music.lastfm_username.as_deref().filter(|_| music.lastfm);
        match key {
            SectionKey::TrendingArtists if lastfm_primary => {
                let artists = bounded(
                    "last.fm chart",
                    listening.lastfm_chart_artists(user_id, PROVIDER_ROWS, ask),
                )
                .await;
                one(shelves::trending_artists(&artists, ListenSource::Lastfm))
            }
            SectionKey::TrendingArtists => {
                let artists = bounded(
                    "listenbrainz trending artists",
                    listening.sitewide_artists(StatsRange::ThisWeek, PROVIDER_ROWS, 0, ask),
                )
                .await;
                one(shelves::trending_artists(
                    &artists,
                    ListenSource::Listenbrainz,
                ))
            }
            SectionKey::PopularAlbums if !lastfm_primary => {
                let albums = bounded("listenbrainz popular albums", async {
                    listening
                        .sitewide_albums(StatsRange::ThisWeek, PROVIDER_ROWS, 0, ask)
                        .await
                        .map(ranked)
                })
                .await;
                one(shelves::popular_albums(&albums))
            }
            SectionKey::YourTopAlbums => match (lastfm_primary, lb_user, lfm_user) {
                (false, Some(username), _) => {
                    let top = bounded("listenbrainz top albums", async {
                        listening
                            .user_albums(username, StatsRange::ThisMonth, PROVIDER_ROWS, 0, ask)
                            .await
                            .map(ranked)
                    })
                    .await;
                    one(shelves::your_top_albums(&top))
                }
                (true, _, Some(username)) => {
                    let top = bounded(
                        "last.fm top albums",
                        listening.lastfm_user_albums(
                            user_id,
                            username,
                            "1month",
                            PROVIDER_ROWS,
                            ask,
                        ),
                    )
                    .await;
                    one(shelves::your_top_albums_lastfm(&top))
                }
                _ => None,
            },
            SectionKey::FavoriteArtists => match (lastfm_primary, lb_user, lfm_user) {
                (false, Some(username), _) => {
                    let loved = bounded(
                        "listenbrainz loved",
                        listening.loved(username, PROVIDER_ROWS, ask),
                    )
                    .await;
                    one(shelves::favorites(&loved, ListenSource::Listenbrainz))
                }
                (true, _, Some(username)) => {
                    let loved = bounded(
                        "last.fm loved",
                        listening.lastfm_loved(user_id, username, PROVIDER_ROWS, ask),
                    )
                    .await;
                    one(shelves::favorites(&loved, ListenSource::Lastfm))
                }
                _ => None,
            },
            SectionKey::WeeklyExploration => {
                let username = lb_user?;
                tokio::time::timeout(
                    READ_TIMEOUT,
                    weekly_exploration(listening, ask, user_id, username),
                )
                .await
                .unwrap_or_else(|_| {
                    tracing::warn!("weekly exploration timed out on home");
                    None
                })
                .map(|weekly| SectionContent::Weekly { weekly })
            }
            SectionKey::GenreList => {
                let username = lb_user?;
                let genres = bounded("listenbrainz genres", listening.genres(username, ask)).await;
                one((!genres.is_empty())
                    .then(|| genre_list(Some(&genres)))
                    .flatten())
            }
            _ => None,
        }
    }

    /// The discover teaser, when there is a discover page.
    async fn discover_preview(&self, user_id: &str) -> Option<super::models::DiscoverPreview> {
        self.inner.discover.as_ref()?.teaser(user_id).await
    }

    async fn apply_artwork(&self, page: &mut HomeResponse) {
        let names = genre_names(page_shelves::shelf_of(
            &page.sections,
            SectionKey::GenreList,
        ));
        page.genre_artwork = if names.is_empty() {
            HashMap::new()
        } else {
            self.inner.art.batch(&names).await
        };
    }
}

/// The first 20 genre names of a genre shelf.
pub fn genre_names(section: Option<&ChartSection>) -> Vec<String> {
    section
        .map(|section| {
            section
                .items
                .iter()
                .filter_map(|item| match item {
                    SectionItem::Genre(genre) => Some(genre.name.clone()),
                    _ => None,
                })
                .take(ART_GENRES)
                .collect()
        })
        .unwrap_or_default()
}

/// The instance status with ListenBrainz and Last.fm as this user has
/// them linked (media servers and the library stay instance-wide).
fn user_status(mut status: IntegrationStatus, music: &UserMusic) -> IntegrationStatus {
    status.listenbrainz = music.listenbrainz.is_some();
    status.lastfm = music.lastfm;
    status
}

impl Warm for LiveHome {
    fn feature(&self) -> Feature {
        Feature::Home
    }

    fn warm<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(LiveHome::warm(self, user_id))
    }
}

/// What production home is built from.
pub struct HomeWiring {
    /// The database.
    pub db: Db,
    /// The kernel clock.
    pub clock: Arc<dyn Clock>,
    /// The integration flags, read per visit.
    pub integrations: IntegrationSettings,
    /// The listening reads the discover page also uses.
    pub listening: ListeningData,
    /// The discover pages, for the teaser.
    pub discover: DiscoverPages,
    /// Where built pages queue their image warm.
    pub warm: Option<CoverWarm>,
    /// The charts genre pages are added to.
    pub charts: super::charts::LiveCharts,
}

/// Production home: the page, the genre artwork shared with discover, and
/// the charts with genre pages.
pub fn wire(wiring: HomeWiring) -> (LiveHome, Arc<GenreArt>, genre::WithGenrePages) {
    let pool = wiring.db.sqlx().clone();
    let art = Arc::new(GenreArt::new(HomeLibrary::new(pool.clone())));
    let home = LiveHome::new(HomeInputs {
        listening: wiring.listening.clone(),
        db: wiring.db,
        clock: wiring.clock,
        integrations: wiring.integrations,
        art: art.clone(),
        warm: wiring.warm.clone(),
        discover: Some(wiring.discover),
    });
    let pages = Arc::new(genre::GenrePages::new(
        pool,
        wiring.listening,
        art.clone(),
        wiring.warm,
    ));
    let charts = genre::WithGenrePages::new(wiring.charts, pages);
    (home, art, charts)
}
