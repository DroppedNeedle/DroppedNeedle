//! The genre page (v2 `get_genre_artists`): what the library owns in the
//! genre, what is popular in it on MusicBrainz (paged), and its artwork.
//!
//! Opening a genre also warms the cover and artist image cache for the
//! popular rows in the background (v2 `GenreCoverPrewarmService`), so the
//! page fills in quickly on the next visit. The warm-up is polite: one run
//! per genre at a time, three genres at once, at most 100 images per run,
//! two seconds between downloads, and images already cached are skipped
//! without waiting.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::Semaphore;

use super::artwork::GenreArt;
use super::library::HomeLibrary;
use super::mark_owned;
use super::shelves::{library_album, library_artist};
use super::sources::HomeSources;
use crate::reads::discover::adapters::page::library::fold;
use crate::reads::discover::adapters::page::shelves::album;
use crate::reads::discover::models::{
    ChartAlbum, ChartArtist, ChartRange, ChartSource, GenreDetailResponse, GenreLibrarySection,
    GenrePopularSection, PopularAlbumsPage, TrendingArtistsPage,
};
use crate::reads::discover::ports::{BoxFuture, ChartsSource, ProviderFailure};
use crate::reads::platform::artwork::ArtworkService;

/// Owned rows per lane (v2).
const LIBRARY_ROWS: i64 = 50;
/// Images one warm-up run fetches at most.
const PREWARM_MAX: usize = 100;
/// Genres warming at once.
const PREWARM_PARALLEL: usize = 3;
/// Pause between two downloads of one run.
const PREWARM_PAUSE: Duration = Duration::from_secs(2);

/// Builds genre pages.
pub struct GenrePages {
    library: HomeLibrary,
    pool: SqlitePool,
    sources: Arc<dyn HomeSources>,
    art: Arc<GenreArt>,
    prewarm: Option<Prewarm>,
}

impl GenrePages {
    /// Pages over the library in `pool` and the provider reads in
    /// `sources`; `artwork`, when given, is warmed for popular rows.
    pub fn new(
        pool: SqlitePool,
        sources: Arc<dyn HomeSources>,
        art: Arc<GenreArt>,
        artwork: Option<ArtworkService>,
    ) -> Self {
        Self {
            library: HomeLibrary::new(pool.clone()),
            pool,
            sources,
            art,
            prewarm: artwork.map(Prewarm::new),
        }
    }

    /// One genre page. Never fails: a lane that cannot be read is empty,
    /// with a log line.
    pub async fn detail(
        &self,
        genre: &str,
        limit: i64,
        artist_offset: i64,
        album_offset: i64,
    ) -> GenreDetailResponse {
        let limit_u32 = u32::try_from(limit.clamp(1, 200)).unwrap_or(50);
        let offset = |value: i64| u32::try_from(value.max(0)).unwrap_or(u32::MAX);
        let (owned_artists, owned_albums, popular_artists, popular_albums, artwork) = tokio::join!(
            self.library.genre_artists(genre, LIBRARY_ROWS),
            self.library.genre_albums(genre, LIBRARY_ROWS),
            self.sources
                .musicbrainz_tag_artists(genre, limit_u32, offset(artist_offset)),
            self.sources
                .musicbrainz_tag_albums(genre, limit_u32, offset(album_offset)),
            self.art.one(genre),
        );
        let owned_artists = owned_artists.unwrap_or_else(|cause| {
            tracing::warn!(%cause, "owned genre artists unreadable");
            Vec::new()
        });
        let owned_albums = owned_albums.unwrap_or_else(|cause| {
            tracing::warn!(%cause, "owned genre albums unreadable");
            Vec::new()
        });
        let popular_artists = popular_artists.unwrap_or_else(|cause| {
            tracing::warn!(%cause, "musicbrainz genre artists unavailable");
            Vec::new()
        });
        let popular_albums = popular_albums.unwrap_or_else(|cause| {
            tracing::warn!(%cause, "musicbrainz genre albums unavailable");
            Vec::new()
        });
        let has_more_artists = popular_artists.len() >= limit_u32 as usize;
        let has_more_albums = popular_albums.len() >= limit_u32 as usize;
        let mut artists: Vec<ChartArtist> = popular_artists
            .iter()
            .map(|row| ChartArtist {
                name: row.name.clone(),
                mbid: row.mbid.clone(),
                local_id: None,
                image_url: None,
                listen_count: None,
                in_library: false,
                source: None,
            })
            .collect();
        let mut albums: Vec<ChartAlbum> = popular_albums
            .iter()
            .map(|row| {
                let mut item = album(
                    Some(&row.release_group_mbid),
                    &row.title,
                    Some(&row.artist_name),
                    None,
                    None,
                );
                item.release_date = row.year.map(|year| year.to_string());
                item
            })
            .collect();
        mark_owned(
            &self.pool,
            albums.iter_mut().collect(),
            artists.iter_mut().collect(),
        )
        .await;
        if let Some(prewarm) = &self.prewarm {
            prewarm.schedule(
                genre,
                artists.iter().filter_map(|row| row.mbid.clone()).collect(),
                albums.iter().filter_map(|row| row.mbid.clone()).collect(),
            );
        }
        GenreDetailResponse {
            genre: genre.to_owned(),
            genre_artwork: artwork,
            library: Some(GenreLibrarySection {
                artist_count: owned_artists.len() as i64,
                album_count: owned_albums.len() as i64,
                artists: owned_artists.iter().map(library_artist).collect(),
                albums: owned_albums.iter().map(library_album).collect(),
            }),
            popular: Some(GenrePopularSection {
                artists: artists.clone(),
                albums,
                has_more_artists,
                has_more_albums,
            }),
            total_count: Some(artists.len() as i64),
            artists,
        }
    }
}

