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

pub mod library;
pub mod shelves;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::cache::{PageKind, Timing, UserPageCache, Variant};
use super::content::IntegrationSettings;
use super::listening::{Ask, ListenedAlbum, ListeningData, MusicSource, StatsRange, UserMusic};
use super::ownership;
use super::page::build::{genre_list, weekly_exploration};
use super::page::shelves::SHELF_SIZE;
use super::page::{DiscoverPages, GENRE_ARTWORK_SCHEMA, add_section};
use super::scheduler::{Feature, Warm};
use crate::reads::discover::genre::{self, artwork::GenreArt};
use crate::reads::discover::models::{
    ChartAlbum, ChartArtist, ChartSection, DiscoverPreview, HomeResponse, IntegrationStatus,
    SectionItem, WeeklyExploration,
};
use crate::reads::discover::ports::BoxFuture;
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
    albums: Vec<&mut ChartAlbum>,
    artists: Vec<&mut ChartArtist>,
) {
    let album_ids: Vec<&str> = albums
        .iter()
        .filter_map(|row| row.mbid.as_deref())
        .collect();
    let artist_ids: Vec<&str> = artists
        .iter()
        .filter_map(|row| row.mbid.as_deref())
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
            .mbid
            .as_deref()
            .and_then(|mbid| owned_albums.get(&mbid.to_ascii_lowercase()))
        {
            row.in_library = true;
            row.local_id.get_or_insert_with(|| local.clone());
        }
    }
    for row in artists {
        if let Some(local) = row
            .mbid
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
    let mut albums: Vec<&mut ChartAlbum> = Vec::new();
    let mut artists: Vec<&mut ChartArtist> = Vec::new();
    let shelves = [
        page.recently_added.as_mut(),
        page.library_artists.as_mut(),
        page.library_albums.as_mut(),
        page.trending_artists.as_mut(),
        page.popular_albums.as_mut(),
        page.your_top_albums.as_mut(),
    ];
    for section in shelves.into_iter().flatten() {
        for item in &mut section.items {
            match item {
                SectionItem::Album(row) => albums.push(row),
                SectionItem::Artist(row) => artists.push(row),
                _ => {}
            }
        }
    }
    if let Some(preview) = page.discover_preview.as_mut() {
        artists.extend(preview.items.iter_mut());
    }
    mark_owned(pool, albums, artists).await;
}

/// The covers and artist images a home page shows, top to bottom: the
/// local shelves of `page` and a build's `shelves`.
fn warm_targets(page: &HomeResponse, shelves: &ProviderShelves) -> WarmTargets {
    let mut targets = WarmTargets::default();
    let sections = [
        &page.recently_added,
        &page.library_albums,
        &page.library_artists,
        &shelves.trending_artists,
        &shelves.popular_albums,
        &shelves.favorite_artists,
        &shelves.your_top_albums,
    ];
    for section in sections.into_iter().flatten() {
        add_section(&mut targets, section);
    }
    if let Some(weekly) = &shelves.weekly_exploration {
        for track in &weekly.tracks {
            targets.cover(track.release_group_mbid.as_deref());
        }
    }
    if let Some(preview) = &page.discover_preview {
        for artist in &preview.items {
            targets.artist(artist.mbid.as_deref());
        }
    }
    targets
}

/// A page with no shelves.
fn empty_page(status: IntegrationStatus, refreshing: bool) -> HomeResponse {
    HomeResponse {
        recently_added: None,
        library_artists: None,
        library_albums: None,
        trending_artists: None,
        popular_albums: None,
        recently_played: None,
        genre_list: None,
        favorite_artists: None,
        your_top_albums: None,
        weekly_exploration: None,
        service_prompts: Vec::new(),
        integration_status: Some(status),
        genre_artwork: HashMap::new(),
        genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
        discover_preview: None,
        refreshing,
    }
}

