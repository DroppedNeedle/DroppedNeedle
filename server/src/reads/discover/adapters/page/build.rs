//! One discover page build for one user (v2 `build_discover_data`).
//!
//! The build runs in two rounds. The first fetches what several shelves
//! share (seed artists, their similar artists, charts, the user's genres,
//! Jellyfin plays, the library's identified albums), every read bounded
//! to 25 seconds so one slow service only costs its own shelves. The
//! second builds the shelves that need more reads of their own (Top
//! Picks, Daily Mixes, radio, the weekly playlist, ...) in parallel under
//! the same bound. The rest is shaping: artist shelves share a "seen"
//! set so an artist appears once on the page, ownership is marked from
//! the library, and album shelves are deduplicated.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::future::join_all;

use super::library::{LibraryReads, fold};
use super::memo::BuildMemo;
use super::picks::{self, Candidate};
use super::shelves::{self, SHELF_SIZE, album, artist, genre, shelf};
use super::sources::{LastFmChartAlbum, PageSources, ScoredArtist};
use crate::reads::discover::adapters::ownership;
use crate::reads::discover::adapters::queue::QueueSettings;
use crate::reads::discover::adapters::queue::build::{
    BuildContext, VARIOUS_ARTISTS_MBID, lastfm_pools, listened_albums, normalize, or_empty,
    resolve_release_groups, seed_artists, similar_artist_pools,
};
use crate::reads::discover::adapters::queue::select::{MAX_PER_ARTIST, Shuffler, round_robin};
use crate::reads::discover::adapters::queue::sources::{
    AlbumRow, ArtistRow, MusicSource, StatsRange, UserMusic,
};
use crate::reads::discover::adapters::queue::store::QueueDb;
use crate::reads::discover::models::{
    BecauseYouListenTo, ChartAlbum, ChartSection, DiscoverResponse, SectionItem, TopPickItem,
    TopPicksSection, WeeklyExploration, WeeklyTrack,
};

/// Bound on any one upstream read in a build.
const TASK_TIMEOUT: Duration = Duration::from_secs(25);
/// Bound on the Last.fm route into Top Picks during a ListenBrainz outage.
const PICKS_SIMILARITY_BUDGET: Duration = Duration::from_secs(12);
/// Bound on resolving Last.fm release ids and weekly playlist covers.
const RESOLVE_BUDGET: Duration = Duration::from_secs(15);
/// Bound on one radio shelf.
const RADIO_BUDGET: Duration = Duration::from_secs(18);
/// Seed artists the page builds from.
const SEEDS: usize = 3;
/// Rediscover: artists played at least this often...
const REDISCOVER_MIN_PLAYS: i64 = 5;
/// ...and not in the last three months.
const REDISCOVER_IDLE_DAYS: f64 = 90.0;
/// Missing Essentials: artists with at least this many library albums.
const ESSENTIALS_MIN_ALBUMS: usize = 3;
/// Missing Essentials: at most this many albums per artist.
const ESSENTIALS_PER_ARTIST: usize = 3;
/// Genres to Explore: genres with fewer library artists than this.
const UNEXPLORED_THRESHOLD: i64 = 2;
/// Genres to Explore: at most this many.
const UNEXPLORED_MAX: usize = 8;
/// Milestone birthdays celebrated by Anniversaries.
const ANNIVERSARY_YEARS: [i64; 7] = [10, 20, 25, 30, 40, 50, 60];
/// Albums per Daily Mix.
const MIX_SIZE: usize = 12;

/// The settings one build reads, fresh each time.
#[derive(Debug, Clone)]
pub struct PageSettings {
    /// Top Picks shown.
    pub picks_count: usize,
    /// Weight of the genre signal in Top Picks.
    pub genre_weight: f64,
    /// The queue settings (lookup budgets, ignore ledger).
    pub queue: QueueSettings,
    /// Whether some download client is set up (connect-a-service cards).
    pub download_client: bool,
}

/// What one build reads from.
pub struct Builder<'a> {
    /// Provider reads.
    pub sources: &'a dyn PageSources,
    /// The queue tables (ignores, remembered release groups).
    pub queue_db: &'a QueueDb,
    /// Library reads.
    pub library: &'a LibraryReads,
    /// Daily Mix and Top Picks memos.
    pub memo: &'a BuildMemo,
    /// Settings for this build.
    pub settings: PageSettings,
    /// Today's date (`YYYY-MM-DD`, UTC).
    pub today: String,
    /// The current year (UTC).
    pub year: i64,
    /// Now, unix seconds.
    pub now: f64,
}

/// A build's page plus the services whose every read failed.
pub struct Built {
    /// The page.
    pub page: DiscoverResponse,
    /// `listenbrainz`/`lastfm` mapped to `unavailable` when all of that
    /// service's reads failed.
    pub degraded: HashMap<String, String>,
}

/// Failed reads of the first round, for the degraded banner.
#[derive(Default)]
struct Tracker {
    attempted: Mutex<Vec<&'static str>>,
    failed: Mutex<HashSet<&'static str>>,
}

impl Tracker {
    /// Run one read under the task bound. `None` when it failed.
    async fn run<T>(
        &self,
        key: &'static str,
        read: impl Future<Output = Result<T, String>>,
    ) -> Option<T> {
        if let Ok(mut attempted) = self.attempted.lock() {
            attempted.push(key);
        }
        let outcome = match tokio::time::timeout(TASK_TIMEOUT, read).await {
            Ok(Ok(value)) => return Some(value),
            Ok(Err(cause)) => cause,
            Err(_) => "timed out".to_owned(),
        };
        tracing::warn!(task = key, cause = %outcome, "discover read failed; its shelves stay empty");
        if let Ok(mut failed) = self.failed.lock() {
            failed.insert(key);
        }
        None
    }

    /// Services whose every attempted read failed.
    fn degraded(&self) -> HashMap<String, String> {
        let attempted = self.attempted.lock().map(|a| a.clone()).unwrap_or_default();
        let failed = self.failed.lock().map(|f| f.clone()).unwrap_or_default();
        let mut status = HashMap::new();
        for (prefix, label) in [("lb_", "listenbrainz"), ("lfm_", "lastfm")] {
            let family: Vec<&&str> = attempted.iter().filter(|k| k.starts_with(prefix)).collect();
            if !family.is_empty() && family.iter().all(|key| failed.contains(**key)) {
                status.insert(label.to_owned(), "unavailable".to_owned());
            }
        }
        status
    }
}

/// Run a shelf builder under the task bound; a timeout empties it.
async fn bounded<T: Default>(what: &str, build: impl Future<Output = T>) -> T {
    match tokio::time::timeout(TASK_TIMEOUT, build).await {
        Ok(value) => value,
        Err(_) => {
            tracing::warn!(shelf = what, "discover shelf timed out; leaving it empty");
            T::default()
        }
    }
}

/// What the first round fetched.
#[derive(Default)]
struct Shared {
    similar: Vec<Vec<ScoredArtist>>,
    trending_artists: Vec<ArtistRow>,
    lastfm_weekly_artists: Vec<ArtistRow>,
    lastfm_weekly_albums: Vec<LastFmChartAlbum>,
    lastfm_recent: Vec<LastFmChartAlbum>,
    fresh: Vec<AlbumRow>,
    genres: Option<Vec<(String, i64)>>,
    lastfm_genre_artists: Vec<ArtistRow>,
    jellyfin: Vec<super::sources::PlayedArtist>,
    library_albums: Vec<super::library::LibraryAlbum>,
}

fn source_label(source: MusicSource) -> &'static str {
    match source {
        MusicSource::ListenBrainz => "listenbrainz",
        MusicSource::LastFm => "lastfm",
    }
}

