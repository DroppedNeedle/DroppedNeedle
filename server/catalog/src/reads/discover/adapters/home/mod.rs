//! The home page: library shelves at once, everything else built in the
//! background and kept fresh while people use it (v2 `HomeService`).
//!
//! A visit never waits on a provider. With a built page in memory it gets
//! that page, and when the page is more than five minutes old a rebuild
//! starts behind it (`refreshing`, which the page polls). Without one it
//! gets the local shelves straight from SQLite (recently added, your
//! albums and artists, recently played, the genre tiles and the discover
//! teaser) while the full build runs. A build that fails is not retried
//! for five minutes, so a dead provider does not cost every poll a build.
//!
//! The full build adds what the user's listening services know: trending
//! artists and popular albums (ListenBrainz, or Last.fm when that is the
//! user's primary source), the user's top albums and loved tracks, their
//! genres and the weekly exploration playlist. Genre tiles get artwork
//! from the user's own collection on every answer, so new art shows
//! without a rebuild.

pub mod artwork;
pub mod genre;
pub mod library;
pub mod shelves;
pub mod sources;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::SqlitePool;

use super::content::IntegrationSettings;
use super::ownership;
use super::page::build::genre_list;
use super::page::shelves::SHELF_SIZE;
use super::page::{FeatureWarmer, GENRE_ARTWORK_SCHEMA, LiveDiscover};
use super::queue::sources::{MusicSource, UserMusic};
use crate::reads::discover::models::{
    ChartAlbum, ChartArtist, ChartSection, DiscoverPreview, HomeResponse, IntegrationStatus,
    SectionItem,
};
use crate::reads::discover::ports::BoxFuture;
use artwork::GenreArt;
use library::HomeLibrary;
use sources::HomeSources;

/// How long a built page is kept.
const KEEP_SECS: f64 = 3600.0;
/// A page older than this is rebuilt behind the next visit, and a failed
/// build is not retried sooner.
const STALE_SECS: f64 = 300.0;
/// Rows a library read fetches.
const LIBRARY_ROWS: i64 = 15;
/// Rows a provider read asks for (v2).
const PROVIDER_ROWS: u32 = 20;
/// Bound on one provider read in a full build.
const READ_TIMEOUT: Duration = Duration::from_secs(25);
/// Genres whose tiles get artwork.
const ART_GENRES: usize = 20;

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

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

/// A page with no shelves.
fn empty_page(status: IntegrationStatus, refreshing: bool) -> HomeResponse {
    HomeResponse {
        recently_added: None,
        library_artists: None,
        library_albums: None,
        recommended_artists: None,
        trending_artists: None,
        popular_albums: None,
        recently_played: None,
        top_genres: None,
        genre_list: None,
        fresh_releases: None,
        favorite_artists: None,
        your_top_albums: None,
        weekly_exploration: None,
        service_prompts: Vec::new(),
        integration_status: Some(status),
        genre_artwork: HashMap::new(),
        genre_artwork_schema_version: GENRE_ARTWORK_SCHEMA.to_owned(),
        discover_preview: None,
        service_status: None,
        refreshing,
    }
}

/// One built page in memory.
struct Kept {
    page: HomeResponse,
    expires: f64,
}

/// What home keeps between requests.
#[derive(Default)]
struct State {
    pages: HashMap<String, Kept>,
    /// When each key's last build finished, and whether it worked.
    attempts: HashMap<String, (f64, bool)>,
    /// Users with a build running.
    building: HashSet<String>,
}

struct Inner {
    sources: Arc<dyn HomeSources>,
    library: HomeLibrary,
    pool: SqlitePool,
    art: Arc<GenreArt>,
    integrations: IntegrationSettings,
    state: Mutex<State>,
}

/// The live home page.
#[derive(Clone)]
pub struct LiveHome {
    inner: Arc<Inner>,
}

/// Clears a user's "building" mark however the build ends.
struct BuildGuard {
    inner: Arc<Inner>,
    user_id: String,
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.building.remove(&self.user_id);
        }
    }
}

/// The cache key: the user plus which services are linked, so linking or
/// unlinking one starts a fresh page.
fn key_for(user_id: &str, music: &UserMusic) -> String {
    format!(
        "{user_id}:{}:{}",
        music.listenbrainz.is_some(),
        music.lastfm
    )
}

