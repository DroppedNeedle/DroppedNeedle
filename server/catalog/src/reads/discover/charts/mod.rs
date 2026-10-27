//! Home charts from ListenBrainz statistics and Last.fm.
//!
//! Ports v2's `HomeChartsService` range pages. Every row is marked when the
//! library holds it (best effort: a failed lookup leaves rows unmarked).
//! Every read goes through the listening data, so a chart shares its cache
//! with home and Discover: sitewide pages are kept an hour, a user's own
//! five minutes.
//!
//! ListenBrainz (the default source): trending artists and popular albums
//! come from the sitewide stats, "your top albums" from the user's own
//! stats through their linked ListenBrainz account. One row more than asked
//! for tells whether another page follows. As in v2, artists without an
//! MBID are dropped (they cannot link anywhere). When ListenBrainz is down
//! and the user can read Last.fm, trending artists and "your top albums"
//! come from Last.fm instead, marked as Last.fm rows.
//!
//! Last.fm (`source=lastfm`): trending artists come from Last.fm's chart,
//! read with the user's own key, else the instance key. With Last.fm
//! switched off or no key, these routes answer "not configured". Last.fm
//! has no sitewide album chart; v2 showed the admin's own Last.fm account
//! there, which this server does not keep, so popular albums from Last.fm
//! are an empty page.
//!
//! As in v2, "your top albums" fall back to whichever service the user
//! has linked, and a user with neither gets an empty page. Genre pages are
//! served by the genre pages in front of this source.

use sqlx::SqlitePool;

use super::ownership::{owned_albums, owned_artists};
use crate::reads::discover::listening::{
    ArtistRow, Ask, LastFmChartAlbum, ListenedAlbum, ListeningData, ListeningError, MusicSource,
    SourcePolicy, StatsRange,
};
use crate::reads::discover::{
    models::{
        ChartAlbum, ChartArtist, ChartRange, ChartSource, PopularAlbumsPage, TrendingArtistsPage,
    },
    ports::{BoxFuture, ProviderFailure},
};
use droppedneedle_providers::lastfm;

/// Most rows one Last.fm range page reads (v2 `min(limit + offset + 1, 200)`).
const LASTFM_MAX_ROWS: i64 = 200;

/// The sentence a Last.fm chart answers while Last.fm cannot be read.
const LASTFM_OFF: &str = "Last.fm charts need Last.fm switched on and an API key saved under Settings > Last.fm. ListenBrainz charts work without one.";

/// Live charts over the listening data and the catalog.
pub struct LiveCharts {
    listening: ListeningData,
    pool: SqlitePool,
}

impl LiveCharts {
    /// Charts over `listening`, marked against the catalog in `pool`.
    pub fn new(listening: ListeningData, pool: SqlitePool) -> Self {
        Self { listening, pool }
    }