/// Title-case a folded genre ("hip hop" to "Hip Hop").
fn title_case(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A shuffler fixed for one day and one purpose, so a rebuild the same
/// day reproduces the same shelves instead of reshuffling them.
fn daily_shuffler(today: &str, parts: &[&str]) -> Shuffler {
    let digest = md5::compute(format!("{today}:{}", parts.join(":")));
    let mut seed = [0u8; 8];
    seed.copy_from_slice(&digest.0[..8]);
    Shuffler::seeded(u64::from_le_bytes(seed))
}

/// Days since an ISO 8601 instant (date part only), `None` when it does
/// not parse.
fn days_since(iso: &str, now: f64) -> Option<f64> {
    let date = iso.trim().get(..10)?;
    let mut parts = date.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    let month = time::Month::try_from(month).ok()?;
    let played = time::Date::from_calendar_date(year, month, day).ok()?;
    let today = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .ok()?
        .date();
    Some(f64::from(today.to_julian_day() - played.to_julian_day()))
}

impl Builder<'_> {
    fn queue_ctx<'b>(&'b self, settings: &'b QueueSettings) -> BuildContext<'b> {
        BuildContext {
            sources: self.sources,
            db: self.queue_db,
            settings,
        }
    }

    /// Whether Last.fm stands in for ListenBrainz popularity: only while
    /// ListenBrainz refuses popularity reads, and only when the user has
    /// Last.fm.
    fn lastfm_for_popularity(&self, user: &UserMusic) -> bool {
        user.lastfm && self.sources.listenbrainz_popularity_down()
    }

    /// Build the page.
    pub async fn build(&self, user_id: &str) -> Built {
        let user = self.sources.user_music(user_id).await;
        let primary = user.resolved_source();
        let lb_user = user.listenbrainz.clone();
        let lfm_user = user.lastfm_username.clone().filter(|_| user.lastfm);
        let mut seed_settings = self.settings.queue.clone();
        seed_settings.seed_artists = SEEDS;
        let seeds = seed_artists(&self.queue_ctx(&seed_settings), user_id, &user, primary).await;

        let tracker = Tracker::default();
        let shared = self
            .first_round(
                &tracker,
                user_id,
                &user,
                primary,
                &seeds,
                lb_user.as_deref(),
                lfm_user.as_deref(),
            )
            .await;
        let library_groups: HashSet<String> = shared
            .library_albums
            .iter()
            .map(|album| album.release_group_mbid.clone())
            .collect();

        let mut page = super::empty_page();
        let mut seen_artists: HashSet<String> = HashSet::new();
        page.because_you_listen_to =
            because_sections(&seeds, &shared.similar, &mut seen_artists, primary);
        page.fresh_releases = fresh_releases(&shared.fresh);

        let (
            missing,
            weekly_albums,
            recent,
            mixes,
            top_picks,
            radio,
            anniversaries,
            followed,
            lounge,
            weekly,
        ) = tokio::join!(
            bounded(
                "missing essentials",
                self.missing_essentials(user_id, &shared)
            ),
            bounded(
                "weekly albums",
                self.lastfm_album_shelf(
                    &shared.lastfm_weekly_albums,
                    20,
                    "Your Top Albums This Week",
                    false,
                )
            ),
            bounded(
                "recent scrobbles",
                self.lastfm_album_shelf(&shared.lastfm_recent, 30, "Recently Scrobbled", true,)
            ),
            bounded(
                "daily mixes",
                self.daily_mixes(user_id, &user, primary, &seeds, &shared, &library_groups)
            ),
            bounded(
                "top picks",
                self.top_picks(user_id, &user, primary, &seeds, &shared)
            ),
            bounded(
                "radio",
                self.radio_sections(user_id, &user, primary, &seeds, &library_groups)
            ),
            bounded("anniversaries", self.anniversaries()),
            bounded("followed", self.new_from_followed(user_id)),
            bounded("listeners like you", async {
                match lb_user.as_deref() {
                    Some(username) => self.listeners_like_you(user_id, username).await,
                    None => None,
                }
            }),
            bounded("weekly exploration", async {
                match lb_user.as_deref() {
                    Some(username) => self.weekly_exploration(user_id, username).await,
                    None => None,
                }
            }),
        );
        page.missing_essentials = missing;
        page.lastfm_weekly_album_chart = weekly_albums;
        page.lastfm_recent_scrobbles = recent;
        page.daily_mixes = mixes;
        page.top_picks = top_picks;
        page.radio_sections = radio;
        page.anniversaries = anniversaries;
        page.new_from_followed = followed;
        page.listeners_like_you = lounge;
        page.weekly_exploration = weekly;

        page.rediscover = rediscover(&shared.jellyfin, self.now);
        page.artists_you_might_like =
            artists_you_might_like(&shared.similar, &mut seen_artists, primary);
        page.popular_in_your_genres = self
            .popular_in_genres(user_id, primary, &shared, &mut seen_artists)
            .await;
        page.genre_list = genre_list(shared.genres.as_deref());
        let similar_mbids: Vec<String> = shared
            .similar
            .iter()
            .flatten()
            .filter_map(|scored| scored.artist.mbid.clone())
            .collect();
        page.unexplored_genres = self
            .unexplored_genres(&page.because_you_listen_to, &similar_mbids)
            .await;
        let trending_source = if primary == MusicSource::LastFm && user.lastfm {
            Some("lastfm")
        } else {
            Some("listenbrainz")
        };
        page.globally_trending = artist_shelf(
            &shared.trending_artists,
            &mut seen_artists,
            "Globally Trending",
            trending_source,
        );
        page.lastfm_weekly_artist_chart = artist_shelf(
            &shared.lastfm_weekly_artists,
            &mut seen_artists,
            "Your Weekly Top Artists",
            Some("lastfm"),
        );
        page.service_prompts = shelves::service_prompts(
            lb_user.is_some(),
            user.jellyfin,
            self.settings.download_client,
            user.lastfm,
        );
        mark_ownership(self.library_pool(), &mut page).await;
        shelves::dedupe_albums(&mut page);
        Built {
            page,
            degraded: tracker.degraded(),
        }
    }

    fn library_pool(&self) -> &sqlx::SqlitePool {
        self.library.pool()
    }

    #[allow(clippy::too_many_arguments)]
    async fn first_round(
        &self,
        tracker: &Tracker,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        seeds: &[ArtistRow],
        lb_user: Option<&str>,
        lfm_user: Option<&str>,
    ) -> Shared {
        let sources = self.sources;
        // ListenBrainz's similar-artist read is popularity ranked, so it goes
        // empty in a popularity outage; Last.fm stands in then, and always
        // for users who chose Last.fm.
        let lastfm_similar = user.lastfm
            && (primary == MusicSource::LastFm || sources.listenbrainz_popularity_down());
        const SIMILAR_KEYS: [&str; SEEDS] = ["similar_0", "similar_1", "similar_2"];
        let similar = join_all(seeds.iter().take(SEEDS).enumerate().map(|(index, seed)| {
            let key = SIMILAR_KEYS[index];
            async move {
                let Some(mbid) = seed.mbid.as_deref() else {
                    return Vec::new();
                };
                let read = async {
                    if lastfm_similar {
                        sources.lastfm_similar_scored(user_id, seed, 20).await
                    } else {
                        sources.listenbrainz_similar_scored(user_id, mbid, 20).await
                    }
                };
                tracker.run(key, read).await.unwrap_or_default()
            }
        }));
        let trending = async {
            if primary == MusicSource::LastFm && user.lastfm {
                tracker
                    .run("lfm_global_top", sources.lastfm_chart_artists(user_id, 20))
                    .await
            } else {
                tracker
                    .run("lb_trending", sources.listenbrainz_sitewide_artists(20))
                    .await
            }
            .unwrap_or_default()
        };
        let lastfm_charts = async {
            let Some(username) = lfm_user else {
                return (Vec::new(), Vec::new(), Vec::new());
            };
            tokio::join!(
                async {
                    tracker
                        .run(
                            "lfm_weekly_artists",
                            sources.lastfm_weekly_artists(user_id, username),
                        )
                        .await
                        .unwrap_or_default()
                },
                async {
                    tracker
                        .run(
                            "lfm_weekly_albums",
                            sources.lastfm_weekly_albums(user_id, username),
                        )
                        .await
                        .unwrap_or_default()
                },
                async {
                    tracker
                        .run(
                            "lfm_recent",
                            sources.lastfm_recent_albums(user_id, username, 20),
                        )
                        .await
                        .unwrap_or_default()
                },
            )
        };
        let listenbrainz_user = async {
            let Some(username) = lb_user else {
                return (Vec::new(), None);
            };
            tokio::join!(
                async {
                    tracker
                        .run("lb_fresh", sources.listenbrainz_fresh_releases(username))
                        .await
                        .unwrap_or_default()
                },
                tracker.run("lb_genres", sources.listenbrainz_genre_counts(username)),
            )
        };
        let lastfm_genre_artists = async {
            match lfm_user {
                Some(username) if primary == MusicSource::LastFm => tracker
                    .run(
                        "lfm_user_top_artists_for_genres",
                        sources.lastfm_top_artists(user_id, username, 5),
                    )
                    .await
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        };
        let jellyfin = async {
            if !user.jellyfin {
                return Vec::new();
            }
            tracker
                .run("jf_most_played", sources.jellyfin_artist_plays(user_id, 50))
                .await
                .unwrap_or_default()
        };
        let library = async {
            tracker
                .run("library_albums", self.library.identified_albums(500))
                .await
                .unwrap_or_default()
        };
        let (
            similar,
            trending_artists,
            (lastfm_weekly_artists, lastfm_weekly_albums, lastfm_recent),
            (fresh, genres),
            lastfm_genre_artists,
            jellyfin,
            library_albums,
        ) = tokio::join!(
            similar,
            trending,
            lastfm_charts,
            listenbrainz_user,
            lastfm_genre_artists,
            jellyfin,
            library
        );
        Shared {
            similar,
            trending_artists,
            lastfm_weekly_artists,
            lastfm_weekly_albums,
            lastfm_recent,
            fresh,
            genres,
            lastfm_genre_artists,
            jellyfin,
            library_albums,
        }
    }

    /// Albums missing from the artists the library holds most of: up to
    /// three of each one's most played albums (v2 Missing Essentials).
    async fn missing_essentials(&self, user_id: &str, shared: &Shared) -> Option<ChartSection> {
        let mut per_artist: HashMap<String, usize> = HashMap::new();
        for album in &shared.library_albums {
            if let Some(artist) = &album.artist_mbid {
                *per_artist.entry(artist.clone()).or_insert(0) += 1;
            }
        }
        let owned: HashSet<&str> = shared
            .library_albums
            .iter()
            .map(|album| album.release_group_mbid.as_str())
            .collect();
        let mut artists: Vec<(String, usize)> = per_artist
            .into_iter()
            .filter(|(_, count)| *count >= ESSENTIALS_MIN_ALBUMS)
            .collect();
        artists.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        artists.truncate(10);
        let found = join_all(artists.iter().map(|(artist_mbid, _)| async move {
            or_empty(
                "listenbrainz artist albums",
                self.sources
                    .listenbrainz_artist_ranked(user_id, artist_mbid, 10)
                    .await,
            )
        }))
        .await;
        let mut missing: Vec<(i64, ChartAlbum)> = Vec::new();
        for albums in found {
            let mut taken = 0;
            for ranked in albums {
                if taken >= ESSENTIALS_PER_ARTIST {
                    break;
                }
                let group = ranked.album.release_group_mbid.to_lowercase();
                if group.is_empty() || owned.contains(group.as_str()) {
                    continue;
                }
                missing.push((
                    ranked.listen_count,
                    album(
                        Some(&ranked.album.release_group_mbid),
                        &ranked.album.title,
                        Some(&ranked.album.artist_name),
                        ranked.album.artist_mbid.as_deref(),
                        Some(ranked.listen_count),
                    ),
                ));
                taken += 1;
            }
        }
        missing.sort_by(|a, b| b.0.cmp(&a.0));
        shelf(
            "Missing Essentials",
            "albums",
            missing
                .into_iter()
                .take(SHELF_SIZE)
                .map(|(_, album)| SectionItem::Album(album))
                .collect(),
            Some("library"),
        )
    }

    /// A Last.fm album chart as a shelf. Last.fm names releases, so each id
    /// is matched to its release group first; rows that do not match in
    /// time keep their Last.fm art but carry no id (v2 passed the release
    /// id through as if it were a release group, which broke links).
    async fn lastfm_album_shelf(
        &self,
        albums: &[LastFmChartAlbum],
        window: usize,
        title: &str,
        dedupe: bool,
    ) -> Option<ChartSection> {
        let albums = &albums[..albums.len().min(window)];
        if albums.is_empty() {
            return None;
        }
        let releases: Vec<String> = albums.iter().filter_map(|a| a.mbid.clone()).collect();
        let settings = self.settings.queue.clone();
        let groups = match tokio::time::timeout(
            RESOLVE_BUDGET,
            resolve_release_groups(&self.queue_ctx(&settings), &releases),
        )
        .await
        {
            Ok(groups) => groups,
            Err(_) => {
                tracing::warn!(
                    "release group matching ran out of time; showing Last.fm rows without links"
                );
                HashMap::new()
            }
        };
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        for row in albums {
            let group =
                normalize(row.mbid.as_deref()).and_then(|release| groups.get(&release).cloned());
            let key = group
                .clone()
                .unwrap_or_else(|| format!("{}|{}", row.artist_name, row.name).to_lowercase());
            if dedupe && !seen.insert(key) {
                continue;
            }
            let mut item = album(
                group.as_deref(),
                &row.name,
                Some(&row.artist_name),
                None,
                (row.playcount > 0).then_some(row.playcount),
            );
            if item.image_url.is_none() && !row.image_url.is_empty() {
                item.image_url = Some(row.image_url.clone());
            }
            items.push(SectionItem::Album(item));
        }
        items.truncate(SHELF_SIZE);
        shelf(title, "albums", items, Some("lastfm"))
    }

    /// Candidate albums from artists similar to `seeds`, one pool per
    /// seed: ListenBrainz normally, Last.fm while ListenBrainz popularity
    /// is down.
    async fn similar_pools(
        &self,
        user_id: &str,
        user: &UserMusic,
        seeds: &[ArtistRow],
        excluded: &HashSet<String>,
        similar_limit: usize,
        albums_per: usize,
    ) -> Vec<Vec<crate::reads::discover::models::QueueItemLight>> {
        let mut settings = self.settings.queue.clone();
        settings.similar_artists_limit = similar_limit;
        settings.albums_per_similar = albums_per;
        let ctx = self.queue_ctx(&settings);
        if self.lastfm_for_popularity(user) {
            lastfm_pools(&ctx, user_id, seeds, excluded, similar_limit * albums_per).await
        } else {
            similar_artist_pools(&ctx, user_id, seeds, excluded).await
        }
    }

    /// Three to five genre-clustered mixes, roughly 60% new to 40% owned,
    /// built once a day per user and source.
    async fn daily_mixes(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        seeds: &[ArtistRow],
        shared: &Shared,
        library_groups: &HashSet<String>,
    ) -> Vec<ChartSection> {
        let memo_key = format!("{user_id}:{}:{}", source_label(primary), self.today);
        if let Some(mixes) = self.memo.mixes(&memo_key, self.now) {
            return mixes;
        }
        let mixes = self
            .build_daily_mixes(user_id, user, primary, seeds, shared, library_groups)
            .await;
        self.memo.store_mixes(memo_key, mixes.clone(), self.now);
        mixes
    }

    async fn build_daily_mixes(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        seeds: &[ArtistRow],
        shared: &Shared,
        library_groups: &HashSet<String>,
    ) -> Vec<ChartSection> {
        let mut preferred: Vec<String> = Vec::new();
        for (name, _) in shared.genres.iter().flatten() {
            let folded = fold(name);
            if !preferred.contains(&folded) {
                preferred.push(folded);
            }
        }
        if preferred.is_empty() {
            let seed_mbids: Vec<String> = seeds.iter().filter_map(|s| s.mbid.clone()).collect();
            let by_artist = or_empty(
                "artist genres",
                self.library.genres_for_artists(&seed_mbids).await,
            );
            for mbid in &seed_mbids {
                for genre in by_artist.get(&mbid.to_lowercase()).into_iter().flatten() {
                    let folded = fold(genre);
                    if !preferred.contains(&folded) {
                        preferred.push(folded);
                    }
                }
            }
        }
        let top = or_empty("top genres", self.library.top_genres(20).await);
        if top.is_empty() {
            return Vec::new();
        }
        let mut ordered = preferred.clone();
        for (genre, _) in &top {
            if !ordered.contains(genre) {
                ordered.push(genre.clone());
            }
        }
        let names: Vec<String> = ordered.iter().take(10).cloned().collect();
        let by_genre = or_empty(
            "genre artists",
            self.library.artists_for_genres(&names).await,
        );
        let mut seen = HashSet::new();
        let mut clusters: Vec<(String, Vec<String>)> = Vec::new();
        for genre in &ordered {
            let unique: Vec<String> = by_genre
                .get(genre)
                .into_iter()
                .flatten()
                .filter(|mbid| crate::providers::musicbrainz::is_valid_mbid(mbid))
                .filter(|mbid| !seen.contains(*mbid))
                .cloned()
                .collect();
            if unique.len() < 3 {
                continue;
            }
            seen.extend(unique.iter().cloned());
            clusters.push((genre.clone(), unique));
        }
        clusters.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        clusters.truncate(5);
        let mut mixes = Vec::new();
        for (index, (genre, artists)) in clusters.into_iter().enumerate() {
            if let Some(mix) = self
                .one_daily_mix(
                    user_id,
                    user,
                    primary,
                    index,
                    &genre,
                    artists,
                    library_groups,
                )
                .await
            {
                mixes.push(mix);
            }
        }
        mixes
    }

    #[allow(clippy::too_many_arguments)]
    async fn one_daily_mix(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        index: usize,
        genre: &str,
        mut artists: Vec<String>,
        library_groups: &HashSet<String>,
    ) -> Option<ChartSection> {
        let label = title_case(genre);
        daily_shuffler(&self.today, &["daily-mix", user_id, genre]).shuffle(&mut artists);
        artists.truncate(3);
        let names = if self.sources.listenbrainz_popularity_down() {
            vec![None; artists.len()]
        } else {
            join_all(artists.iter().map(|mbid| async move {
                self.sources
                    .listenbrainz_artist_ranked(user_id, mbid, 1)
                    .await
                    .ok()
                    .and_then(|rows| rows.into_iter().next())
                    .map(|row| row.album.artist_name)
                    .filter(|name| !name.is_empty())
            }))
            .await
        };
        let seeds: Vec<ArtistRow> = artists
            .iter()
            .zip(names)
            .map(|(mbid, name)| ArtistRow {
                name: name.unwrap_or_else(|| format!("{label} artist")),
                mbid: Some(mbid.clone()),
                listen_count: 0,
            })
            .collect();
        let pools = self
            .similar_pools(user_id, user, &seeds, library_groups, 10, 3)
            .await;
        let mut seen = HashSet::new();
        let fresh: Vec<ChartAlbum> = pools
            .into_iter()
            .flatten()
            .filter(|item| seen.insert(item.release_group_mbid.to_lowercase()))
            .map(|item| {
                album(
                    Some(&item.release_group_mbid),
                    &item.album_name,
                    Some(&item.artist_name),
                    Some(&item.artist_mbid),
                    None,
                )
            })
            .collect();
        let owned = or_empty(
            "genre albums",
            self.library.albums_by_genre(genre, 20).await,
        );
        let familiar: Vec<ChartAlbum> = owned
            .into_iter()
            .filter(|row| {
                let key = row
                    .release_group_mbid
                    .clone()
                    .unwrap_or_else(|| row.local_id.clone())
                    .to_lowercase();
                seen.insert(key)
            })
            .map(|row| {
                let mut item = album(
                    row.release_group_mbid.as_deref(),
                    &row.title,
                    row.artist_name.as_deref(),
                    row.artist_mbid.as_deref(),
                    None,
                );
                item.local_id = Some(row.local_id);
                item.in_library = true;
                item
            })
            .collect();
        let (new_count, familiar_count) = mix_split(fresh.len(), familiar.len());
        let items: Vec<SectionItem> = fresh
            .into_iter()
            .take(new_count)
            .chain(familiar.into_iter().take(familiar_count))
            .map(SectionItem::Album)
            .collect();
        shelf(
            &format!("Daily Mix {} - {label}", index + 1),
            "albums",
            items,
            Some(source_label(primary)),
        )
    }

    /// One radio shelf per seed artist: albums from similar artists, picked
    /// round-robin with at most two per artist.
    async fn radio_sections(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        seeds: &[ArtistRow],
        library_groups: &HashSet<String>,
    ) -> Vec<ChartSection> {
        let built = join_all(seeds.iter().take(SEEDS).filter(|s| s.mbid.is_some()).map(
            |seed| async move {
                let seed_mbid = seed.mbid.clone().unwrap_or_default();
                let pools = match tokio::time::timeout(
                    RADIO_BUDGET,
                    self.similar_pools(
                        user_id,
                        user,
                        std::slice::from_ref(seed),
                        library_groups,
                        15,
                        3,
                    ),
                )
                .await
                {
                    Ok(pools) => pools,
                    Err(_) => {
                        tracing::warn!(seed = %seed_mbid, "radio shelf timed out; leaving it out");
                        return None;
                    }
                };
                let mut shuffler = daily_shuffler(&self.today, &["radio", user_id, &seed_mbid]);
                let picked = round_robin(pools, 10, MAX_PER_ARTIST, &mut shuffler);
                let items = picked
                    .into_iter()
                    .map(|item| {
                        SectionItem::Album(album(
                            Some(&item.release_group_mbid),
                            &item.album_name,
                            Some(&item.artist_name),
                            Some(&item.artist_mbid),
                            None,
                        ))
                    })
                    .collect();
                let mut section = ChartSection {
                    title: format!("Radio: {}", seed.name),
                    section_type: "albums".to_owned(),
                    items,
                    source: Some(source_label(primary).to_owned()),
                    fallback_message: None,
                    connect_service: None,
                    radio_seed_type: Some("artist".to_owned()),
                    radio_seed_id: Some(seed_mbid),
                };
                section.items.truncate(10);
                Some(section)
            },
        ))
        .await;
        built.into_iter().flatten().collect()
    }

    /// Scored album picks, memoised per user and source for four hours
    /// (five minutes when ListenBrainz is down and nothing personal came
    /// back, so the next build tries again).
    async fn top_picks(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        seeds: &[ArtistRow],
        shared: &Shared,
    ) -> Option<TopPicksSection> {
        let memo_key = format!("{user_id}:{}", source_label(primary));
        if let Some(picks) = self.memo.picks(&memo_key, self.now) {
            return picks;
        }
        let (picks, degraded) = self
            .build_top_picks(user_id, user, primary, seeds, shared)
            .await;
        self.memo
            .store_picks(memo_key, picks.clone(), degraded, self.now);
        picks
    }

    async fn build_top_picks(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: MusicSource,
        seeds: &[ArtistRow],
        shared: &Shared,
    ) -> (Option<TopPicksSection>, bool) {
        let settings = self.settings.queue.clone();
        let listened = listened_albums(&self.queue_ctx(&settings), user, primary).await;
        let ignored = self
            .queue_db
            .ignored_mbids(user_id)
            .await
            .unwrap_or_else(|cause| {
                tracing::warn!(%cause, "ignored releases unreadable; top picks may show one");
                HashSet::new()
            });
        let exclude: HashSet<String> = listened.union(&ignored).cloned().collect();

        // Similarity from the reads the Because shelves already made.
        let mut similar: Vec<(String, String, f64, String)> = Vec::new();
        for (seed, rows) in seeds.iter().zip(&shared.similar) {
            for scored in rows {
                let Some(mbid) = normalize(scored.artist.mbid.as_deref()) else {
                    continue;
                };
                if mbid == VARIOUS_ARTISTS_MBID {
                    continue;
                }
                similar.push((
                    mbid,
                    scored.artist.name.clone(),
                    scored.score,
                    seed.name.clone(),
                ));
            }
        }
        let max_raw = similar.iter().map(|row| row.2).fold(0.0_f64, f64::max);
        let mut best: HashMap<String, (f64, String, String)> = HashMap::new();
        for (mbid, name, raw, seed) in similar {
            let sim = if max_raw > 1.0 {
                raw / max_raw
            } else {
                raw.min(1.0)
            };
            if best.get(&mbid).is_none_or(|current| sim > current.0) {
                best.insert(mbid, (sim, name, seed));
            }
        }
        let mut ranked: Vec<(String, (f64, String, String))> = best.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
        ranked.truncate(20);

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let lastfm_route = self.lastfm_for_popularity(user);
        if lastfm_route {
            match tokio::time::timeout(
                PICKS_SIMILARITY_BUDGET,
                self.lastfm_pick_candidates(user_id, &ranked, &exclude),
            )
            .await
            {
                Ok(found) => {
                    for candidate in found {
                        if seen.insert(candidate.release_group_mbid.clone()) {
                            candidates.push(candidate);
                        }
                    }
                }
                Err(_) => tracing::warn!(
                    "top picks ran out of time matching Last.fm albums; using the chart only"
                ),
            }
        } else {
            let albums = join_all(ranked.iter().map(|(mbid, _)| async move {
                self.sources
                    .listenbrainz_artist_ranked(user_id, mbid, 2)
                    .await
                    .unwrap_or_default()
            }))
            .await;
            for ((artist_mbid, (sim, artist_name, seed)), rows) in ranked.iter().zip(albums) {
                for row in rows {
                    let Some(group) = normalize(Some(&row.album.release_group_mbid)) else {
                        continue;
                    };
                    if exclude.contains(&group) || !seen.insert(group.clone()) {
                        continue;
                    }
                    candidates.push(Candidate {
                        release_group_mbid: group,
                        album_name: row.album.title,
                        artist_name: if row.album.artist_name.is_empty() {
                            artist_name.clone()
                        } else {
                            row.album.artist_name
                        },
                        artist_mbid: artist_mbid.clone(),
                        sim: *sim,
                        listen_count: row.listen_count,
                        seed_artist: Some(seed.clone()).filter(|s| !s.is_empty()),
                        from_trending: false,
                    });
                }
            }
        }
        let trending = match tokio::time::timeout(
            Duration::from_secs(30),
            self.sources.listenbrainz_trending_ranked(100),
        )
        .await
        {
            Ok(Ok(rows)) => rows,
            _ => Vec::new(),
        };
        for row in trending {
            let Some(group) = normalize(Some(&row.album.release_group_mbid)) else {
                continue;
            };
            let artist_mbid = normalize(row.album.artist_mbid.as_deref()).unwrap_or_default();
            if artist_mbid == VARIOUS_ARTISTS_MBID
                || exclude.contains(&group)
                || !seen.insert(group.clone())
            {
                continue;
            }
            candidates.push(Candidate {
                release_group_mbid: group,
                album_name: row.album.title,
                artist_name: row.album.artist_name,
                artist_mbid,
                sim: 0.0,
                listen_count: row.listen_count,
                seed_artist: None,
                from_trending: true,
            });
        }
        let ids: Vec<String> = candidates
            .iter()
            .map(|c| c.release_group_mbid.clone())
            .collect();
        let owned = owned_album_ids(self.library_pool(), &ids).await;
        candidates.retain(|candidate| !owned.contains_key(&candidate.release_group_mbid));
        let personal = candidates.iter().filter(|c| !c.from_trending).count();
        let degraded = lastfm_route && personal == 0;
        if candidates.is_empty() {
            return (None, degraded);
        }

        let mut user_genres: HashSet<String> = shared
            .genres
            .iter()
            .flatten()
            .map(|(name, _)| name.to_lowercase())
            .collect();
        let seed_mbids: Vec<String> = seeds.iter().filter_map(|s| s.mbid.clone()).collect();
        if let Ok(by_seed) = self.library.genres_for_artists(&seed_mbids).await {
            user_genres.extend(by_seed.into_values().flatten().map(|g| g.to_lowercase()));
        }
        let artist_mbids: Vec<String> = candidates
            .iter()
            .map(|c| c.artist_mbid.clone())
            .filter(|mbid| !mbid.is_empty())
            .collect();
        let genres_by_artist = self
            .library
            .genres_for_artists(&artist_mbids)
            .await
            .unwrap_or_default();
        let picked = picks::score(
            candidates,
            user_id,
            &self.today,
            &user_genres,
            &genres_by_artist,
            self.settings.genre_weight,
            self.settings.picks_count,
        );
        if picked.is_empty() {
            return (None, degraded);
        }
        tracing::info!(
            picks = picked.len(),
            personalised = picked.iter().filter(|p| !p.candidate.from_trending).count(),
            degraded,
            "top picks built"
        );
        let section = TopPicksSection {
            title: "Top Picks for You".to_owned(),
            items: picked
                .into_iter()
                .map(|pick| {
                    let candidate = pick.candidate;
                    TopPickItem {
                        album: album(
                            Some(&candidate.release_group_mbid),
                            &candidate.album_name,
                            Some(&candidate.artist_name),
                            Some(&candidate.artist_mbid),
                            (candidate.listen_count > 0).then_some(candidate.listen_count),
                        ),
                        match_pct: pick.match_pct,
                        reasons: pick.reasons,
                        seed_artist: candidate.seed_artist,
                    }
                })
                .collect(),
            source: Some(source_label(primary).to_owned()),
            personalizing: degraded,
        };
        (Some(section), degraded)
    }

    /// Top Picks candidates from Last.fm's top albums of each similar
    /// artist, matched to release groups (used only while ListenBrainz
    /// popularity is down). Last.fm play counts are not on ListenBrainz's
    /// scale, so the popularity signal stays neutral.
    async fn lastfm_pick_candidates(
        &self,
        user_id: &str,
        ranked: &[(String, (f64, String, String))],
        exclude: &HashSet<String>,
    ) -> Vec<Candidate> {
        let rows: Vec<ArtistRow> = ranked
            .iter()
            .map(|(mbid, (_, name, _))| ArtistRow {
                name: name.clone(),
                mbid: Some(mbid.clone()),
                listen_count: 0,
            })
            .collect();
        let albums = join_all(
            rows.iter()
                .map(|artist| self.sources.lastfm_artist_albums(user_id, artist, 3)),
        )
        .await;
        let releases: Vec<String> = albums
            .iter()
            .flatten()
            .flatten()
            .filter_map(|album| album.mbid.clone())
            .collect();
        let settings = self.settings.queue.clone();
        let groups = resolve_release_groups(&self.queue_ctx(&settings), &releases).await;
        let mut candidates = Vec::new();
        for ((artist_mbid, (sim, artist_name, seed)), found) in ranked.iter().zip(albums) {
            for row in found.unwrap_or_default() {
                let Some(group) =
                    normalize(row.mbid.as_deref()).and_then(|r| groups.get(&r).cloned())
                else {
                    continue;
                };
                if exclude.contains(&group) || candidates.len() >= 40 {
                    continue;
                }
                candidates.push(Candidate {
                    release_group_mbid: group,
                    album_name: row.name,
                    artist_name: artist_name.clone(),
                    artist_mbid: artist_mbid.clone(),
                    sim: *sim,
                    listen_count: 0,
                    seed_artist: Some(seed.clone()).filter(|s| !s.is_empty()),
                    from_trending: false,
                });
            }
        }
        candidates
    }

    /// Albums ListenBrainz users with similar taste play this month.
    async fn listeners_like_you(&self, user_id: &str, username: &str) -> Option<ChartSection> {
        let names = self
            .sources
            .listenbrainz_similar_users(username)
            .await
            .unwrap_or_else(|cause| {
                tracing::debug!(%cause, "similar listeners unavailable");
                Vec::new()
            });
        let names: Vec<String> = names.into_iter().take(3).collect();
        if names.is_empty() {
            return None;
        }
        let ignored = self
            .queue_db
            .ignored_mbids(user_id)
            .await
            .unwrap_or_default();
        let lists = join_all(names.iter().map(|name| {
            self.sources
                .listenbrainz_user_ranked(name, StatsRange::ThisMonth, 15)
        }))
        .await;
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        'lists: for list in lists.into_iter().flatten() {
            for row in list {
                let Some(group) = normalize(Some(&row.album.release_group_mbid)) else {
                    continue;
                };
                let artist_mbid = normalize(row.album.artist_mbid.as_deref());
                if ignored.contains(&group)
                    || artist_mbid.as_deref() == Some(VARIOUS_ARTISTS_MBID)
                    || !seen.insert(group.clone())
                {
                    continue;
                }
                items.push(SectionItem::Album(album(
                    Some(&group),
                    &row.album.title,
                    Some(&row.album.artist_name),
                    artist_mbid.as_deref(),
                    (row.listen_count > 0).then_some(row.listen_count),
                )));
                if items.len() >= SHELF_SIZE {
                    break 'lists;
                }
            }
        }
        shelf(
            "Listeners Like You Are Playing",
            "albums",
            items,
            Some("listenbrainz"),
        )
    }

    /// The user's ListenBrainz weekly exploration playlist, with covers
    /// from each track's release group where it can be found in time.
    async fn weekly_exploration(&self, user_id: &str, username: &str) -> Option<WeeklyExploration> {
        let playlist = match self
            .sources
            .listenbrainz_weekly_playlist(user_id, username)
            .await
        {
            Ok(Some(playlist)) => playlist,
            Ok(None) => return None,
            Err(cause) => {
                tracing::warn!(%cause, "weekly exploration unavailable");
                return None;
            }
        };
        let recordings: Vec<String> = playlist
            .tracks
            .iter()
            .filter_map(|track| track.recording_mbid.clone())
            .collect();
        let by_recording = self
            .sources
            .listenbrainz_recording_groups(user_id, &recordings)
            .await
            .unwrap_or_default();
        let group_of = |recording: &Option<String>| {
            recording
                .as_deref()
                .and_then(|r| by_recording.get(&r.to_ascii_lowercase()).cloned())
        };
        let mut releases: Vec<String> = playlist
            .tracks
            .iter()
            .filter(|track| group_of(&track.recording_mbid).is_none())
            .filter_map(|track| track.caa_release_mbid.clone())
            .collect();
        releases.sort();
        releases.dedup();
        let release_groups: HashMap<String, String> = match tokio::time::timeout(
            RESOLVE_BUDGET,
            join_all(releases.iter().map(|release| async move {
                let group = self
                    .sources
                    .musicbrainz_release_group_of(release)
                    .await
                    .ok()
                    .flatten();
                (release.clone(), group)
            })),
        )
        .await
        {
            Ok(found) => found
                .into_iter()
                .filter_map(|(release, group)| Some((release, group?)))
                .collect(),
            Err(_) => {
                tracing::warn!(
                    "weekly exploration cover lookups ran out of time; using release covers"
                );
                HashMap::new()
            }
        };
        let tracks = playlist
            .tracks
            .into_iter()
            .map(|track| {
                let group = group_of(&track.recording_mbid).or_else(|| {
                    track
                        .caa_release_mbid
                        .as_ref()
                        .and_then(|release| release_groups.get(release).cloned())
                });
                let cover_url = match (&group, &track.caa_release_mbid) {
                    (Some(group), _) => {
                        Some(format!("/api/v3/covers/release-group/{group}?size=250"))
                    }
                    (None, Some(release)) => {
                        Some(format!("/api/v3/covers/release/{release}?size=250"))
                    }
                    (None, None) => None,
                };
                WeeklyTrack {
                    title: track.title,
                    artist_name: track.creator,
                    album_name: track.album,
                    recording_mbid: track.recording_mbid,
                    artist_mbid: track.artist_mbid,
                    release_group_mbid: group,
                    cover_url,
                    duration_ms: track.duration_ms,
                }
            })
            .collect();
        Some(WeeklyExploration {
            title: playlist.title,
            playlist_date: playlist.date,
            tracks,
            source_url: playlist.source_url,
        })
    }

    /// Library albums turning 10, 20, 25, ... years old this year,
    /// roundest birthdays first.
    async fn anniversaries(&self) -> Option<ChartSection> {
        let mut albums = or_empty(
            "anniversary albums",
            self.library
                .anniversary_albums(self.year, &ANNIVERSARY_YEARS, 12)
                .await,
        );
        albums.sort_by(|a, b| {
            let age = |row: &super::library::ShelfAlbum| self.year - row.year.unwrap_or(self.year);
            age(b).cmp(&age(a)).then(b.year.cmp(&a.year))
        });
        let items = albums
            .into_iter()
            .take(12)
            .map(|row| {
                let mut item = album(
                    row.release_group_mbid.as_deref(),
                    &row.title,
                    row.artist_name.as_deref(),
                    row.artist_mbid.as_deref(),
                    None,
                );
                item.local_id = Some(row.local_id);
                item.release_date = row.year.map(|year| year.to_string());
                item.in_library = true;
                SectionItem::Album(item)
            })
            .collect();
        shelf("Milestone Anniversaries", "albums", items, Some("library"))
    }

    /// The newest releases from artists the user follows.
    async fn new_from_followed(&self, user_id: &str) -> Option<ChartSection> {
        let rows = or_empty(
            "followed releases",
            self.library.followed_releases(user_id, 10).await,
        );
        let items = rows
            .into_iter()
            .map(|row| {
                let mut item = album(
                    Some(&row.release_group_mbid),
                    &row.title,
                    Some(&row.artist_name),
                    Some(&row.artist_mbid),
                    None,
                );
                item.release_date = row.first_release_date;
                SectionItem::Album(item)
            })
            .collect();
        shelf(
            "New From Artists You Follow",
            "albums",
            items,
            Some("library"),
        )
    }

    /// Artists popular in the user's top genres: MusicBrainz tag search
    /// over ListenBrainz genres, or Last.fm tag charts for Last.fm users.
    async fn popular_in_genres(
        &self,
        user_id: &str,
        primary: MusicSource,
        shared: &Shared,
        seen: &mut HashSet<String>,
    ) -> Option<ChartSection> {
        if primary == MusicSource::LastFm {
            return self.popular_in_genres_lastfm(user_id, shared, seen).await;
        }
        let genres: Vec<String> = shared
            .genres
            .iter()
            .flatten()
            .take(3)
            .map(|(name, _)| name.clone())
            .collect();
        if genres.is_empty() {
            return None;
        }
        let found = join_all(
            genres
                .iter()
                .map(|genre| self.sources.musicbrainz_tag_artists(genre, 10)),
        )
        .await;
        let mut items = Vec::new();
        for row in found.into_iter().flat_map(|r| or_empty("tag artists", r)) {
            let Some(mbid) = normalize(row.mbid.as_deref()) else {
                continue;
            };
            if seen.insert(mbid.clone()) {
                items.push(artist(row.mbid.as_deref(), &row.name, None));
            }
        }
        items.truncate(SHELF_SIZE);
        shelf(
            "Popular In Your Genres",
            "artists",
            items,
            Some("musicbrainz"),
        )
    }

    async fn popular_in_genres_lastfm(
        &self,
        user_id: &str,
        shared: &Shared,
        seen: &mut HashSet<String>,
    ) -> Option<ChartSection> {
        let top = &shared.lastfm_genre_artists;
        if top.is_empty() {
            return None;
        }
        let tags = join_all(
            top.iter()
                .take(5)
                .map(|artist| self.sources.lastfm_artist_tags(user_id, artist)),
        )
        .await;
        let mut genres: Vec<String> = Vec::new();
        'tags: for found in tags {
            for tag in or_empty("lastfm artist tags", found).into_iter().take(2) {
                if !genres.iter().any(|g| g.eq_ignore_ascii_case(&tag)) {
                    genres.push(tag);
                    if genres.len() >= 3 {
                        break 'tags;
                    }
                }
            }
        }
        if genres.is_empty() {
            return None;
        }
        let found = join_all(
            genres
                .iter()
                .map(|genre| self.sources.lastfm_tag_artists(user_id, genre, 10)),
        )
        .await;
        let mut items = Vec::new();
        for row in found
            .into_iter()
            .flat_map(|r| or_empty("lastfm tag artists", r))
        {
            let Some(mbid) = normalize(row.mbid.as_deref()) else {
                continue;
            };
            if seen.insert(mbid) {
                items.push(artist(
                    row.mbid.as_deref(),
                    &row.name,
                    Some(row.listen_count),
                ));
            }
        }
        items.truncate(SHELF_SIZE);
        shelf("Popular In Your Genres", "artists", items, Some("lastfm"))
    }

    /// Genres around the user's similar artists that the library barely
    /// has, else genres the library holds only one artist of.
    async fn unexplored_genres(
        &self,
        because: &[BecauseYouListenTo],
        similar_mbids: &[String],
    ) -> Option<ChartSection> {
        let mut candidates: Vec<String> = similar_mbids.to_vec();
        for entry in because {
            for item in &entry.section.items {
                if let SectionItem::Artist(row) = item
                    && let Some(mbid) = &row.mbid
                {
                    candidates.push(mbid.clone());
                }
            }
        }
        let by_artist = self.library.genres_for_artists(&candidates).await.ok()?;
        let mut display: Vec<(String, String)> = Vec::new();
        let mut keys: Vec<&String> = by_artist.keys().collect();
        keys.sort();
        for key in keys {
            for name in &by_artist[key] {
                let folded = fold(name);
                if !display.iter().any(|(f, _)| *f == folded) {
                    display.push((folded, name.clone()));
                }
            }
        }
        let names: Vec<String> = display.iter().map(|(_, name)| name.clone()).collect();
        let counts = self
            .library
            .genre_artist_counts(&names)
            .await
            .unwrap_or_default();
        let top = self.library.top_genres(20).await.unwrap_or_default();
        let top_set: HashSet<&str> = top.iter().map(|(genre, _)| genre.as_str()).collect();
        let mut filtered: Vec<(String, i64)> = display
            .into_iter()
            .filter_map(|(folded, name)| {
                let count = counts.get(&folded).copied().unwrap_or(0);
                (count < UNEXPLORED_THRESHOLD && !top_set.contains(folded.as_str()))
                    .then_some((name, count))
            })
            .collect();
        daily_shuffler(&self.today, &["unexplored"]).shuffle(&mut filtered);
        filtered.truncate(UNEXPLORED_MAX);
        let items: Vec<SectionItem> = if filtered.is_empty() {
            let known: HashSet<String> = top.iter().map(|(genre, _)| genre.clone()).collect();
            let all = self.library.top_genres(100_000).await.unwrap_or_default();
            let mut fallback: Vec<(String, i64)> = all
                .into_iter()
                .filter(|(genre, count)| {
                    (1..UNEXPLORED_THRESHOLD.max(1)).contains(count) && !known.contains(genre)
                })
                .collect();
            daily_shuffler(&self.today, &["unexplored-fallback"]).shuffle(&mut fallback);
            fallback
                .into_iter()
                .take(UNEXPLORED_MAX)
                .map(|(genre_name, count)| genre(&title_case(&genre_name), None, Some(count)))
                .collect()
        } else {
            filtered
                .into_iter()
                .map(|(name, count)| genre(&name, None, Some(count)))
                .collect()
        };
        shelf("Genres to Explore", "genres", items, None)
    }
}