impl LiveHome {
    /// Home over the provider reads in `sources`, the library in `pool`
    /// and the integration flags in `integrations`, with genre art from `art`.
    pub fn new(
        sources: Arc<dyn HomeSources>,
        pool: SqlitePool,
        integrations: IntegrationSettings,
        art: Arc<GenreArt>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                sources,
                library: HomeLibrary::new(pool.clone()),
                pool,
                art,
                integrations,
                state: Mutex::new(State::default()),
            }),
        }
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, State>> {
        match self.inner.state.lock() {
            Ok(state) => Some(state),
            Err(_) => {
                tracing::error!("home state lock poisoned");
                None
            }
        }
    }

    /// The user's home page. `discover`, when given, supplies the discover
    /// teaser from its last built page.
    pub async fn page(&self, user_id: &str, discover: Option<&LiveDiscover>) -> HomeResponse {
        let music = self.inner.sources.user_music(user_id).await;
        let key = key_for(user_id, &music);
        let now = now_secs();
        let (kept, attempt, mut building) = match self.lock() {
            Some(mut state) => {
                state.pages.retain(|_, kept| kept.expires > now);
                state.attempts.retain(|_, (at, _)| now - *at <= KEEP_SECS);
                (
                    state.pages.get(&key).map(|kept| kept.page.clone()),
                    state.attempts.get(&key).copied(),
                    state.building.contains(user_id),
                )
            }
            None => (None, None, false),
        };
        let mut page = match kept {
            Some(page) => {
                let built_at = attempt.map_or(0.0, |(at, _)| at);
                if !building && now - built_at > STALE_SECS {
                    self.trigger_build(user_id, discover);
                    building = true;
                }
                page
            }
            None => {
                let failed_recently = attempt.is_some_and(|(at, ok)| !ok && now - at <= STALE_SECS);
                if !building && !failed_recently {
                    self.trigger_build(user_id, discover);
                    building = true;
                }
                let status = self.inner.integrations.status();
                self.build_local(user_id, &music, status, discover).await
            }
        };
        page.refreshing = building;
        self.apply_artwork(&mut page).await;
        page
    }

    /// Start a background build for the user unless one is running.
    fn trigger_build(&self, user_id: &str, discover: Option<&LiveDiscover>) {
        let home = self.clone();
        let user_id = user_id.to_owned();
        let discover = discover.cloned();
        tokio::spawn(async move {
            home.build(&user_id, discover.as_ref()).await;
        });
    }

    /// Build the user's full page now and keep it. Returns whether a page
    /// was kept.
    pub async fn build(&self, user_id: &str, discover: Option<&LiveDiscover>) -> bool {
        {
            let Some(mut state) = self.lock() else {
                return false;
            };
            if !state.building.insert(user_id.to_owned()) {
                return false;
            }
        }
        let _guard = BuildGuard {
            inner: self.inner.clone(),
            user_id: user_id.to_owned(),
        };
        let source = self.inner.sources.source_key();
        let music = self.inner.sources.user_music(user_id).await;
        let key = key_for(user_id, &music);
        let status = self.inner.integrations.status();
        let page = self.build_full(user_id, &music, status, discover).await;
        let ok = self.inner.sources.source_key() == source;
        if !ok {
            tracing::info!("MusicBrainz source changed during a home build; dropping it");
        }
        let now = now_secs();
        if let Some(mut state) = self.lock() {
            if ok {
                state.pages.insert(
                    key.clone(),
                    Kept {
                        page,
                        expires: now + KEEP_SECS,
                    },
                );
            }
            state.attempts.insert(key, (now, ok));
        }
        ok
    }

    /// The local first paint: shelves from SQLite only.
    async fn build_local(
        &self,
        user_id: &str,
        music: &UserMusic,
        status: IntegrationStatus,
        discover: Option<&LiveDiscover>,
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
        page.discover_preview = Self::discover_preview(user_id, discover).await;
        mark_page(&self.inner.pool, &mut page).await;
        page
    }

    /// The full page: the local shelves plus what the user's listening
    /// services know.
    async fn build_full(
        &self,
        user_id: &str,
        music: &UserMusic,
        status: IntegrationStatus,
        discover: Option<&LiveDiscover>,
    ) -> HomeResponse {
        let sources = self.inner.sources.as_ref();
        let lastfm_primary = music.resolved_source() == MusicSource::LastFm;
        let lb_user = music.listenbrainz.as_deref();
        let lfm_user = music.lastfm_username.as_deref().filter(|_| music.lastfm);
        let charts = async {
            if lastfm_primary {
                let artists = bounded(
                    "last.fm chart",
                    sources.lastfm_trending_artists(user_id, PROVIDER_ROWS),
                )
                .await;
                (shelves::trending_artists(&artists, "lastfm"), None)
            } else {
                let (artists, albums) = tokio::join!(
                    bounded(
                        "listenbrainz trending artists",
                        sources.listenbrainz_trending_artists(PROVIDER_ROWS)
                    ),
                    bounded(
                        "listenbrainz popular albums",
                        sources.listenbrainz_popular_albums(PROVIDER_ROWS)
                    ),
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
                        bounded(
                            "listenbrainz top albums",
                            sources.listenbrainz_user_albums(username, PROVIDER_ROWS)
                        ),
                        bounded(
                            "listenbrainz loved",
                            sources.listenbrainz_loved(username, PROVIDER_ROWS)
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
                            sources.lastfm_user_albums(user_id, username, PROVIDER_ROWS)
                        ),
                        bounded(
                            "last.fm loved",
                            sources.lastfm_loved(user_id, username, PROVIDER_ROWS)
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
                            sources.listenbrainz_weekly(user_id, username),
                        )
                        .await
                        .unwrap_or_else(|_| {
                            tracing::warn!("weekly exploration timed out on home");
                            None
                        })
                    };
                    tokio::join!(
                        bounded("listenbrainz genres", sources.listenbrainz_genres(username)),
                        weekly,
                    )
                }
                None => (Vec::new(), None),
            }
        };
        let (local, (trending, popular), (top, favorites), (genres, weekly)) = tokio::join!(
            self.build_local(user_id, music, status, discover),
            charts,
            personal,
            listenbrainz_extras,
        );
        let mut page = local;
        page.trending_artists = trending;
        page.popular_albums = popular;
        page.your_top_albums = top;
        page.favorite_artists = favorites;
        page.weekly_exploration = weekly;
        page.genre_list = genre_list(Some(&genres));
        mark_page(&self.inner.pool, &mut page).await;
        page
    }

    /// The discover teaser: the first "because you listen to" shelf of the
    /// user's last discover page.
    async fn discover_preview(
        user_id: &str,
        discover: Option<&LiveDiscover>,
    ) -> Option<DiscoverPreview> {
        let page = discover?.cached_page(user_id).await?;
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

impl FeatureWarmer for LiveHome {
    fn warm<'a>(&'a self, user_id: &'a str, discover: &'a LiveDiscover) -> BoxFuture<'a, bool> {
        Box::pin(self.build(user_id, Some(discover)))
    }
}

