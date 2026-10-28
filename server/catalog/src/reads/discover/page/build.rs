//! One discover page build for one user (v2 `build_discover_data`).
//!
//! A build makes the sections the user has switched on, from the section
//! list ([`sections`]). It runs in two rounds. The first makes the reads
//! several sections share (seed artists, their similar artists, charts,
//! the user's genres, Jellyfin plays, the library's identified albums),
//! only those a wanted section needs, every read bounded to 25 seconds
//! so one slow service only costs its own sections. The second builds
//! the sections in parallel under the same bound, except the artist
//! sections, which are built one after another in page order and share a
//! "seen" set so an artist appears once on the page. Album sections are
//! then deduplicated by rank. Ownership is marked when the page is served
//! ([`mark_ownership`]), so a library change never makes a page wrong.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::future::join_all;

use super::library::{LibraryReads, fold};
use super::memo::{BuildMemo, MemoKey};
use super::picks::{self, Candidate};
use super::shelves::{self, RowMut, SHELF_SIZE, album, artist, genre, one, shelf};
use crate::reads::discover::cache::Variant;
use crate::reads::discover::listening::{
    AlbumRow, ArtistRow, Ask, LastFmChartAlbum, ListenSource, ListenedAlbum, ListeningData,
    PlayedArtist, ScoredArtist, StatsRange, TAG_PAGE, UserMusic,
};
use crate::reads::discover::models::{
    AlbumRef, BecauseYouListenTo, ChartSection, DiscoverResponse, RadioSeedKind, SectionContent,
    SectionItem, SectionKey, SectionKind, ServiceState, ShelfSource, TopPickItem, TopPicksSection,
    WeeklyExploration, WeeklyTrack,
};
use crate::reads::discover::ownership;
use crate::reads::discover::queue::QueueSettings;
use crate::reads::discover::queue::build::{
    BuildContext, VARIOUS_ARTISTS_MBID, lastfm_pools, listened_albums, normalize, or_empty,
    resolve_release_groups, seed_artists, similar_artist_pools,
};
use crate::reads::discover::queue::select::{MAX_PER_ARTIST, Shuffler, round_robin};
use crate::reads::discover::queue::store::QueueDb;
use crate::reads::discover::sections::{self, Dedupe, Input, Page, SectionDef};

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
/// Popular In Your Genres: the user's top genres it reads.
const TOP_GENRES: usize = 3;
/// Popular In Your Genres: a Last.fm user's top artists whose tags it reads.
const GENRE_ARTISTS: usize = 5;
/// Tags of a Last.fm top artist kept as its genres.
const LEARNED_ARTIST_TAGS: usize = 5;
/// How long a learned artist genre is kept after a build last saw it.
const GENRES_KEPT_MS: i64 = 30 * 86_400_000;

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
    pub listening: &'a ListeningData,
    /// The build's background priority and its cap on provider calls in
    /// flight, shared by every read the build makes.
    pub ask: Ask,
    /// The queue tables (ignores, remembered release groups).
    pub queue_db: &'a QueueDb,
    /// Library reads.
    pub library: &'a LibraryReads,
    /// Daily Mix and Top Picks memos.
    pub memo: &'a BuildMemo,
    /// What the page is built for, which the memos are kept under.
    pub variant: &'a Variant,
    /// The clear generation the build was claimed in.
    pub generation: i64,
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
    /// Each listening service all of whose reads failed.
    pub degraded: HashMap<ListenSource, ServiceState>,
}

/// Failed reads of the first round, for the degraded banner.
#[derive(Default)]
struct Tracker {
    attempted: Mutex<Vec<&'static str>>,
    failed: Mutex<HashSet<&'static str>>,
}

impl Tracker {
    /// Run one read under the task bound. `None` when it failed.
    async fn run<T, E: std::fmt::Display>(
        &self,
        key: &'static str,
        read: impl Future<Output = Result<T, E>>,
    ) -> Option<T> {
        if let Ok(mut attempted) = self.attempted.lock() {
            attempted.push(key);
        }
        let outcome = match tokio::time::timeout(TASK_TIMEOUT, read).await {
            Ok(Ok(value)) => return Some(value),
            Ok(Err(cause)) => cause.to_string(),
            Err(_) => "timed out".to_owned(),
        };
        tracing::warn!(task = key, cause = %outcome, "discover read failed; its shelves stay empty");
        if let Ok(mut failed) = self.failed.lock() {
            failed.insert(key);
        }
        None
    }

