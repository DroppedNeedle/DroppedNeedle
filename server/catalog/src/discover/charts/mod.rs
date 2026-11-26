//! Home charts from ListenBrainz statistics and Last.fm.
//!
//! Chart range pages. Every row is marked when the
//! library holds it (best effort: a failed lookup leaves rows unmarked).
//! Every read goes through the listening data, so a chart shares its cache
//! with home and Discover: sitewide pages are kept twelve hours, a user's
//! own six.
//!
//! ListenBrainz (the default source): trending artists and popular albums
//! come from the sitewide stats, "your top albums" from the user's own
//! stats through their linked ListenBrainz account, or, while their
//! imported ListenBrainz history is current, from their plays in the app
//! when those name a few albums. One row more than asked
//! for tells whether another page follows, and a page shows each release
//! group once, with its higher count. Artists without an
//! MBID are dropped (they cannot link anywhere). When ListenBrainz is down
//! and the user can read Last.fm, trending artists and "your top albums"
//! come from Last.fm instead, marked as Last.fm rows.
//!
//! Last.fm (`source=lastfm`): trending artists come from Last.fm's chart,
//! read with the user's own key, else the instance key. With Last.fm
//! switched off or no key, these routes answer "not configured". Last.fm
//! has no sitewide album chart. This server does not keep an admin Last.fm
//! account to read one from, so popular albums from Last.fm are an empty
//! page. Last.fm names an album by its release: each is matched to its
//! release group as the queue matches them (remembered answers first, a
//! few MusicBrainz lookups after), and a row not matched yet carries no
//! id and keeps Last.fm's art.
//!
//! "your top albums" fall back to whichever service the user
//! has linked, and a user with neither gets an empty page. Genre pages are
//! served by the genre pages in front of this source.
//!
//! A person with Last.fm linked also gets their own Last.fm lists beside
//! their top albums: recent scrobbles and this week's top albums and
//! artists, read when they open their charts page. Anyone else gets none,
//! and nothing is read for them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;

use super::engine::history;
use super::ownership::{owned_albums, owned_artists};
use super::queue::QueueSettings;
use super::queue::build::{BuildContext, normalize, resolve_release_groups};
use super::queue::store::QueueDb;
use crate::discover::home::mark_owned;
use crate::discover::listening::{
    ArtistRow, Ask, LastFmChartAlbum, ListenSource, ListenedAlbum, ListeningData, ListeningError,
    SourcePolicy, StatsRange, one_per_group,
};
use crate::discover::page::shelves::{SHELF_SIZE, album, artist, shelf};
use crate::discover::{
    models::{
        AlbumRef, ArtistRef, ChartPage, ChartSection, SectionItem, SectionKind, ShelfSource,
        YourLastfmResponse,
    },
    ports::{BoxFuture, ProviderFailure},
};

/// The fewest albums played in the app over a range for "your top albums"
/// to be made from them.
const PLAYED_TOP_MIN: i64 = 3;

/// Most rows one Last.fm range page reads: 200.
const LASTFM_MAX_ROWS: i64 = 200;

/// Recent scrobbles read for the person's own lists.
const RECENT_ROWS: u32 = 20;
/// Rows of each list looked at, before the shelf is cut.
const WEEKLY_ROWS: usize = 20;

/// How long a person waits on a Last.fm page's new release group lookups.
/// Each answer is remembered as it comes, so lookups cut off keep what
/// they found, and the rest are tried again next time.
const RESOLVE_BUDGET: Duration = Duration::from_secs(5);

/// The sentence a Last.fm chart answers while Last.fm cannot be read.
const LASTFM_OFF: &str = "Last.fm charts need Last.fm switched on and an API key saved under Settings > Last.fm. ListenBrainz charts work without one.";

/// Live charts over the listening data and the catalog.
pub struct LiveCharts {
    listening: ListeningData,
    db: Db,
    queue_db: QueueDb,
    advanced: Live<AdvancedSettings>,
    clock: Arc<dyn Clock>,
}

