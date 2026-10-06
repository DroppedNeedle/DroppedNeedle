//! Home charts from ListenBrainz statistics.
//!
//! Ports v2's `HomeChartsService` range pages: trending artists and popular
//! albums from the sitewide stats, "your top albums" from the user's own
//! stats through their linked ListenBrainz account, one row more than asked
//! for to learn whether another page follows, and every row marked when the
//! library holds it (best effort: a failed lookup leaves rows unmarked). As in v2, artists without an MBID are dropped (they
//! cannot link anywhere) and a user without a linked account gets an empty
//! page.
//!
//! Last.fm charts need the instance Last.fm API key, which this server does
//! not hold yet, so `source=lastfm` answers "not available". Genre pages
//! need the genre index and tag search, not ported yet, and answer the same.

use std::sync::Arc;

use sqlx::SqlitePool;

use super::ownership::{owned_albums, owned_artists};
use crate::plugins::scrobble::ListenBrainzLinkStore;
use crate::providers::adapters::{CorePacer, CoreSink};
use crate::providers::listenbrainz::{
    ListenBrainzClient, Outcome,
    stats::{ArtistStat, ReleaseGroupStat},
};
use crate::reads::discover::{
    models::{
        ChartAlbum, ChartArtist, ChartRange, ChartSource, GenreDetailResponse, PopularAlbumsPage,
        TrendingArtistsPage,
    },
    ports::{BoxFuture, ChartsSource, ProviderFailure},
};

/// Live ListenBrainz charts.
pub struct ListenBrainzCharts {
    client: ListenBrainzClient<CorePacer, CoreSink>,
    links: Arc<dyn ListenBrainzLinkStore>,
    pool: SqlitePool,
}

impl ListenBrainzCharts {
    /// Charts over one paced client, the users' links, and the catalog.
    pub fn new(
        client: ListenBrainzClient<CorePacer, CoreSink>,
        links: Arc<dyn ListenBrainzLinkStore>,
        pool: SqlitePool,
    ) -> Self {
        Self {
            client,
            links,
            pool,
        }
    }

    async fn artist_page(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        rows: Vec<ArtistStat>,
    ) -> Result<TrendingArtistsPage, ProviderFailure> {
        let mut artists: Vec<ChartArtist> = rows
            .into_iter()
            .filter_map(|row| {
                let mbid = row.artist_mbids.into_iter().next()?;
                Some(ChartArtist {
                    name: row.artist_name,
                    mbid: Some(mbid),
                    local_id: None,
                    image_url: None,
                    listen_count: Some(row.listen_count),
                    in_library: false,
                    source: None,
                })
            })
            .collect();
        let mbids: Vec<&str> = artists.iter().filter_map(|a| a.mbid.as_deref()).collect();
        let owned = best_effort(owned_artists(&self.pool, &mbids).await);
        for artist in &mut artists {
            if let Some(local) = artist
                .mbid
                .as_deref()
                .and_then(|mbid| owned.get(&mbid.to_ascii_lowercase()))
            {
                artist.in_library = true;
                artist.local_id = Some(local.clone());
            }
        }
        let has_more = artists.len() as i64 > limit;
        artists.truncate(usize::try_from(limit).unwrap_or(0));
        Ok(TrendingArtistsPage {
            range_key: range.as_str().to_owned(),
            label: range.label().to_owned(),
            items: artists,
            offset,
            limit,
            has_more,
        })
    }

    async fn album_page(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        rows: Vec<ReleaseGroupStat>,
    ) -> Result<PopularAlbumsPage, ProviderFailure> {
        let mut albums: Vec<ChartAlbum> = rows
            .into_iter()
            .map(|row| ChartAlbum {
                name: row.release_group_name,
                mbid: row.release_group_mbid,
                local_id: None,
                artist_name: Some(row.artist_name),
                artist_mbid: row.artist_mbids.into_iter().next(),
                image_url: None,
                release_date: None,
                listen_count: Some(row.listen_count),
                in_library: false,
                requested: false,
                source: None,
            })
            .collect();
        let mbids: Vec<&str> = albums.iter().filter_map(|a| a.mbid.as_deref()).collect();
        let owned = best_effort(owned_albums(&self.pool, &mbids).await);
        for album in &mut albums {
            if let Some(local) = album
                .mbid
                .as_deref()
                .and_then(|mbid| owned.get(&mbid.to_ascii_lowercase()))
            {
                album.in_library = true;
                album.local_id = Some(local.clone());
            }
        }
        let has_more = albums.len() as i64 > limit;
        albums.truncate(usize::try_from(limit).unwrap_or(0));
        Ok(PopularAlbumsPage {
            range_key: range.as_str().to_owned(),
            label: range.label().to_owned(),
            items: albums,
            offset,
            limit,
            has_more,
        })
    }
}

/// Ownership marks are a nicety: a failed lookup is logged and the page
/// renders unmarked, as in v2.
fn best_effort(
    owned: Result<std::collections::HashMap<String, String>, ProviderFailure>,
) -> std::collections::HashMap<String, String> {
    owned.unwrap_or_else(|failure| {
        tracing::warn!(%failure, "chart ownership lookup failed; rows stay unmarked");
        std::collections::HashMap::new()
    })
}

/// One row past the page tells whether another page follows (v2).
fn window(limit: i64, offset: i64) -> (u32, u32) {
    (
        u32::try_from(limit + 1).unwrap_or(u32::MAX),
        u32::try_from(offset).unwrap_or(0),
    )
}

/// Found rows, an authoritative empty answer, or the provider failure.
fn rows<T>(outcome: Outcome<Vec<T>>) -> Result<Vec<T>, ProviderFailure> {
    match outcome {
        Outcome::Found(rows) => Ok(rows),
        Outcome::Missing => Ok(Vec::new()),
        Outcome::Unavailable { message, .. } => Err(ProviderFailure::failed(message)),
    }
}

fn lastfm_unavailable<T>() -> Result<T, ProviderFailure> {
    Err(ProviderFailure::not_configured(
        "Last.fm charts need the instance Last.fm API key, which this version does not hold yet. Use ListenBrainz charts.",
    ))
}

impl ChartsSource for ListenBrainzCharts {
    fn trending_artists(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>> {
        Box::pin(async move {
            if source == ChartSource::Lastfm {
                return lastfm_unavailable();
            }
            let (count, skip) = window(limit, offset);
            let found = rows(
                self.client
                    .sitewide_top_artists(range.as_str(), count, skip)
                    .await,
            )?;
            self.artist_page(range, limit, offset, found).await
        })
    }

    fn popular_albums(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async move {
            if source == ChartSource::Lastfm {
                return lastfm_unavailable();
            }
            let (count, skip) = window(limit, offset);
            let found = rows(
                self.client
                    .sitewide_top_release_groups(range.as_str(), count, skip)
                    .await,
            )?;
            self.album_page(range, limit, offset, found).await
        })
    }

    fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async move {
            if source == ChartSource::Lastfm {
                return lastfm_unavailable();
            }
            let found = match self.links.status(user_id).await {
                Some(link) => {
                    let (count, skip) = window(limit, offset);
                    rows(
                        self.client
                            .user_top_release_groups(&link.username, range.as_str(), count, skip)
                            .await,
                    )?
                }
                None => Vec::new(),
            };
            self.album_page(range, limit, offset, found).await
        })
    }

    fn genre_detail<'a>(
        &'a self,
        _genre: &'a str,
        _limit: i64,
        _artist_offset: i64,
        _album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_built(
                "Genre pages are not built in this version yet.",
            ))
        })
    }
}