    /// Services whose every attempted read failed.
    fn degraded(&self) -> HashMap<ListenSource, ServiceState> {
        let attempted = self.attempted.lock().map(|a| a.clone()).unwrap_or_default();
        let failed = self.failed.lock().map(|f| f.clone()).unwrap_or_default();
        let mut status = HashMap::new();
        for (prefix, source) in [
            ("lb_", ListenSource::Listenbrainz),
            ("lfm_", ListenSource::Lastfm),
        ] {
            let family: Vec<&&str> = attempted.iter().filter(|k| k.starts_with(prefix)).collect();
            if !family.is_empty() && family.iter().all(|key| failed.contains(**key)) {
                status.insert(source, ServiceState::Unavailable);
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
    jellyfin: Vec<PlayedArtist>,
    library_albums: Vec<super::library::LibraryAlbum>,
}

/// What every section of one build reads.
struct Ctx<'c> {
    user_id: &'c str,
    user: &'c UserMusic,
    primary: ListenSource,
    seeds: &'c [ArtistRow],
    shared: &'c Shared,
    library_groups: &'c HashSet<String>,
    lb_user: Option<&'c str>,
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
pub(in crate::reads::discover) fn daily_shuffler(today: &str, parts: &[&str]) -> Shuffler {
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
            listening: self.listening,
            db: self.queue_db,
            settings,
            ask: self.ask.clone(),
        }
    }

    /// Whether Last.fm stands in for ListenBrainz popularity: only while
    /// ListenBrainz refuses popularity reads, and only when the user has
    /// Last.fm.
    fn lastfm_for_popularity(&self, user: &UserMusic) -> bool {
        self.listening.policy_for(user).lastfm_stands_in()
    }

    /// Build the page's `wanted` sections, in page order.
    pub async fn build(&self, user_id: &str, wanted: &[&'static SectionDef]) -> Built {
        let policy = self.listening.policy(user_id).await;
        let primary = policy.listening();
        let user = policy.user().clone();
        let lb_user = user.listenbrainz.clone();
        let lfm_user = user.lastfm_username.clone().filter(|_| user.lastfm);
        let needs = |input: Input| wanted.iter().any(|def| def.inputs.contains(&input));
        let seeds = if needs(Input::Seeds) {
            let mut seed_settings = self.settings.queue.clone();
            seed_settings.seed_artists = SEEDS;
            seed_artists(&self.queue_ctx(&seed_settings), user_id, &user, primary).await
        } else {
            Vec::new()
        };

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
                &needs,
            )
            .await;
        if needs(Input::Genres) {
            bounded(
                "learn genres",
                self.learn_genres(user_id, primary, &seeds, &shared),
            )
            .await;
        }
        let library_groups: HashSet<String> = shared
            .library_albums
            .iter()
            .map(|album| album.release_group_mbid.clone())
            .collect();
        let ctx = Ctx {
            user_id,
            user: &user,
            primary,
            seeds: &seeds,
            shared: &shared,
            library_groups: &library_groups,
            lb_user: lb_user.as_deref(),
        };

        let independent = join_all(
            wanted
                .iter()
                .filter(|def| def.dedupe != Dedupe::Artists)
                .map(|def| async {
                    let name = def.key.name();
                    (def.key, bounded(&name, self.section(def.key, &ctx)).await)
                }),
        )
        .await;
        let mut built: HashMap<SectionKey, SectionContent> = independent
            .into_iter()
            .filter_map(|(key, content)| Some((key, content?)))
            .collect();
        let mut seen_artists: HashSet<String> = HashSet::new();
        for def in wanted.iter().filter(|def| def.dedupe == Dedupe::Artists) {
            let name = def.key.name();
            let content =
                bounded(&name, self.artist_section(def.key, &ctx, &mut seen_artists)).await;
            if let Some(content) = content {
                built.insert(def.key, content);
            }
        }

        let mut page = super::empty_page();
        page.sections = wanted
            .iter()
            .filter_map(|def| shelves::section(Page::Discover, def.key, built.remove(&def.key)?))
            .collect();
        shelves::dedupe_albums(&mut page.sections);
        page.service_prompts = shelves::service_prompts(
            lb_user.is_some(),
            user.jellyfin,
            self.settings.download_client,
            user.lastfm,
        );
        Built {
            page,
            degraded: tracker.degraded(),
        }
    }

    /// One section that keeps every artist it finds.
    async fn section(&self, key: SectionKey, ctx: &Ctx<'_>) -> Option<SectionContent> {
        let (user_id, shared) = (ctx.user_id, ctx.shared);
        match key {
            SectionKey::TopPicks => self
                .top_picks(user_id, ctx.user, ctx.primary, ctx.seeds, shared)
                .await
                .map(|picks| SectionContent::Picks { picks }),
            SectionKey::ListenersLikeYou => match ctx.lb_user {
                Some(username) => one(self.listeners_like_you(user_id, username).await),
                None => None,
            },
            SectionKey::WeeklyExploration => match ctx.lb_user {
                Some(username) => self
                    .weekly_exploration(user_id, username)
                    .await
                    .map(|weekly| SectionContent::Weekly { weekly }),
                None => None,
            },
            SectionKey::DailyMixes => Some(SectionContent::Shelves {
                shelves: self
                    .daily_mixes(
                        user_id,
                        ctx.user,
                        ctx.primary,
                        ctx.seeds,
                        shared,
                        ctx.library_groups,
                    )
                    .await,
            }),
            SectionKey::RadioSections => Some(SectionContent::Shelves {
                shelves: self
                    .radio_sections(
                        user_id,
                        ctx.user,
                        ctx.primary,
                        ctx.seeds,
                        ctx.library_groups,
                    )
                    .await,
            }),
            SectionKey::FreshReleases => one(fresh_releases(&shared.fresh)),
            SectionKey::NewFromFollowed => one(self.new_from_followed(user_id).await),
            SectionKey::MissingEssentials => one(self.missing_essentials(user_id, shared).await),
            SectionKey::Rediscover => one(rediscover(&shared.jellyfin, self.now)),
            SectionKey::Anniversaries => one(self.anniversaries().await),
            SectionKey::LastfmRecentScrobbles => one(self
                .lastfm_album_shelf(&shared.lastfm_recent, 30, "Recently Scrobbled", true)
                .await),
            SectionKey::LastfmWeeklyAlbumChart => one(self
                .lastfm_album_shelf(
                    &shared.lastfm_weekly_albums,
                    20,
                    "Your Top Albums This Week",
                    false,
                )
                .await),
            SectionKey::UnexploredGenres => one(self.unexplored_genres(&shared.similar).await),
            SectionKey::GenreList => one(genre_list(shared.genres.as_deref())),
            _ => None,
        }
    }

    /// One artist section: it skips artists `seen` holds and adds the
    /// ones it shows, so an artist appears once on the page.
    async fn artist_section(
        &self,
        key: SectionKey,
        ctx: &Ctx<'_>,
        seen: &mut HashSet<String>,
    ) -> Option<SectionContent> {
        let shared = ctx.shared;
        match key {
            SectionKey::BecauseYouListenTo => Some(SectionContent::Because {
                entries: because_sections(ctx.seeds, &shared.similar, seen, ctx.primary),
            }),
            SectionKey::ArtistsYouMightLike => {
                one(artists_you_might_like(&shared.similar, seen, ctx.primary))
            }
            SectionKey::PopularInYourGenres => one(self
                .popular_in_genres(ctx.user_id, ctx.primary, shared, seen)
                .await),
            SectionKey::GloballyTrending => {
                let source = if ctx.primary == ListenSource::Lastfm && ctx.user.lastfm {
                    ShelfSource::Lastfm
                } else {
                    ShelfSource::Listenbrainz
                };
                one(artist_shelf(
                    &shared.trending_artists,
                    seen,
                    "Globally Trending",
                    Some(source),
                ))
            }
            SectionKey::LastfmWeeklyArtistChart => one(artist_shelf(
                &shared.lastfm_weekly_artists,
                seen,
                "Your Weekly Top Artists",
                Some(ShelfSource::Lastfm),
            )),
            _ => None,
        }
    }

    fn library_pool(&self) -> &sqlx::SqlitePool {
        self.library.pool()
    }

    fn memo_key<'k>(&'k self, user_id: &'k str, primary: ListenSource) -> MemoKey<'k> {
        MemoKey {
            user_id,
            variant: self.variant,
            generation: self.generation,
            listening: primary.as_str(),
            today: &self.today,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn first_round(
        &self,
        tracker: &Tracker,
        user_id: &str,
        user: &UserMusic,
        primary: ListenSource,
        seeds: &[ArtistRow],
        lb_user: Option<&str>,
        lfm_user: Option<&str>,
        needs: &(dyn Fn(Input) -> bool + Sync),
    ) -> Shared {
        let listening = self.listening;
        let ask = &self.ask;
        let policy = listening.policy_for(user);
        // ListenBrainz's similar-artist read is popularity ranked, so it goes
        // empty in a popularity outage; Last.fm stands in then, and always
        // for users who chose Last.fm.
        let lastfm_similar = policy.popularity() == ListenSource::Lastfm;
        const SIMILAR_KEYS: [&str; SEEDS] = ["similar_0", "similar_1", "similar_2"];
        let similar_seeds = if needs(Input::Similar) { SEEDS } else { 0 };
        let similar = join_all(seeds.iter().take(similar_seeds).enumerate().map(
            |(index, seed)| {
                let key = SIMILAR_KEYS[index];
                async move {
                    let Some(mbid) = seed.mbid.as_deref() else {
                        return Vec::new();
                    };
                    let read = async {
                        if lastfm_similar {
                            listening.lastfm_similar(user_id, seed, 20, ask).await
                        } else {
                            listening.similar_artists(user_id, mbid, 20, ask).await
                        }
                    };
                    tracker.run(key, read).await.unwrap_or_default()
                }
            },
        ));
        let trending = async {
            if !needs(Input::Trending) {
                return Vec::new();
            }
            if policy.sitewide() == ListenSource::Lastfm {
                tracker
                    .run(
                        "lfm_global_top",
                        listening.lastfm_chart_artists(user_id, 20, ask),
                    )
                    .await
            } else {
                tracker
                    .run(
                        "lb_trending",
                        listening.sitewide_artists(StatsRange::ThisWeek, 20, 0, ask),
                    )
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
                    if !needs(Input::LastfmWeeklyArtists) {
                        return Vec::new();
                    }
                    tracker
                        .run(
                            "lfm_weekly_artists",
                            listening.lastfm_weekly_artists(user_id, username, ask),
                        )
                        .await
                        .unwrap_or_default()
                },
                async {
                    if !needs(Input::LastfmWeeklyAlbums) {
                        return Vec::new();
                    }
                    tracker
                        .run(
                            "lfm_weekly_albums",
                            listening.lastfm_weekly_albums(user_id, username, ask),
                        )
                        .await
                        .unwrap_or_default()
                },
                async {
                    if !needs(Input::LastfmRecent) {
                        return Vec::new();
                    }
                    tracker
                        .run(
                            "lfm_recent",
                            listening.lastfm_recent_albums(user_id, username, 20, ask),
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
                    if !needs(Input::Fresh) {
                        return Vec::new();
                    }
                    tracker
                        .run("lb_fresh", listening.fresh_releases(username, ask))
                        .await
                        .unwrap_or_default()
                },
                async {
                    if !needs(Input::Genres) {
                        return None;
                    }
                    tracker
                        .run("lb_genres", listening.genres(username, ask))
                        .await
                },
            )
        };
        let lastfm_genre_artists = async {
            match lfm_user {
                Some(username)
                    if primary == ListenSource::Lastfm && needs(Input::LastfmGenreArtists) =>
                {
                    tracker
                        .run(
                            "lfm_user_top_artists_for_genres",
                            listening.lastfm_user_artists(user_id, username, 5, ask),
                        )
                        .await
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            }
        };
        let jellyfin = async {
            if !user.jellyfin || !needs(Input::Jellyfin) {
                return Vec::new();
            }
            tracker
                .run("jf_most_played", listening.jellyfin_plays(user_id, 50, ask))
                .await
                .unwrap_or_default()
        };
        let library = async {
            if !needs(Input::LibraryAlbums) {
                return Vec::new();
            }
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
                self.listening
                    .artist_albums(user_id, artist_mbid, 10, &self.ask)
                    .await,
            )
        }))
        .await;
        let mut missing: Vec<(i64, AlbumRef)> = Vec::new();
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
            SectionKind::Albums,
            missing
                .into_iter()
                .take(SHELF_SIZE)
                .map(|(_, album)| SectionItem::Album(album))
                .collect(),
            Some(ShelfSource::Library),
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
            if item.cover_url.is_none() && !row.image_url.is_empty() {
                item.cover_url = Some(row.image_url.clone());
            }
            items.push(SectionItem::Album(item));
        }
        items.truncate(SHELF_SIZE);
        shelf(title, SectionKind::Albums, items, Some(ShelfSource::Lastfm))
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
        primary: ListenSource,
        seeds: &[ArtistRow],
        shared: &Shared,
        library_groups: &HashSet<String>,
    ) -> Vec<ChartSection> {
        let key = self.memo_key(user_id, primary);
        if let Some(mixes) = self.memo.mixes(&key).await {
            return mixes;
        }
        let mixes = self
            .build_daily_mixes(user_id, user, primary, seeds, shared, library_groups)
            .await;
        self.memo.store_mixes(&key, mixes.clone()).await;
        mixes
    }