impl LiveCharts {
    /// Charts over `listening`, marked against the catalog in `db`, with
    /// Last.fm albums matched to release groups within the lookup budget
    /// in `advanced`, at the time `clock` tells.
    pub fn new(
        listening: ListeningData,
        db: Db,
        advanced: Live<AdvancedSettings>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            listening,
            queue_db: QueueDb::new(db.clone()),
            db,
            advanced,
            clock,
        }
    }

    async fn artist_page(
        &self,
        range: StatsRange,
        limit: i64,
        offset: i64,
        mut artists: Vec<ArtistRef>,
    ) -> Result<ChartPage<ArtistRef>, ProviderFailure> {
        let mbids: Vec<&str> = artists
            .iter()
            .filter_map(|a| a.musicbrainz_id.as_deref())
            .collect();
        let owned = best_effort(owned_artists(&self.db, &mbids).await);
        for artist in &mut artists {
            if let Some(local) = artist
                .musicbrainz_id
                .as_deref()
                .and_then(|mbid| owned.get(&mbid.to_ascii_lowercase()))
            {
                artist.in_library = true;
                artist.local_id = Some(local.clone());
            }
        }
        let has_more = artists.len() as i64 > limit;
        artists.truncate(usize::try_from(limit).unwrap_or(0));
        Ok(ChartPage {
            range_key: range,
            label: range.label().to_owned(),
            items: artists,
            offset,
            limit,
            has_more,
        })
    }

    async fn album_page(
        &self,
        range: StatsRange,
        limit: i64,
        offset: i64,
        mut albums: Vec<AlbumRef>,
    ) -> Result<ChartPage<AlbumRef>, ProviderFailure> {
        let mbids: Vec<&str> = albums
            .iter()
            .filter_map(|a| a.musicbrainz_id.as_deref())
            .collect();
        let owned = best_effort(owned_albums(&self.db, &mbids).await);
        for album in &mut albums {
            if let Some(local) = album
                .musicbrainz_id
                .as_deref()
                .and_then(|mbid| owned.get(&mbid.to_ascii_lowercase()))
            {
                album.in_library = true;
                album.local_id = Some(local.clone());
            }
        }
        let has_more = albums.len() as i64 > limit;
        albums.truncate(usize::try_from(limit).unwrap_or(0));
        // After the page is cut, so a repeated group never ends the list.
        let albums = one_per_group(
            albums,
            |album| album.musicbrainz_id.as_deref(),
            |album| album.listen_count.unwrap_or(0),
        );
        Ok(ChartPage {
            range_key: range,
            label: range.label().to_owned(),
            items: albums,
            offset,
            limit,
            has_more,
        })
    }
}

fn listenbrainz_artists(rows: Vec<ArtistRow>) -> Vec<ArtistRef> {
    rows.into_iter()
        .filter_map(|row| {
            let mbid = row.mbid?;
            Some(ArtistRef {
                name: row.name,
                musicbrainz_id: Some(mbid),
                local_id: None,
                image_url: None,
                listen_count: row.listen_count,
                in_library: false,
            })
        })
        .collect()
}

fn listenbrainz_albums(rows: Vec<ListenedAlbum>) -> Vec<AlbumRef> {
    rows.into_iter()
        .map(|row| AlbumRef {
            title: row.title,
            musicbrainz_id: row.release_group_mbid,
            local_id: None,
            artist_name: Some(row.artist_name),
            artist_id: row.artist_mbid,
            cover_url: None,
            release_date: None,
            year: None,
            release_type: None,
            listen_count: Some(row.listen_count),
            in_library: false,
            requested: false,
        })
        .collect()
}

/// Last.fm chart artists keep rows without an MBID. Quirk (v2): unlike the
/// ListenBrainz chart, these rows are not dropped.
fn lastfm_artists(rows: Vec<ArtistRow>) -> Vec<ArtistRef> {
    rows.into_iter()
        .map(|row| ArtistRef {
            name: row.name,
            musicbrainz_id: row.mbid,
            local_id: None,
            image_url: None,
            listen_count: row.listen_count,
            in_library: false,
        })
        .collect()
}

/// Last.fm albums as rows, each with the release group its release id
/// matched in `groups`, else no id.
fn lastfm_albums(rows: Vec<LastFmChartAlbum>, groups: &HashMap<String, String>) -> Vec<AlbumRef> {
    rows.into_iter()
        .map(|row| AlbumRef {
            musicbrainz_id: normalize(row.mbid.as_deref())
                .and_then(|release| groups.get(&release).cloned()),
            title: row.name,
            local_id: None,
            artist_name: Some(row.artist_name).filter(|name| !name.is_empty()),
            artist_id: None,
            cover_url: Some(row.image_url).filter(|url| !url.is_empty()),
            release_date: None,
            year: None,
            release_type: None,
            listen_count: row.playcount,
            in_library: false,
            requested: false,
        })
        .collect()
}

