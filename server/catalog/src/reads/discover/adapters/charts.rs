//! Home charts from ListenBrainz statistics and Last.fm.
//!
//! Ports v2's `HomeChartsService` range pages. Every row is marked when the
//! library holds it (best effort: a failed lookup leaves rows unmarked).
//!
//! ListenBrainz (the default source): trending artists and popular albums
//! come from the sitewide stats, "your top albums" from the user's own
//! stats through their linked ListenBrainz account. One row more than asked
//! for tells whether another page follows. As in v2, artists without an
//! MBID are dropped (they cannot link anywhere).
//!
//! Last.fm (`source=lastfm`): trending artists come from Last.fm's chart
//! with the instance API key, read on every call, so a key saved in
//! Settings applies at once. Without a key, or with Last.fm switched off,
//! these routes answer "not configured". Last.fm has no sitewide album
//! chart; v2 showed the admin's own Last.fm account there, which this
//! server does not keep, so popular albums from Last.fm are an empty page.
//! "Your top albums" read the user's linked Last.fm account with their own
//! key, else the instance key.
//!
//! As in v2, "your top albums" fall back to whichever service the user
//! has linked, and a user with neither gets an empty page. Genre pages need
//! the genre index and tag search, not ported yet, and answer "not built".

use std::sync::Arc;

use sqlx::SqlitePool;

use super::ownership::{owned_albums, owned_artists};
use crate::reads::catalog::upstream::{instance_lastfm_key, user_lastfm};
use crate::reads::discover::{
    models::{
        ChartAlbum, ChartArtist, ChartRange, ChartSource, GenreDetailResponse, PopularAlbumsPage,
        TrendingArtistsPage,
    },
    ports::{BoxFuture, ChartsSource, ProviderFailure},
};
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_plugins::scrobble::ListenBrainzLinkStore;
use droppedneedle_providers::lastfm::{self, LastFmClient, LastFmCredentials, TopItem};
use droppedneedle_providers::listenbrainz::{
    ListenBrainzClient,
    stats::{ArtistStat, ReleaseGroupStat},
};
use droppedneedle_providers::{ProviderError, RequestPriority};

/// Most rows one Last.fm range page reads (v2 `min(limit + offset + 1, 200)`).
const LASTFM_MAX_ROWS: i64 = 200;

/// The chart pages are read while a person waits.
const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

/// Live charts: ListenBrainz always, Last.fm when its client could be paced.
pub struct LiveCharts {
    client: ListenBrainzClient,
    links: Arc<dyn ListenBrainzLinkStore>,
    lastfm: Option<LastFmCharts>,
    pool: SqlitePool,
}

/// The Last.fm half of the charts.
pub struct LastFmCharts {
    client: LastFmClient,
    users: UsersDeps,
}

impl LastFmCharts {
    /// Last.fm charts over the one Last.fm client. `users` holds the
    /// Last.fm switch with the instance key, read on every call, and the
    /// users' links.
    pub fn new(client: LastFmClient, users: UsersDeps) -> Self {
        Self { client, users }
    }

    /// The instance key, while Last.fm is switched on and a key is saved.
    fn instance_key(&self) -> Option<String> {
        if !self.users.lastfm_switch.enabled() {
            return None;
        }
        instance_lastfm_key(&self.users)
    }

    /// The user's Last.fm username and the key to read it with (their own,
    /// else the instance key), when both exist.
    async fn user(&self, user_id: &str) -> Option<(String, LastFmCredentials)> {
        if !self.users.lastfm_switch.enabled() {
            return None;
        }
        let link = user_lastfm(&self.users, user_id).await;
        let username = link.username?;
        let api_key = match link.api_key {
            Some(key) => key,
            None => self.instance_key()?,
        };
        Some((username, credentials(api_key)))
    }
}

fn credentials(api_key: String) -> LastFmCredentials {
    LastFmCredentials {
        api_key,
        ..LastFmCredentials::default()
    }
}

impl LiveCharts {
    /// Charts over the ListenBrainz client, the users' links, the Last.fm
    /// half (when it could be built) and the catalog.
    pub fn new(
        client: ListenBrainzClient,
        links: Arc<dyn ListenBrainzLinkStore>,
        lastfm: Option<LastFmCharts>,
        pool: SqlitePool,
    ) -> Self {
        Self {
            client,
            links,
            lastfm,
            pool,
        }
    }