    async fn build_daily_mixes(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: ListenSource,
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
                .filter(|mbid| droppedneedle_providers::musicbrainz::is_valid_mbid(mbid))
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
        primary: ListenSource,
        index: usize,
        genre: &str,
        mut artists: Vec<String>,
        library_groups: &HashSet<String>,
    ) -> Option<ChartSection> {
        let label = title_case(genre);
        daily_shuffler(&self.today, &["daily-mix", user_id, genre]).shuffle(&mut artists);
        artists.truncate(3);
        let names = if self.listening.popularity_down() {
            vec![None; artists.len()]
        } else {
            join_all(artists.iter().map(|mbid| async move {
                self.listening
                    .artist_albums(user_id, mbid, 1, &self.ask)
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
        let fresh: Vec<AlbumRef> = pools
            .into_iter()
            .flatten()
            .filter(|item| seen.insert(item.musicbrainz_id.to_lowercase()))
            .map(|item| {
                album(
                    Some(&item.musicbrainz_id),
                    &item.title,
                    Some(&item.artist_name),
                    Some(&item.artist_id),
                    None,
                )
            })
            .collect();
        let owned = or_empty(
            "genre albums",
            self.library.albums_by_genre(genre, 20).await,
        );
        let familiar: Vec<AlbumRef> = owned
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
            SectionKind::Albums,
            items,
            Some(primary.into()),
        )
    }

    /// One radio shelf per seed artist: albums from similar artists, picked
    /// round-robin with at most two per artist.
    async fn radio_sections(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: ListenSource,
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
                            Some(&item.musicbrainz_id),
                            &item.title,
                            Some(&item.artist_name),
                            Some(&item.artist_id),
                            None,
                        ))
                    })
                    .collect();
                let mut section = ChartSection {
                    title: format!("Radio: {}", seed.name),
                    kind: SectionKind::Albums,
                    items,
                    source: Some(primary.into()),
                    fallback_message: None,
                    connect_service: None,
                    radio_seed_type: Some(RadioSeedKind::Artist),
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
        primary: ListenSource,
        seeds: &[ArtistRow],
        shared: &Shared,
    ) -> Option<TopPicksSection> {
        let key = self.memo_key(user_id, primary);
        if let Some(picks) = self.memo.picks(&key).await {
            return picks;
        }
        let (picks, degraded) = self
            .build_top_picks(user_id, user, primary, seeds, shared)
            .await;
        self.memo.store_picks(&key, picks.clone(), degraded).await;
        picks
    }