/// Log a Last.fm list whose read failed, which is left out.
fn note_failed<T>(list: &str, read: &Result<T, ListeningError>) {
    if let Err(error) = read {
        tracing::warn!(%error, list, "a last.fm list failed; leaving it out");
    }
}

/// A Last.fm artist list as a shelf: rows with an MBID, once each.
fn artist_shelf(rows: &[ArtistRow], title: &str) -> Option<ChartSection> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for row in rows.iter().take(WEEKLY_ROWS) {
        let Some(mbid) = normalize(row.mbid.as_deref()) else {
            continue;
        };
        if seen.insert(mbid) {
            items.push(artist(row.mbid.as_deref(), &row.name, row.listen_count));
        }
    }
    items.truncate(SHELF_SIZE);
    shelf(
        title,
        SectionKind::Artists,
        items,
        Some(ShelfSource::Lastfm),
    )
}

/// Ownership marks are a nicety: a failed lookup is logged and the page
/// renders unmarked.
fn best_effort(
    owned: Result<std::collections::HashMap<String, String>, ProviderFailure>,
) -> std::collections::HashMap<String, String> {
    owned.unwrap_or_else(|failure| {
        tracing::warn!(%failure, "chart ownership lookup failed; rows stay unmarked");
        std::collections::HashMap::new()
    })
}

/// One row past the page tells whether another page follows.
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

/// Last.fm's period for a chart range.
fn lastfm_period(range: StatsRange) -> &'static str {
    match range {
        StatsRange::ThisWeek => "7day",
        StatsRange::ThisMonth => "1month",
        StatsRange::ThisYear => "12month",
        StatsRange::AllTime => "overall",
    }
}

impl LiveCharts {
    /// The Last.fm sitewide artist chart for this page. Last.fm's chart
    /// has no ranges; every range reads the same global list. Quirk (v2): the
    /// requested range is not applied to this chart.
    async fn lastfm_trending(
        &self,
        user_id: &str,
        limit: i64,
        offset: i64,
        ask: &Ask,
    ) -> Result<Vec<ArtistRef>, ProviderFailure> {
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
        range: StatsRange,
        limit: i64,
        offset: i64,
        ask: &Ask,
    ) -> Result<Vec<AlbumRef>, ProviderFailure> {
        let (count, skip) = lastfm_window(limit, offset);
        let rows: Vec<LastFmChartAlbum> = self
            .listening
            .lastfm_user_albums(user_id, username, lastfm_period(range), count, ask)
            .await?
            .into_iter()
            .skip(skip)
            .collect();
        let groups = self.release_groups(&rows, ask).await;
        Ok(lastfm_albums(rows, &groups))
    }