    async fn artist_page(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        mut artists: Vec<ChartArtist>,
    ) -> Result<TrendingArtistsPage, ProviderFailure> {
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
        mut albums: Vec<ChartAlbum>,
    ) -> Result<PopularAlbumsPage, ProviderFailure> {
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

fn stats_range(range: ChartRange) -> StatsRange {
    match range {
        ChartRange::ThisWeek => StatsRange::ThisWeek,
        ChartRange::ThisMonth => StatsRange::ThisMonth,
        ChartRange::ThisYear => StatsRange::ThisYear,
        ChartRange::AllTime => StatsRange::AllTime,
    }
}

fn listenbrainz_artists(rows: Vec<ArtistRow>) -> Vec<ChartArtist> {
    rows.into_iter()
        .filter_map(|row| {
            let mbid = row.mbid?;
            Some(ChartArtist {
                name: row.name,
                mbid: Some(mbid),
                local_id: None,
                image_url: None,
                listen_count: Some(row.listen_count),
                in_library: false,
                source: None,
            })
        })
        .collect()
}

fn listenbrainz_albums(rows: Vec<ListenedAlbum>) -> Vec<ChartAlbum> {
    rows.into_iter()
        .map(|row| ChartAlbum {
            name: row.title,
            mbid: row.release_group_mbid,
            local_id: None,
            artist_name: Some(row.artist_name),
            artist_mbid: row.artist_mbid,
            image_url: None,
            release_date: None,
            listen_count: Some(row.listen_count),
            in_library: false,
            requested: false,
            source: None,
        })
        .collect()
}

/// Last.fm chart artists keep rows without an MBID, as v2 did.
fn lastfm_artists(rows: Vec<ArtistRow>) -> Vec<ChartArtist> {
    rows.into_iter()
        .map(|row| ChartArtist {
            name: row.name,
            mbid: row.mbid,
            local_id: None,
            image_url: None,
            listen_count: Some(row.listen_count),
            in_library: false,
            source: Some(lastfm::SOURCE.to_owned()),
        })
        .collect()
}

fn lastfm_albums(rows: Vec<LastFmChartAlbum>) -> Vec<ChartAlbum> {
    rows.into_iter()
        .map(|row| ChartAlbum {
            name: row.name,
            mbid: row.mbid,
            local_id: None,
            artist_name: Some(row.artist_name).filter(|name| !name.is_empty()),
            artist_mbid: None,
            image_url: Some(row.image_url).filter(|url| !url.is_empty()),
            release_date: None,
            listen_count: Some(row.playcount),
            in_library: false,
            requested: false,
            source: Some(lastfm::SOURCE.to_owned()),
        })
        .collect()
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

/// Last.fm lists have no offset: read from the top through one row past
/// the page, at most [`LASTFM_MAX_ROWS`], and skip the earlier pages.
fn lastfm_window(limit: i64, offset: i64) -> (u32, usize) {
    let rows = (limit + offset + 1).clamp(0, LASTFM_MAX_ROWS);
    (
        u32::try_from(rows).unwrap_or(0),
        usize::try_from(offset).unwrap_or(0),
    )
}

/// Last.fm's period for a chart range (v2 `_lastfm_period_for_range`).
fn lastfm_period(range: ChartRange) -> &'static str {
    match range {
        ChartRange::ThisWeek => "7day",
        ChartRange::ThisMonth => "1month",
        ChartRange::ThisYear => "12month",
        ChartRange::AllTime => "overall",
    }
}

impl LiveCharts {
    /// The Last.fm sitewide artist chart for this page. Last.fm's chart
    /// has no ranges; every range reads the same global list, as in v2.
    async fn lastfm_trending(
        &self,
        user_id: &str,
        limit: i64,
        offset: i64,
        ask: &Ask,
    ) -> Result<Vec<ChartArtist>, ProviderFailure> {
        let (count, skip) = lastfm_window(limit, offset);
        let rows = self
            .listening
            .lastfm_chart_artists(user_id, count, ask)
            .await?;
        Ok(lastfm_artists(rows.into_iter().skip(skip).collect()))
    }

    /// The user's Last.fm top albums for this page.
    async fn lastfm_top(
        &self,
        user_id: &str,
        username: &str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        ask: &Ask,
    ) -> Result<Vec<ChartAlbum>, ProviderFailure> {
        let (count, skip) = lastfm_window(limit, offset);
        let rows = self
            .listening
            .lastfm_user_albums(user_id, username, lastfm_period(range), count, ask)
            .await?;
        Ok(lastfm_albums(rows.into_iter().skip(skip).collect()))
    }

    /// One trending-artists page for the user.
    pub fn trending_artists<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<TrendingArtistsPage, ProviderFailure>> {
        Box::pin(async move {
            let ask = Ask::person();
            let policy = self.listening.policy(user_id).await;
            let artists = match source {
                ChartSource::Lastfm => {
                    if !policy.ready(MusicSource::LastFm) {
                        return Err(ProviderFailure::not_configured(LASTFM_OFF));
                    }
                    self.lastfm_trending(user_id, limit, offset, &ask).await?
                }
                ChartSource::Listenbrainz => {
                    let (count, skip) = window(limit, offset);
                    match self
                        .listening
                        .sitewide_artists(stats_range(range), count, skip, &ask)
                        .await
                    {
                        Ok(rows) => listenbrainz_artists(rows),
                        Err(error) => {
                            self.stand_in(&policy, &error)?;
                            self.lastfm_trending(user_id, limit, offset, &ask).await?
                        }
                    }
                }
            };
            self.artist_page(range, limit, offset, artists).await
        })
    }

    /// One popular-albums page. Last.fm has no sitewide album chart, so a
    /// ListenBrainz outage has no stand-in here.
    pub fn popular_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async move {
            let albums = match source {
                ChartSource::Lastfm => {
                    // No sitewide Last.fm album chart and no admin account
                    // to stand in for one (see the module docs).
                    if !self
                        .listening
                        .policy(user_id)
                        .await
                        .ready(MusicSource::LastFm)
                    {
                        return Err(ProviderFailure::not_configured(LASTFM_OFF));
                    }
                    Vec::new()
                }
                ChartSource::Listenbrainz => {
                    let (count, skip) = window(limit, offset);
                    listenbrainz_albums(
                        self.listening
                            .sitewide_albums(stats_range(range), count, skip, &Ask::person())
                            .await?,
                    )
                }
            };
            self.album_page(range, limit, offset, albums).await
        })
    }

    /// One your-top-albums page for the user.
    pub fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async move {
            let ask = Ask::person();
            let policy = self.listening.policy(user_id).await;
            let user = policy.user();
            let lastfm_user = user
                .lastfm_username
                .clone()
                .filter(|_| policy.ready(MusicSource::LastFm));
            let asked = match source {
                ChartSource::Lastfm => MusicSource::LastFm,
                ChartSource::Listenbrainz => MusicSource::ListenBrainz,
            };
            // v2 `resolve_source_value`: the asked source when the user has
            // it linked, else the other one.
            let use_lastfm = lastfm_user.is_some()
                && (asked == MusicSource::LastFm || user.listenbrainz.is_none());
            let albums = match (use_lastfm, &lastfm_user, &user.listenbrainz) {
                (true, Some(username), _) => {
                    self.lastfm_top(user_id, username, range, limit, offset, &ask)
                        .await?
                }
                (_, _, Some(listenbrainz)) => {
                    let (count, skip) = window(limit, offset);
                    match self
                        .listening
                        .user_albums(listenbrainz, stats_range(range), count, skip, &ask)
                        .await
                    {
                        Ok(rows) => listenbrainz_albums(rows),
                        Err(error) => match &lastfm_user {
                            Some(username) if policy.stand_in(&error).is_some() => {
                                self.lastfm_top(user_id, username, range, limit, offset, &ask)
                                    .await?
                            }
                            _ => return Err(error.into()),
                        },
                    }
                }
                _ => Vec::new(),
            };
            self.album_page(range, limit, offset, albums).await
        })
    }

    /// `Ok` when Last.fm stands in after `error`, else the error itself.
    fn stand_in(
        &self,
        policy: &SourcePolicy,
        error: &ListeningError,
    ) -> Result<(), ProviderFailure> {
        match policy.stand_in(error) {
            Some(_) => {
                tracing::info!(%error, "listenbrainz chart unavailable; serving last.fm");
                Ok(())
            }
            None => Err(error.clone().into()),
        }
    }
}