    async fn build_top_picks(
        &self,
        user_id: &str,
        user: &UserMusic,
        primary: ListenSource,
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
                self.listening
                    .artist_albums(user_id, mbid, 2, &self.ask)
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
            self.listening
                .sitewide_albums(StatsRange::ThisWeek, 100, 0, &self.ask),
        )
        .await
        {
            Ok(Ok(rows)) => rows.into_iter().filter_map(ListenedAlbum::ranked).collect(),
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
        let owned = owned_album_ids(self.library_pool(), &ids)
            .await
            .unwrap_or_default();
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
            source: Some(primary.into()),
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
        let albums = join_all(rows.iter().map(|artist| {
            self.listening
                .lastfm_artist_albums(user_id, artist, 3, &self.ask)
        }))
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
            .listening
            .similar_users(username, &self.ask)
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
            self.listening
                .user_albums(name, StatsRange::ThisMonth, 15, 0, &self.ask)
        }))
        .await;
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        'lists: for list in lists.into_iter().flatten() {
            for row in list.into_iter().filter_map(ListenedAlbum::ranked) {
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
            SectionKind::Albums,
            items,
            Some(ShelfSource::Listenbrainz),
        )
    }

    /// The user's ListenBrainz weekly exploration playlist.
    async fn weekly_exploration(&self, user_id: &str, username: &str) -> Option<WeeklyExploration> {
        weekly_exploration(self.listening, &self.ask, user_id, username).await
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
        shelf(
            "Milestone Anniversaries",
            SectionKind::Albums,
            items,
            Some(ShelfSource::Library),
        )
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
            SectionKind::Albums,
            items,
            Some(ShelfSource::Library),
        )
    }

    /// Artists popular in the user's top genres: MusicBrainz tag search
    /// over ListenBrainz genres, or Last.fm tag charts for Last.fm users.
    async fn popular_in_genres(
        &self,
        user_id: &str,
        primary: ListenSource,
        shared: &Shared,
        seen: &mut HashSet<String>,
    ) -> Option<ChartSection> {
        if primary == ListenSource::Lastfm {
            return self.popular_in_genres_lastfm(user_id, shared, seen).await;
        }
        let genres = listenbrainz_top_genres(shared);
        if genres.is_empty() {
            return None;
        }
        let found = join_all(
            genres
                .iter()
                .map(|genre| self.listening.tag_artists(genre, 10, 0, &self.ask)),
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
            SectionKind::Artists,
            items,
            Some(ShelfSource::Musicbrainz),
        )
    }

    /// The Last.fm tags of the first few of `artists`, in order.
    async fn lastfm_tags<'r>(
        &self,
        user_id: &str,
        artists: &'r [ArtistRow],
    ) -> Vec<(&'r ArtistRow, Vec<String>)> {
        let top = &artists[..artists.len().min(GENRE_ARTISTS)];
        let tags = join_all(top.iter().map(|artist| self.artist_tags(user_id, artist))).await;
        top.iter()
            .zip(tags)
            .map(|(artist, found)| (artist, or_empty("lastfm artist tags", found)))
            .collect()
    }

    /// Learn genres for the artists the tag reads of Popular In Your
    /// Genres name, before the sections that score genre overlap run.
    /// The reads are that section's own and cached, so a page showing it
    /// makes no extra call. A ListenBrainz user's top genres each name
    /// the artists MusicBrainz tags with them; a Last.fm user's top
    /// artists (the seeds when that read was not made) carry their own
    /// tags, and the tag charts of the genres drawn from them name more.
    async fn learn_genres(
        &self,
        user_id: &str,
        primary: ListenSource,
        seeds: &[ArtistRow],
        shared: &Shared,
    ) {
        let mut learned: Vec<(String, String)> = Vec::new();
        let mut add = |mbid: Option<&str>, genre: &str| {
            if let Some(mbid) = normalize(mbid)
                && droppedneedle_providers::musicbrainz::is_valid_mbid(&mbid)
                && mbid != VARIOUS_ARTISTS_MBID
                && !genre.trim().is_empty()
            {
                learned.push((mbid, genre.trim().to_owned()));
            }
        };
        if primary == ListenSource::Lastfm {
            let artists = if shared.lastfm_genre_artists.is_empty() {
                seeds
            } else {
                &shared.lastfm_genre_artists
            };
            let tags = self.lastfm_tags(user_id, artists).await;
            for (artist, tags) in &tags {
                for tag in tags.iter().take(LEARNED_ARTIST_TAGS) {
                    add(artist.mbid.as_deref(), tag);
                }
            }
            let genres = lastfm_top_genres(&tags);
            let found = join_all(genres.iter().map(|genre| {
                self.listening
                    .lastfm_tag_artists(user_id, genre, 10, &self.ask)
            }))
            .await;
            for (genre, rows) in genres.iter().zip(found) {
                for row in or_empty("lastfm tag artists", rows) {
                    add(row.mbid.as_deref(), genre);
                }
            }
        } else {
            let genres = listenbrainz_top_genres(shared);
            let found = join_all(
                genres
                    .iter()
                    .map(|genre| self.listening.tag_artists(genre, TAG_PAGE, 0, &self.ask)),
            )
            .await;
            for (genre, rows) in genres.iter().zip(found) {
                for row in or_empty("tag artists", rows) {
                    add(row.mbid.as_deref(), genre);
                }
            }
        }
        if learned.is_empty() {
            return;
        }
        let at = (self.now * 1000.0) as i64;
        self.queue_db
            .learn_genres(learned, at, at - GENRES_KEPT_MS)
            .await;
    }

    /// An artist's Last.fm tags, most used first.
    async fn artist_tags(
        &self,
        user_id: &str,
        artist: &ArtistRow,
    ) -> crate::reads::discover::listening::ListeningResult<Vec<String>> {
        let facts = self
            .listening
            .lastfm_artist(user_id, &artist.name, artist.mbid.as_deref(), &self.ask)
            .await?;
        Ok(facts
            .map(|facts| facts.tags)
            .unwrap_or_default()
            .into_iter()
            .filter(|name| !name.trim().is_empty())
            .collect())
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
        let tags = self.lastfm_tags(user_id, top).await;
        let genres = lastfm_top_genres(&tags);
        if genres.is_empty() {
            return None;
        }
        let found = join_all(genres.iter().map(|genre| {
            self.listening
                .lastfm_tag_artists(user_id, genre, 10, &self.ask)
        }))
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
        shelf(
            "Popular In Your Genres",
            SectionKind::Artists,
            items,
            Some(ShelfSource::Lastfm),
        )
    }

    /// Genres around the user's similar artists that the library barely
    /// has, else genres the library holds only one artist of.
    async fn unexplored_genres(&self, similar: &[Vec<ScoredArtist>]) -> Option<ChartSection> {
        let candidates: Vec<String> = similar
            .iter()
            .flatten()
            .filter_map(|scored| scored.artist.mbid.clone())
            .collect();
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
        shelf("Genres to Explore", SectionKind::Genres, items, None)
    }
}