/// What production home is built from.
pub struct HomeWiring {
    /// The reader pool.
    pub pool: SqlitePool,
    /// The integration flags, read per build.
    pub integrations: IntegrationSettings,
    /// The live provider reads the discover page also uses.
    pub sources: Arc<dyn HomeSources>,
    /// What the discover page is built from. Home shows its teaser, and
    /// its warm cycle warms home.
    pub discover: super::page::PageInputs,
    /// The chart source genre pages are added to.
    pub charts: Arc<dyn crate::reads::discover::ports::ChartsSource>,
    /// The image cache genre pages warm, when there is one.
    pub artwork: Option<crate::reads::platform::artwork::ArtworkService>,
}

/// Production home: the page, the discover page whose warm cycle warms it,
/// the genre artwork shared with discover, and the chart source with genre
/// pages.
pub fn wire(
    wiring: HomeWiring,
) -> (
    LiveHome,
    LiveDiscover,
    Arc<GenreArt>,
    Arc<dyn crate::reads::discover::ports::ChartsSource>,
) {
    let art = Arc::new(GenreArt::new(HomeLibrary::new(wiring.pool.clone())));
    let home = LiveHome::new(
        wiring.sources.clone(),
        wiring.pool.clone(),
        wiring.integrations,
        art.clone(),
    );
    let discover = LiveDiscover::with_home(wiring.discover, Arc::new(home.clone()));
    let pages = Arc::new(genre::GenrePages::new(
        wiring.pool,
        wiring.sources,
        art.clone(),
        wiring.artwork,
    ));
    let charts = Arc::new(genre::WithGenrePages::new(wiring.charts, pages));
    (home, discover, art, charts)
}