/// The 60/40 new-to-familiar split of a Daily Mix, topped up from
/// whichever side has more when the other runs short.
fn mix_split(fresh: usize, familiar: usize) -> (usize, usize) {
    let mut new_count = fresh.min((MIX_SIZE as f64 * 0.6).round() as usize);
    let mut familiar_count = familiar.min(MIX_SIZE - new_count);
    if new_count + familiar_count < MIX_SIZE {
        new_count += (fresh - new_count).min(MIX_SIZE - new_count - familiar_count);
        familiar_count += (familiar - familiar_count).min(MIX_SIZE - new_count - familiar_count);
    }
    (new_count, familiar_count)
}

/// One "Because You Listen To" shelf per seed with similar artists. A
/// later seed needs at least three artists no earlier shelf showed.
fn because_sections(
    seeds: &[ArtistRow],
    similar: &[Vec<ScoredArtist>],
    seen: &mut HashSet<String>,
    primary: MusicSource,
) -> Vec<BecauseYouListenTo> {
    let mut sections = Vec::new();
    for (seed, rows) in seeds.iter().zip(similar) {
        let mut items = Vec::new();
        for scored in rows {
            let Some(mbid) = normalize(scored.artist.mbid.as_deref()) else {
                continue;
            };
            if !seen.insert(mbid) {
                continue;
            }
            items.push(artist(
                scored.artist.mbid.as_deref(),
                &scored.artist.name,
                Some(scored.artist.listen_count),
            ));
        }
        if items.is_empty() || (items.len() < 3 && !sections.is_empty()) {
            continue;
        }
        items.truncate(SHELF_SIZE);
        let Some(section) = shelf(
            &format!("Because You Listen To {}", seed.name),
            "artists",
            items,
            Some(source_label(primary)),
        ) else {
            continue;
        };
        sections.push(BecauseYouListenTo {
            seed_artist: seed.name.clone(),
            seed_artist_mbid: seed.mbid.clone().unwrap_or_default(),
            section,
            listen_count: seed.listen_count,
            banner_url: None,
            wide_thumb_url: None,
            fanart_url: None,
        });
    }
    sections
}