/// A ListenBrainz user's top genres, most listened first.
fn listenbrainz_top_genres(shared: &Shared) -> Vec<String> {
    shared
        .genres
        .iter()
        .flatten()
        .take(TOP_GENRES)
        .map(|(name, _)| name.clone())
        .collect()
}

/// A Last.fm user's top genres: the first two tags of each top artist,
/// in order, once each.
fn lastfm_top_genres(tags: &[(&ArtistRow, Vec<String>)]) -> Vec<String> {
    let mut genres: Vec<String> = Vec::new();
    for (_, tags) in tags {
        for tag in tags.iter().take(2) {
            if !genres.iter().any(|g| g.eq_ignore_ascii_case(tag)) {
                genres.push(tag.clone());
                if genres.len() >= TOP_GENRES {
                    return genres;
                }
            }
        }
    }
    genres
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
    primary: ListenSource,
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
            SectionKind::Artists,
            items,
            Some(primary.into()),
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
    primary: ListenSource,
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
        SectionKind::Artists,
        items,
        Some(primary.into()),
    )
}

/// An artist chart as a shelf, skipping artists already on the page.
fn artist_shelf(
    rows: &[ArtistRow],
    seen: &mut HashSet<String>,
    title: &str,
    source: Option<ShelfSource>,
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
    shelf(title, SectionKind::Artists, items, source)
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
        SectionKind::Albums,
        items,
        Some(ShelfSource::Listenbrainz),
    )
}