    /// The release group of each Last.fm album: the remembered answers
    /// always, then new lookups for the rest within [`RESOLVE_BUDGET`] and
    /// the lookup budget in the advanced settings. While those cannot be
    /// read, no new lookup is made.
    async fn release_groups(
        &self,
        rows: &[LastFmChartAlbum],
        ask: &Ask,
    ) -> HashMap<String, String> {
        let mut releases: Vec<String> = rows
            .iter()
            .filter_map(|row| normalize(row.mbid.as_deref()))
            .collect();
        releases.sort();
        releases.dedup();
        let remembered = self.remembered(&releases).await;
        let mut groups: HashMap<String, String> = remembered
            .iter()
            .filter_map(|(release, group)| Some((release.clone(), group.clone()?)))
            .collect();
        let pending: Vec<String> = releases
            .into_iter()
            .filter(|release| !remembered.contains_key(release))
            .collect();
        let Some(settings) = QueueSettings::read(&self.advanced) else {
            return groups;
        };
        if pending.is_empty() {
            return groups;
        }
        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.queue_db,
            settings: &settings,
            ask: ask.clone(),
            now_ms: self.clock.now_ms(),
        };
        match tokio::time::timeout(RESOLVE_BUDGET, resolve_release_groups(&ctx, &pending)).await {
            Ok(found) => groups.extend(found),
            Err(_) => {
                tracing::info!(
                    "last.fm album matching ran out of time; unmatched rows carry no id"
                );
                let found = self.remembered(&pending).await;
                groups.extend(
                    found
                        .into_iter()
                        .filter_map(|(release, group)| Some((release, group?))),
                );
            }
        }
        groups
    }

    /// The remembered release group answers for `releases` (misses as
    /// `None`); empty when they cannot be read.
    async fn remembered(&self, releases: &[String]) -> HashMap<String, Option<String>> {
        if releases.is_empty() {
            return HashMap::new();
        }
        self.queue_db
            .resolutions(releases)
            .await
            .unwrap_or_else(|cause| {
                tracing::warn!(%cause, "release group answers unreadable; chart rows carry no id");
                HashMap::new()
            })
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
        Box::pin(async move {
            let ask = Ask::person();
            let policy = self.listening.policy(user_id).await;
            let artists = match source {
                ListenSource::Lastfm => {
                    if !policy.ready(ListenSource::Lastfm) {
                        return Err(ProviderFailure::not_configured(LASTFM_OFF));
                    }
                    self.lastfm_trending(user_id, limit, offset, &ask).await?
                }
                ListenSource::Listenbrainz => {
                    let (count, skip) = window(limit, offset);
                    match self
                        .listening
                        .sitewide_artists(range, count, skip, &ask)
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
        range: StatsRange,
        limit: i64,
        offset: i64,
        source: ListenSource,
    ) -> BoxFuture<'a, Result<ChartPage<AlbumRef>, ProviderFailure>> {
        Box::pin(async move {
            let albums = match source {
                ListenSource::Lastfm => {
                    // No sitewide Last.fm album chart and no admin account
                    // to stand in for one (see the module docs).
                    if !self
                        .listening
                        .policy(user_id)
                        .await
                        .ready(ListenSource::Lastfm)
                    {
                        return Err(ProviderFailure::not_configured(LASTFM_OFF));
                    }
                    Vec::new()
                }
                ListenSource::Listenbrainz => {
                    let (count, skip) = window(limit, offset);
                    listenbrainz_albums(
                        self.listening
                            .sitewide_albums(range, count, skip, &Ask::person())
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
        range: StatsRange,
        limit: i64,
        offset: i64,
        source: ListenSource,
    ) -> BoxFuture<'a, Result<ChartPage<AlbumRef>, ProviderFailure>> {
        Box::pin(async move {
            let ask = Ask::person();
            let policy = self.listening.policy(user_id).await;
            let user = policy.user();
            let lastfm_user = user
                .lastfm_username
                .clone()
                .filter(|_| policy.ready(ListenSource::Lastfm));
            // The asked source when the user has it linked, else the other one.
            let use_lastfm = lastfm_user.is_some()
                && (source == ListenSource::Lastfm || user.listenbrainz.is_none());
            let albums = match (use_lastfm, &lastfm_user, &user.listenbrainz) {
                (true, Some(username), _) => {
                    self.lastfm_top(user_id, username, range, limit, offset, &ask)
                        .await?
                }
                (_, _, Some(listenbrainz)) => {
                    let (count, skip) = window(limit, offset);
                    if let Some(played) = self.played_top(user_id, range, count, skip).await {
                        return self.album_page(range, limit, offset, played).await;
                    }
                    match self
                        .listening
                        .user_albums(listenbrainz, range, count, skip, &ask)
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

    /// The person's own Last.fm lists: recent scrobbles, this week's top
    /// albums and this week's top artists, each left out when empty or when
    /// its read failed; the error only when all three failed. Empty, with
    /// nothing read, while they have no Last.fm link or Last.fm cannot be
    /// read.
    pub fn your_lastfm<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<YourLastfmResponse, ProviderFailure>> {
        Box::pin(async move {
            let policy = self.listening.policy(user_id).await;
            let user = policy.user();
            let Some(username) = user
                .lastfm_username
                .clone()
                .filter(|_| user.lastfm && policy.ready(ListenSource::Lastfm))
            else {
                return Ok(YourLastfmResponse {
                    shelves: Vec::new(),
                });
            };
            let ask = Ask::person();
            let (recent, albums, artists) = tokio::join!(
                self.listening
                    .lastfm_recent_albums(user_id, &username, RECENT_ROWS, &ask),
                self.listening
                    .lastfm_weekly_albums(user_id, &username, &ask),
                self.listening
                    .lastfm_weekly_artists(user_id, &username, &ask),
            );
            // A list whose read failed is left out on its own; the page
            // fails only when none of the three answered.
            if let (Err(error), Err(_), Err(_)) = (&recent, &albums, &artists) {
                return Err(error.clone().into());
            }
            note_failed("recent", &recent);
            note_failed("weekly albums", &albums);
            note_failed("weekly artists", &artists);
            let (recent, albums, artists) = (
                recent.unwrap_or_default(),
                albums.unwrap_or_default(),
                artists.unwrap_or_default(),
            );
            let mut shelves: Vec<ChartSection> = [
                self.album_shelf(&recent, "Recently Scrobbled", true, &ask)
                    .await,
                self.album_shelf(&albums, "Your Top Albums This Week", false, &ask)
                    .await,
                artist_shelf(&artists, "Your Weekly Top Artists"),
            ]
            .into_iter()
            .flatten()
            .collect();
            let mut album_rows = Vec::new();
            let mut artist_rows = Vec::new();
            for shelf in &mut shelves {
                for item in &mut shelf.items {
                    match item {
                        SectionItem::Album(row) => album_rows.push(row),
                        SectionItem::Artist(row) => artist_rows.push(row),
                        SectionItem::Track(_) | SectionItem::Genre(_) => {}
                    }
                }
            }
            mark_owned(&self.db, album_rows, artist_rows).await;
            Ok(YourLastfmResponse { shelves })
        })
    }

    /// A Last.fm album list as a shelf. Last.fm names releases, so each id
    /// is matched to its release group first; a row not matched in time
    /// keeps its Last.fm art but carries no id: a release id is not a
    /// release group id, and passing it on would break links. `once` keeps
    /// one row an album.
    async fn album_shelf(
        &self,
        rows: &[LastFmChartAlbum],
        title: &str,
        once: bool,
        ask: &Ask,
    ) -> Option<ChartSection> {
        let rows = &rows[..rows.len().min(WEEKLY_ROWS)];
        if rows.is_empty() {
            return None;
        }
        let groups = self.release_groups(rows, ask).await;
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        for row in rows {
            let group =
                normalize(row.mbid.as_deref()).and_then(|release| groups.get(&release).cloned());
            let key = group
                .clone()
                .unwrap_or_else(|| format!("{}|{}", row.artist_name, row.name).to_lowercase());
            if once && !seen.insert(key) {
                continue;
            }
            let mut item = album(
                group.as_deref(),
                &row.name,
                Some(&row.artist_name),
                None,
                row.playcount.filter(|count| *count > 0),
            );
            if item.cover_url.is_none() && !row.image_url.is_empty() {
                item.cover_url = Some(row.image_url.clone());
            }
            items.push(SectionItem::Album(item));
        }
        items.truncate(SHELF_SIZE);
        shelf(title, SectionKind::Albums, items, Some(ShelfSource::Lastfm))
    }

    /// The user's most played albums in the app over `range`, `count` from
    /// `skip`, while their imported ListenBrainz history is current and
    /// they played at least [`PLAYED_TOP_MIN`] albums in it; `None` sends
    /// the page to ListenBrainz's statistics. Imported listens name no
    /// album, so only plays in the app count.
    async fn played_top(
        &self,
        user_id: &str,
        range: StatsRange,
        count: u32,
        skip: u32,
    ) -> Option<Vec<AlbumRef>> {
        let read = async {
            if !history::import_current(&self.db, user_id).await? {
                return Ok(None);
            }
            let since = history::range_start(range, self.clock.now_ms());
            let (rows, albums) = history::top_albums(&self.db, user_id, since, count, skip).await?;
            Ok::<_, droppedneedle_kernel::db::DbError>(
                (albums >= PLAYED_TOP_MIN).then(|| listenbrainz_albums(rows)),
            )
        };
        read.await.unwrap_or_else(|error| {
            tracing::warn!(%error, "plays in the app unreadable; asking listenbrainz");
            None
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