/// Similar artists no Because shelf showed, most listened first.
fn artists_you_might_like(
    similar: &[Vec<ScoredArtist>],
    seen: &mut HashSet<String>,
    primary: MusicSource,
) -> Option<ChartSection> {
    let mut rows: Vec<&ArtistRow> = Vec::new();
    for scored in similar.iter().flatten() {
        let Some(mbid) = normalize(scored.artist.mbid.as_deref()) else {
            continue;
        };
        if seen.insert(mbid) {
            rows.push(&scored.artist);
        }
    }
    rows.sort_by(|a, b| b.listen_count.cmp(&a.listen_count));
    let items = rows
        .into_iter()
        .take(SHELF_SIZE)
        .map(|row| artist(row.mbid.as_deref(), &row.name, Some(row.listen_count)))
        .collect();
    shelf(
        "Artists You Might Like",
        "artists",
        items,
        Some(source_label(primary)),
    )
}

/// An artist chart as a shelf, skipping artists already on the page.
fn artist_shelf(
    rows: &[ArtistRow],
    seen: &mut HashSet<String>,
    title: &str,
    source: Option<&str>,
) -> Option<ChartSection> {
    let mut items = Vec::new();
    for row in rows.iter().take(20) {
        let Some(mbid) = normalize(row.mbid.as_deref()) else {
            continue;
        };
        if seen.insert(mbid) {
            items.push(artist(
                row.mbid.as_deref(),
                &row.name,
                Some(row.listen_count),
            ));
        }
    }
    items.truncate(SHELF_SIZE);
    shelf(title, "artists", items, source)
}