/// Jellyfin artists played a lot but not in the last three months.
fn rediscover(plays: &[PlayedArtist], now: f64) -> Option<ChartSection> {
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
            crate::reads::discover::models::ArtistRef {
                name: row.name.clone(),
                musicbrainz_id: row.mbid.clone(),
                local_id: None,
                image_url,
                listen_count: Some(row.play_count),
                in_library: false,
            },
        ));
        if items.len() >= SHELF_SIZE {
            break;
        }
    }
    shelf(
        "Rediscover",
        SectionKind::Artists,
        items,
        Some(ShelfSource::Jellyfin),
    )
}

/// The user's ListenBrainz weekly exploration playlist, with covers from
/// each track's release group where it can be found in time. Home shows
/// the same playlist.
pub async fn weekly_exploration(
    listening: &ListeningData,
    ask: &Ask,
    user_id: &str,
    username: &str,
) -> Option<WeeklyExploration> {
    let playlist = match listening.weekly_playlist(user_id, username, ask).await {
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
    let by_recording = listening
        .recording_groups(user_id, &recordings, ask)
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
            let group = listening
                .release_group_of(release, ask)
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
                (Some(group), _) => Some(format!("/api/v3/covers/release-group/{group}?size=250")),
                (None, Some(release)) => Some(format!("/api/v3/covers/release/{release}?size=250")),
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

/// Browse by Genre: the user's ListenBrainz genres, else a default list.
pub fn genre_list(genres: Option<&[(String, i64)]>) -> Option<ChartSection> {
    match genres.filter(|g| !g.is_empty()) {
        Some(genres) => shelf(
            "Browse by Genre",
            SectionKind::Genres,
            genres
                .iter()
                .take(20)
                .map(|(name, count)| genre(name, Some(*count), None))
                .collect(),
            Some(ShelfSource::Listenbrainz),
        ),
        None => shelf(
            "Browse by Genre",
            SectionKind::Genres,
            shelves::DEFAULT_GENRES
                .iter()
                .map(|name| genre(name, None, None))
                .collect(),
            Some(ShelfSource::Library),
        ),
    }
}

/// Owned release groups among `ids`, lowercased, mapped to local ids.
/// `None` when the library could not be read.
async fn owned_album_ids(
    pool: &sqlx::SqlitePool,
    ids: &[String],
) -> Option<HashMap<String, String>> {
    let mut owned = HashMap::new();
    for chunk in ids.chunks(200) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match ownership::owned_albums(pool, &refs).await {
            Ok(found) => owned.extend(found),
            Err(error) => {
                tracing::warn!(%error, "album ownership unreadable; marks left as built");
                return None;
            }
        }
    }
    Some(owned)
}

/// Owned artists among `ids`, lowercased, mapped to local ids. `None`
/// when the library could not be read.
async fn owned_artist_ids(
    pool: &sqlx::SqlitePool,
    ids: &[String],
) -> Option<HashMap<String, String>> {
    let mut owned = HashMap::new();
    for chunk in ids.chunks(200) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match ownership::owned_artists(pool, &refs).await {
            Ok(found) => owned.extend(found),
            Err(error) => {
                tracing::warn!(%error, "artist ownership unreadable; marks left as built");
                return None;
            }
        }
    }
    Some(owned)
}