    /// The Last.fm half with the instance key, or "not configured".
    fn sitewide_lastfm(&self) -> Result<(&LastFmCharts, LastFmCredentials), ProviderFailure> {
        self.lastfm
            .as_ref()
            .and_then(|lastfm| Some((lastfm, credentials(lastfm.instance_key()?))))
            .ok_or_else(|| {
                ProviderFailure::not_configured(
                    "Last.fm charts need Last.fm switched on and an API key saved under Settings > Last.fm. ListenBrainz charts work without one.",
                )
            })
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

fn listenbrainz_artists(rows: Vec<ArtistStat>) -> Vec<ChartArtist> {
    rows.into_iter()
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
        .collect()
}

fn listenbrainz_albums(rows: Vec<ReleaseGroupStat>) -> Vec<ChartAlbum> {
    rows.into_iter()
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
        .collect()
}

/// Last.fm chart artists keep rows without an MBID, as v2 did.
fn lastfm_artists(rows: Vec<TopItem>) -> Vec<ChartArtist> {
    rows.into_iter()
        .map(|row| ChartArtist {
            name: row.name,
            mbid: row.mbid,
            local_id: None,
            image_url: None,
            listen_count: Some(row.playcount),
            in_library: false,
            source: Some(lastfm::SOURCE.to_owned()),
        })
        .collect()
}

fn lastfm_albums(rows: Vec<TopItem>) -> Vec<ChartAlbum> {
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

/// The rows, or the provider failure.
fn rows<T>(result: Result<Vec<T>, ProviderError>) -> Result<Vec<T>, ProviderFailure> {
    result.map_err(|error| ProviderFailure::failed(error.to_string()))
}

/// The Last.fm counterpart of [`rows`], skipping the earlier pages.
fn lastfm_rows(
    result: Result<Vec<TopItem>, ProviderError>,
    skip: usize,
) -> Result<Vec<TopItem>, ProviderFailure> {
    Ok(rows(result)?.into_iter().skip(skip).collect())
}

impl ChartsSource for LiveCharts {
    fn trending_artists(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>> {
        Box::pin(async move {
            let artists = match source {
                ChartSource::Lastfm => {
                    // Last.fm's chart has no ranges; every range reads the
                    // same global list, as in v2.
                    let (lastfm, creds) = self.sitewide_lastfm()?;
                    let (count, skip) = lastfm_window(limit, offset);
                    lastfm_artists(lastfm_rows(
                        lastfm
                            .client
                            .chart_top_artists(&creds, count, PRIORITY)
                            .await,
                        skip,
                    )?)
                }
                ChartSource::Listenbrainz => {
                    let (count, skip) = window(limit, offset);
                    listenbrainz_artists(rows(
                        self.client
                            .sitewide_top_artists(range.as_str(), count, skip, PRIORITY)
                            .await,
                    )?)
                }
            };
            self.artist_page(range, limit, offset, artists).await
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
            let albums = match source {
                ChartSource::Lastfm => {
                    // No sitewide Last.fm album chart and no admin account
                    // to stand in for one (see the module docs).
                    self.sitewide_lastfm()?;
                    Vec::new()
                }
                ChartSource::Listenbrainz => {
                    let (count, skip) = window(limit, offset);
                    listenbrainz_albums(rows(
                        self.client
                            .sitewide_top_release_groups(range.as_str(), count, skip, PRIORITY)
                            .await,
                    )?)
                }
            };
            self.album_page(range, limit, offset, albums).await
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
            let listenbrainz = self.links.status(user_id).await;
            let lastfm = match &self.lastfm {
                Some(lastfm) => lastfm.user(user_id).await.map(|user| (lastfm, user)),
                None => None,
            };
            // v2 `resolve_source_value`: the asked source when the user has
            // it linked, else the other one.
            let use_lastfm =
                lastfm.is_some() && (source == ChartSource::Lastfm || listenbrainz.is_none());
            let albums = match (use_lastfm, lastfm, listenbrainz) {
                (true, Some((lastfm, (username, creds))), _) => {
                    let (count, skip) = lastfm_window(limit, offset);
                    lastfm_albums(lastfm_rows(
                        lastfm
                            .client
                            .user_top_albums(
                                &creds,
                                &username,
                                lastfm_period(range),
                                count,
                                PRIORITY,
                            )
                            .await,
                        skip,
                    )?)
                }
                (_, _, Some(link)) => {
                    let (count, skip) = window(limit, offset);
                    listenbrainz_albums(rows(
                        self.client
                            .user_top_release_groups(
                                &link.username,
                                range.as_str(),
                                count,
                                skip,
                                PRIORITY,
                            )
                            .await,
                    )?)
                }
                _ => Vec::new(),
            };
            self.album_page(range, limit, offset, albums).await
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