/// Recent releases by artists the user listens to.
fn fresh_releases(rows: &[AlbumRow]) -> Option<ChartSection> {
    let items = rows
        .iter()
        .take(SHELF_SIZE)
        .map(|row| {
            SectionItem::Album(album(
                Some(&row.release_group_mbid),
                &row.title,
                Some(&row.artist_name),
                row.artist_mbid.as_deref(),
                None,
            ))
        })
        .collect();
    shelf(
        "Fresh Releases For You",
        "albums",
        items,
        Some("listenbrainz"),
    )
}

/// Jellyfin artists played a lot but not in the last three months.
fn rediscover(plays: &[super::sources::PlayedArtist], now: f64) -> Option<ChartSection> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for row in plays {
        if row.play_count < REDISCOVER_MIN_PLAYS {
            continue;
        }
        let Some(idle) = row
            .last_played
            .as_deref()
            .and_then(|iso| days_since(iso, now))
        else {
            continue;
        };
        if idle < REDISCOVER_IDLE_DAYS || !seen.insert(row.name.to_lowercase()) {
            continue;
        }
        let image_url = match &row.mbid {
            Some(mbid) => Some(format!("/api/v3/covers/artist/{mbid}?size=500")),
            None => row.image_url.clone(),
        };
        items.push(SectionItem::Artist(
            crate::reads::discover::models::ChartArtist {
                name: row.name.clone(),
                mbid: row.mbid.clone(),
                local_id: None,
                image_url,
                listen_count: Some(row.play_count),
                in_library: false,
                source: None,
            },
        ));
        if items.len() >= SHELF_SIZE {
            break;
        }
    }
    shelf("Rediscover", "artists", items, Some("jellyfin"))
}