/// Mark a row with an MBID as the library has it now: owned with its
/// local id, or not owned. A row without an MBID keeps its marks.
fn mark(
    owned: Option<&HashMap<String, String>>,
    mbid: Option<&str>,
    in_library: &mut bool,
    local_id: &mut Option<String>,
) {
    let (Some(owned), Some(mbid)) = (owned, mbid) else {
        return;
    };
    match owned.get(&mbid.to_lowercase()) {
        Some(local) => {
            *in_library = true;
            local_id.get_or_insert_with(|| local.clone());
        }
        None => {
            *in_library = false;
            *local_id = None;
        }
    }
}

/// Mark which albums and artists on the page the library holds now, and
/// drop owned albums from the sections that hide them. Run on every
/// serve.
pub async fn mark_ownership(pool: &sqlx::SqlitePool, page: &mut DiscoverResponse) {
    let mut album_ids: Vec<String> = Vec::new();
    let mut artist_ids: Vec<String> = Vec::new();
    for section in &mut page.sections {
        for row in section.content.rows_mut() {
            match row {
                RowMut::Album(row) => album_ids.extend(row.musicbrainz_id.clone()),
                RowMut::Artist(row) => artist_ids.extend(row.musicbrainz_id.clone()),
            }
        }
    }
    let owned_albums = owned_album_ids(pool, &album_ids).await;
    let owned_artists = owned_artist_ids(pool, &artist_ids).await;
    for section in &mut page.sections {
        for row in section.content.rows_mut() {
            let (owned, row_mbid, in_library, local_id) = match row {
                RowMut::Album(row) => (
                    owned_albums.as_ref(),
                    row.musicbrainz_id.as_deref(),
                    &mut row.in_library,
                    &mut row.local_id,
                ),
                RowMut::Artist(row) => (
                    owned_artists.as_ref(),
                    row.musicbrainz_id.as_deref(),
                    &mut row.in_library,
                    &mut row.local_id,
                ),
            };
            mark(owned, row_mbid, in_library, local_id);
        }
        if sections::find(Page::Discover, section.key).is_some_and(|def| def.hides_owned) {
            for shelf in section.content.shelves_mut() {
                shelf
                    .items
                    .retain(|item| !matches!(item, SectionItem::Album(row) if row.in_library));
            }
        }
    }
    page.sections.retain(|section| !section.content.is_empty());
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
