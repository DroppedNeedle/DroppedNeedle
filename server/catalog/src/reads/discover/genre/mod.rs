//! The genre page (v2 `get_genre_artists`): what the library owns in the
//! genre, what is popular in it on MusicBrainz (paged), and its artwork.
//!
//! Opening a genre also queues a background warm of the popular rows'
//! covers and artist images (v2 `GenreCoverPrewarmService`), so the page
//! fills in quickly on the next visit.

pub mod artwork;

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::reads::discover::charts::LiveCharts;
use crate::reads::discover::home::library::HomeLibrary;
use crate::reads::discover::home::mark_owned;
use crate::reads::discover::home::shelves::{library_album, library_artist};
use crate::reads::discover::listening::{Ask, ListeningData};
use crate::reads::discover::models::{
    AlbumRef, ArtistRef, ChartPage, GenreDetailResponse, GenreLibrarySection, GenrePopularSection,
    ListenSource, StatsRange,
};
use crate::reads::discover::page::shelves::album;
use crate::reads::discover::ports::{BoxFuture, ProviderFailure};
use crate::reads::platform::artwork::warm::{CoverWarm, WarmTargets};
use artwork::GenreArt;

/// Popular rows per lane at most.
const MAX_ROWS: i64 = 100;
/// Owned rows per lane (v2).
const LIBRARY_ROWS: i64 = 50;

/// Builds genre pages.
pub struct GenrePages {
    library: HomeLibrary,
    pool: SqlitePool,
    listening: ListeningData,
    art: Arc<GenreArt>,
    warm: Option<CoverWarm>,
}

impl GenrePages {
    /// Pages over the library in `pool` and the provider reads in
    /// `listening`; `warm`, when given, warms the popular rows' images.
    pub fn new(
        pool: SqlitePool,
        listening: ListeningData,
        art: Arc<GenreArt>,
        warm: Option<CoverWarm>,
    ) -> Self {
        Self {
            library: HomeLibrary::new(pool.clone()),
            pool,
            listening,
            art,
            warm,
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
        let limit_u32 = u32::try_from(limit.clamp(1, MAX_ROWS)).unwrap_or(50);
        let offset = |value: i64| u32::try_from(value.max(0)).unwrap_or(u32::MAX);
        let ask = Ask::person();
        let (owned_artists, owned_albums, popular_artists, popular_albums, artwork) = tokio::join!(
            self.library.genre_artists(genre, LIBRARY_ROWS),
            self.library.genre_albums(genre, LIBRARY_ROWS),
            self.listening
                .tag_artists(genre, limit_u32, offset(artist_offset), &ask),
            self.listening
                .tag_albums(genre, limit_u32, offset(album_offset), &ask),
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
        let mut artists: Vec<ArtistRef> = popular_artists
            .iter()
            .map(|row| ArtistRef {
                name: row.name.clone(),
                musicbrainz_id: row.mbid.clone(),
                local_id: None,
                image_url: None,
                listen_count: None,
                in_library: false,
            })
            .collect();
        let mut albums: Vec<AlbumRef> = popular_albums
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
        if let Some(warm) = &self.warm {
            let mut targets = WarmTargets::default();
            for row in &albums {
                targets.cover(row.musicbrainz_id.as_deref());
            }
            for row in &artists {
                targets.artist(row.musicbrainz_id.as_deref());
            }
            warm.queue("genre", targets);
        }
        let total_count = Some(artists.len() as i64);
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
                artists,
                albums,
                has_more_artists,
                has_more_albums,
            }),
            total_count,
        }
    }
}

/// The charts with real genre pages: charts go to `inner`, genre pages to
/// `pages`.
pub struct WithGenrePages {
    inner: LiveCharts,
    pages: Arc<GenrePages>,
}

impl WithGenrePages {
    /// Serve genre pages from `pages` and everything else from `inner`.
    pub fn new(inner: LiveCharts, pages: Arc<GenrePages>) -> Self {
        Self { inner, pages }
    }

    /// One trending-artists page for the user.
    pub fn trending_artists<'a>(
        &'a self,
        user_id: &'a str,
        range: StatsRange,
        limit: i64,
        offset: i64,
        source: ListenSource,
    ) -> BoxFuture<'a, Result<ChartPage<ArtistRef>, ProviderFailure>> {
        self.inner
            .trending_artists(user_id, range, limit, offset, source)
    }

    /// One popular-albums page for the user.
    pub fn popular_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: StatsRange,
        limit: i64,
        offset: i64,
        source: ListenSource,
    ) -> BoxFuture<'a, Result<ChartPage<AlbumRef>, ProviderFailure>> {
        self.inner
            .popular_albums(user_id, range, limit, offset, source)
    }

    /// One your-top-albums page for the user.
    pub fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: StatsRange,
        limit: i64,
        offset: i64,
        source: ListenSource,
    ) -> BoxFuture<'a, Result<ChartPage<AlbumRef>, ProviderFailure>> {
        self.inner
            .your_top_albums(user_id, range, limit, offset, source)
    }

    /// Genre detail with owned and popular rows.
    pub fn genre_detail<'a>(
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