/// The shelves the user's listening services fill: what a home build
/// makes and the page cache keeps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderShelves {
    trending_artists: Option<ChartSection>,
    popular_albums: Option<ChartSection>,
    your_top_albums: Option<ChartSection>,
    favorite_artists: Option<ChartSection>,
    weekly_exploration: Option<WeeklyExploration>,
    genre_list: Option<ChartSection>,
}

impl ProviderShelves {
    fn has_content(&self) -> bool {
        self.trending_artists.is_some()
            || self.popular_albums.is_some()
            || self.your_top_albums.is_some()
            || self.favorite_artists.is_some()
            || self.weekly_exploration.is_some()
    }

    fn apply(self, page: &mut HomeResponse) {
        page.trending_artists = self.trending_artists;
        page.popular_albums = self.popular_albums;
        page.your_top_albums = self.your_top_albums;
        page.favorite_artists = self.favorite_artists;
        page.weekly_exploration = self.weekly_exploration;
        if self.genre_list.is_some() {
            page.genre_list = self.genre_list;
        }
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
        let music = self.inner.listening.accounts(user_id).await;
        let variant = Variant::of(&self.inner.listening, &music);
        let saved = self.inner.cache.load(user_id, &variant).await;
        let started =
            self.inner.cache.due(user_id, saved.as_ref()) && self.start(user_id, variant).await;
        let refreshing = started || self.inner.cache.building(user_id).is_some();
        let status = self.inner.integrations.status();
        let mut page = self.local(user_id, &music, status).await;
        if let Some(saved) = saved {
            saved.value.apply(&mut page);
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
        let source = self.inner.listening.namespace();
        let music = self.inner.listening.accounts(user_id).await;
        let shelves = self.build_shelves(user_id, &music).await;
        if self.inner.listening.namespace() != source {
            tracing::info!("MusicBrainz source changed during a home build; dropping it");
            return None;
        }
        if !shelves.has_content() {
            tracing::warn!(user = %user_id, "home build found nothing to show; keeping the previous shelves");
            return None;
        }
        if let Some(warm) = &self.inner.warm {
            let local = self
                .local(user_id, &music, self.inner.integrations.status())
                .await;
            warm.queue("home", warm_targets(&local, &shelves));
        }
        Some(shelves)
    }

    /// The local shelves: SQLite only.
    async fn local(
        &self,
        user_id: &str,
        music: &UserMusic,
        status: IntegrationStatus,
    ) -> HomeResponse {
        let status = user_status(status, music);
        let library = &self.inner.library;
        let (recent, artists, plays) = tokio::join!(
            bounded("recent albums", library.recent_albums(LIBRARY_ROWS)),
            bounded("library artists", library.top_artists(LIBRARY_ROWS)),
            bounded("play history", library.recent_plays(user_id, LIBRARY_ROWS)),
        );
        let mut page = empty_page(status.clone(), false);
        page.recently_added = shelves::recently_added(&recent);
        page.library_albums = shelves::your_albums(&recent);
        page.library_artists = shelves::your_artists(&artists);
        page.recently_played = shelves::recently_played(&plays);
        page.genre_list = genre_list(None);
        page.service_prompts =
            shelves::service_prompts(status.listenbrainz, status.download_client, status.lastfm);
        page.discover_preview = self.discover_preview(user_id).await;
        page
    }

    /// The shelves what the user's listening services know fill.
    async fn build_shelves(&self, user_id: &str, music: &UserMusic) -> ProviderShelves {
        let listening = &self.inner.listening;
        let ask = Ask::build(droppedneedle_providers::RequestPriority::BackgroundSync);
        let ask = &ask;
        let lastfm_primary = listening.policy_for(music).listening() == MusicSource::LastFm;
        let lb_user = music.listenbrainz.as_deref();
        let lfm_user = music.lastfm_username.as_deref().filter(|_| music.lastfm);
        let charts = async {
            if lastfm_primary {
                let artists = bounded(
                    "last.fm chart",
                    listening.lastfm_chart_artists(user_id, PROVIDER_ROWS, ask),
                )
                .await;
                (shelves::trending_artists(&artists, "lastfm"), None)
            } else {
                let (artists, albums) = tokio::join!(
                    bounded(
                        "listenbrainz trending artists",
                        listening.sitewide_artists(StatsRange::ThisWeek, PROVIDER_ROWS, 0, ask)
                    ),
                    bounded("listenbrainz popular albums", async {
                        listening
                            .sitewide_albums(StatsRange::ThisWeek, PROVIDER_ROWS, 0, ask)
                            .await
                            .map(ranked)
                    }),
                );
                (
                    shelves::trending_artists(&artists, "listenbrainz"),
                    shelves::popular_albums(&albums),
                )
            }
        };
        let personal = async {
            match (lastfm_primary, lb_user, lfm_user) {
                (false, Some(username), _) => {
                    let (top, loved) = tokio::join!(
                        bounded("listenbrainz top albums", async {
                            listening
                                .user_albums(username, StatsRange::ThisMonth, PROVIDER_ROWS, 0, ask)
                                .await
                                .map(ranked)
                        }),
                        bounded(
                            "listenbrainz loved",
                            listening.loved(username, PROVIDER_ROWS, ask)
                        ),
                    );
                    (
                        shelves::your_top_albums(&top),
                        shelves::favorites(&loved, "listenbrainz"),
                    )
                }
                (true, _, Some(username)) => {
                    let (top, loved) = tokio::join!(
                        bounded(
                            "last.fm top albums",
                            listening.lastfm_user_albums(
                                user_id,
                                username,
                                "1month",
                                PROVIDER_ROWS,
                                ask
                            )
                        ),
                        bounded(
                            "last.fm loved",
                            listening.lastfm_loved(user_id, username, PROVIDER_ROWS, ask)
                        ),
                    );
                    (
                        shelves::your_top_albums_lastfm(&top),
                        shelves::favorites(&loved, "lastfm"),
                    )
                }
                _ => (None, None),
            }
        };
        let listenbrainz_extras = async {
            match lb_user {
                Some(username) => {
                    let weekly = async {
                        tokio::time::timeout(
                            READ_TIMEOUT,
                            weekly_exploration(listening, ask, user_id, username),
                        )
                        .await
                        .unwrap_or_else(|_| {
                            tracing::warn!("weekly exploration timed out on home");
                            None
                        })
                    };
                    tokio::join!(
                        bounded("listenbrainz genres", listening.genres(username, ask)),
                        weekly,
                    )
                }
                None => (Vec::new(), None),
            }
        };
        let ((trending, popular), (top, favorites), (genres, weekly)) =
            tokio::join!(charts, personal, listenbrainz_extras);
        ProviderShelves {
            trending_artists: trending,
            popular_albums: popular,
            your_top_albums: top,
            favorite_artists: favorites,
            weekly_exploration: weekly,
            genre_list: (!genres.is_empty())
                .then(|| genre_list(Some(&genres)))
                .flatten(),
        }
    }

    /// The discover teaser: the first "because you listen to" shelf of the
    /// user's last discover page.
    async fn discover_preview(&self, user_id: &str) -> Option<DiscoverPreview> {
        let page = self.inner.discover.as_ref()?.cached_page(user_id).await?;
        let first = page.because_you_listen_to.into_iter().next()?;
        Some(DiscoverPreview {
            seed_artist: first.seed_artist,
            seed_artist_mbid: first.seed_artist_mbid,
            items: first
                .section
                .items
                .into_iter()
                .take(SHELF_SIZE)
                .filter_map(|item| match item {
                    SectionItem::Artist(artist) => Some(artist),
                    _ => None,
                })
                .collect(),
        })
    }

    async fn apply_artwork(&self, page: &mut HomeResponse) {
        let names = genre_names(page.genre_list.as_ref());
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