/// Background warm-up of the images a genre page shows.
#[derive(Clone)]
struct Prewarm {
    artwork: ArtworkService,
    active: Arc<Mutex<HashSet<String>>>,
    permits: Arc<Semaphore>,
}

/// Forgets a genre's run however it ends.
struct ActiveGuard {
    active: Arc<Mutex<HashSet<String>>>,
    genre: String,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.genre);
        }
    }
}

impl Prewarm {
    fn new(artwork: ArtworkService) -> Self {
        Self {
            artwork,
            active: Arc::new(Mutex::new(HashSet::new())),
            permits: Arc::new(Semaphore::new(PREWARM_PARALLEL)),
        }
    }

    /// Start a run for the genre unless one is going.
    fn schedule(&self, genre: &str, artists: Vec<String>, albums: Vec<String>) {
        let genre = fold(genre);
        match self.active.lock() {
            Ok(mut active) => {
                if !active.insert(genre.clone()) {
                    return;
                }
            }
            Err(_) => {
                tracing::error!("genre warm-up state lock poisoned; skipping");
                return;
            }
        }
        let guard = ActiveGuard {
            active: self.active.clone(),
            genre,
        };
        let this = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            let Ok(_permit) = this.permits.clone().acquire_owned().await else {
                return;
            };
            this.run(artists, albums).await;
        });
    }

    async fn run(&self, artists: Vec<String>, albums: Vec<String>) {
        let artists: Vec<String> = artists.into_iter().take(PREWARM_MAX).collect();
        let albums: Vec<String> = albums
            .into_iter()
            .take(PREWARM_MAX.saturating_sub(artists.len()))
            .collect();
        let mut fetched = false;
        for mbid in &artists {
            if self.artwork.artist_image_cached(mbid).await {
                continue;
            }
            if fetched {
                tokio::time::sleep(PREWARM_PAUSE).await;
            }
            self.artwork.warm_artist_image(mbid).await;
            fetched = true;
        }
        for mbid in &albums {
            if self.artwork.release_group_cover_cached(mbid).await {
                continue;
            }
            if fetched {
                tokio::time::sleep(PREWARM_PAUSE).await;
            }
            self.artwork.warm_release_group_cover(mbid).await;
            fetched = true;
        }
    }
}

/// The chart source with real genre pages: charts go to `inner`, genre
/// pages to `pages`.
pub struct WithGenrePages {
    inner: Arc<dyn ChartsSource>,
    pages: Arc<GenrePages>,
}

impl WithGenrePages {
    /// Serve genre pages from `pages` and everything else from `inner`.
    pub fn new(inner: Arc<dyn ChartsSource>, pages: Arc<GenrePages>) -> Self {
        Self { inner, pages }
    }
}

impl ChartsSource for WithGenrePages {
    fn trending_artists(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>> {
        self.inner.trending_artists(range, limit, offset, source)
    }

    fn popular_albums(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>> {
        self.inner.popular_albums(range, limit, offset, source)
    }

    fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        self.inner
            .your_top_albums(user_id, range, limit, offset, source)
    }

    fn genre_detail<'a>(
        &'a self,
        genre: &'a str,
        limit: i64,
        artist_offset: i64,
        album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>> {
        Box::pin(async move {
            Ok(self
                .pages
                .detail(genre, limit, artist_offset, album_offset)
                .await)
        })
    }
}