/// Browse by Genre: the user's ListenBrainz genres, else a default list.
fn genre_list(genres: Option<&[(String, i64)]>) -> Option<ChartSection> {
    match genres.filter(|g| !g.is_empty()) {
        Some(genres) => shelf(
            "Browse by Genre",
            "genres",
            genres
                .iter()
                .take(20)
                .map(|(name, count)| genre(name, Some(*count), None))
                .collect(),
            Some("listenbrainz"),
        ),
        None => shelf(
            "Browse by Genre",
            "genres",
            shelves::DEFAULT_GENRES
                .iter()
                .map(|name| genre(name, None, None))
                .collect(),
            Some("library"),
        ),
    }
}

/// Owned release groups among `ids`, lowercased, mapped to local ids.
async fn owned_album_ids(pool: &sqlx::SqlitePool, ids: &[String]) -> HashMap<String, String> {
    let mut owned = HashMap::new();
    for chunk in ids.chunks(200) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match ownership::owned_albums(pool, &refs).await {
            Ok(found) => owned.extend(found),
            Err(error) => tracing::warn!(%error, "album ownership unreadable; marking none owned"),
        }
    }
    owned
}

/// Mark which albums and artists on the page the library holds, and drop
/// owned albums from Missing Essentials.
pub async fn mark_ownership(pool: &sqlx::SqlitePool, page: &mut DiscoverResponse) {
    let mut sections: Vec<&mut ChartSection> = Vec::new();
    for entry in &mut page.because_you_listen_to {
        sections.push(&mut entry.section);
    }
    for section in [
        page.fresh_releases.as_mut(),
        page.missing_essentials.as_mut(),
        page.rediscover.as_mut(),
        page.artists_you_might_like.as_mut(),
        page.popular_in_your_genres.as_mut(),
        page.globally_trending.as_mut(),
        page.lastfm_weekly_artist_chart.as_mut(),
        page.lastfm_weekly_album_chart.as_mut(),
        page.lastfm_recent_scrobbles.as_mut(),
        page.listeners_like_you.as_mut(),
        page.anniversaries.as_mut(),
        page.new_from_followed.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        sections.push(section);
    }
    sections.extend(page.daily_mixes.iter_mut());
    sections.extend(page.radio_sections.iter_mut());
    let mut album_ids: Vec<String> = Vec::new();
    let mut artist_ids: Vec<String> = Vec::new();
    for section in &sections {
        for item in &section.items {
            match item {
                SectionItem::Album(row) => album_ids.extend(row.mbid.clone()),
                SectionItem::Artist(row) => artist_ids.extend(row.mbid.clone()),
                _ => {}
            }
        }
    }
    if let Some(picks) = &page.top_picks {
        album_ids.extend(
            picks
                .items
                .iter()
                .filter_map(|pick| pick.album.mbid.clone()),
        );
    }
    let owned_albums = owned_album_ids(pool, &album_ids).await;
    let mut owned_artists = HashMap::new();
    for chunk in artist_ids.chunks(200) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match ownership::owned_artists(pool, &refs).await {
            Ok(found) => owned_artists.extend(found),
            Err(error) => tracing::warn!(%error, "artist ownership unreadable; marking none owned"),
        }
    }
    let mark_album = |row: &mut ChartAlbum| {
        if let Some(local) = row
            .mbid
            .as_deref()
            .and_then(|m| owned_albums.get(&m.to_lowercase()))
        {
            row.in_library = true;
            row.local_id.get_or_insert_with(|| local.clone());
        }
    };
    for section in sections {
        for item in &mut section.items {
            match item {
                SectionItem::Album(row) => mark_album(row),
                SectionItem::Artist(row) => {
                    if let Some(local) = row
                        .mbid
                        .as_deref()
                        .and_then(|m| owned_artists.get(&m.to_lowercase()))
                    {
                        row.in_library = true;
                        row.local_id.get_or_insert_with(|| local.clone());
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(picks) = &mut page.top_picks {
        for pick in &mut picks.items {
            mark_album(&mut pick.album);
        }
    }
    if let Some(missing) = &mut page.missing_essentials {
        missing
            .items
            .retain(|item| !matches!(item, SectionItem::Album(row) if row.in_library));
        if missing.items.is_empty() {
            page.missing_essentials = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mix_split;

    #[test]
    fn mix_split_prefers_sixty_forty_and_tops_up_the_short_side() {
        assert_eq!(mix_split(20, 20), (7, 5));
        assert_eq!(mix_split(2, 20), (2, 10));
        assert_eq!(mix_split(20, 1), (11, 1));
    }
}
