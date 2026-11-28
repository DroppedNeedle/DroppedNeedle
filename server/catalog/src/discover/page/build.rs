//! One discover page build for one user.
//!
//! A build makes the sections the user has switched on, from the section
//! list ([`sections`]). It runs in two rounds. The first makes the reads
//! several sections share (seed artists, the user's genres, the library's
//! identified albums), only those a wanted section needs, every read
//! bounded to 25 seconds so one slow service only costs its own sections.
//! The second builds the sections in parallel under the same bound; the
//! Daily Mixes only read the mixes their job kept. Each section builds
//! under an ask of its own and says how it came out ([`Outcome`]): a read
//! it needed failing, its own or a shared one, or its bound running out
//! makes it failed. Each section about the person leaves out what they
//! hid ([`Feedback`]), and album sections are then deduplicated by rank.
//! Ownership is marked when the page is served ([`mark_ownership`]), so a
//! library change never makes a page wrong.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use futures_util::future::join_all;

use super::library::LibraryReads;
use super::shelves::{self, RowMut, album, genre, one, shelf};
use droppedneedle_kernel::db::Db;
use droppedneedle_library::sound::SoundSearch;

use crate::discover::engine::sources::{gaps, shelves as shelves_source};
use crate::discover::feedback::{self, Feedback};
use crate::discover::listening::{
    ArtistRow, Ask, ListenSource, ListeningData, TAG_PAGE, UserMusic,
};
use crate::discover::models::{
    ChartSection, DiscoverResponse, RadioSeedKind, SectionContent, SectionItem, SectionKey,
    SectionKind, ServiceState, ShelfSource, WeeklyExploration, WeeklyTrack,
};
use crate::discover::queue::QueueSettings;
use crate::discover::queue::build::{
    AlbumCard, BuildContext, VARIOUS_ARTISTS_MBID, lastfm_pools, normalize, or_empty,
    similar_artist_pools,
};
use crate::discover::queue::seeds::{SEEDS, links_listening, weighted_seeds};
use crate::discover::queue::select::{MAX_PER_ARTIST, Shuffler, round_robin};
use crate::discover::queue::store::QueueDb;
use crate::discover::sections::{self, Input, Page, Requires, SectionDef};
use crate::discover::{mixes, new_releases, ownership};

/// Bound on any one upstream read in a build.
const TASK_TIMEOUT: Duration = Duration::from_secs(25);
/// Bound on resolving weekly playlist covers.
const RESOLVE_BUDGET: Duration = Duration::from_secs(15);
/// Bound on one radio shelf.
const RADIO_BUDGET: Duration = Duration::from_secs(18);
/// Learned genres: the user's top genres whose artists are read.
const TOP_GENRES: usize = 3;
/// Learned genres: a Last.fm user's seed artists whose tags are read.
const GENRE_ARTISTS: usize = 5;
/// Tags of a Last.fm top artist kept as its genres.
const LEARNED_ARTIST_TAGS: usize = 5;
/// How long a learned artist genre is kept after a build last saw it.
const GENRES_KEPT_MS: i64 = 30 * 86_400_000;

/// The settings one build reads, fresh each time.
#[derive(Debug, Clone)]
pub struct PageSettings {
    /// The queue settings (lookup budgets, ignore ledger).
    pub queue: QueueSettings,
    /// Whether some download client is set up (connect-a-service cards).
    pub download_client: bool,
    /// Whether the server has Last.fm switched on (connect-a-service cards).
    pub lastfm_on: bool,
}

/// What one build reads from.
pub struct Builder<'a> {
    /// Provider reads.
    pub listening: &'a ListeningData,
    /// The build's background priority and its cap on provider calls in
    /// flight, shared by every read the build makes.
    pub ask: Ask,
    /// The queue tables (remembered release groups).
    pub queue_db: &'a QueueDb,
    /// What the person told Discover, read once for the build.
    pub feedback: &'a Feedback,
    /// Library reads.
    pub library: &'a LibraryReads,
    /// The search over how the library's tracks sound, for the bench.
    pub sound: &'a SoundSearch,
    /// Settings for this build.
    pub settings: PageSettings,
    /// Today's date (`YYYY-MM-DD`, UTC).
    pub today: String,
    /// Now, unix seconds.
    pub now: f64,
}

/// A build's page plus the services whose every read failed.
pub struct Built {
    /// The page.
    pub page: DiscoverResponse,
    /// Each listening service all of whose reads failed.
    pub degraded: HashMap<ListenSource, ServiceState>,
    /// Whether a read failed or a shelf ran out of time, so some shelf
    /// may be missing or short.
    pub incomplete: bool,
    /// Seed artists the build grew from.
    pub seeds_used: usize,
    /// Seed artists the person has, the ones used included, at most 30.
    pub seeds_found: usize,
    /// Whether the user's ListenBrainz sections could fill: linked, with
    /// ListenBrainz reads on.
    pub listenbrainz: bool,
    /// Whether the user's Last.fm sections could fill.
    pub lastfm: bool,
    /// How each section on the page came out, in page order. The weekly
    /// set and the playlist tool, served by routes of their own, are left
    /// out.
    pub outcomes: Vec<(SectionKey, Outcome)>,
}

/// How one section of a build came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It has rows, and every read it made answered.
    Built,
    /// Every read answered, with nothing to show.
    Empty,
    /// A read it needed failed or it ran out of time; rows it has are
    /// partial. The service is the one that failed, when known.
    Failed(Option<ListenSource>),
    /// It needs a service the person has not linked.
    NeedsLink(ListenSource),
    /// It is built from the person's tastes, which are not worked out yet.
    NeedsTaste,
    /// It was built from ListenBrainz rows kept since this time (unix
    /// seconds), whose daily read has missed since.
    Dated(i64),
}

/// Failed reads of the first round, for the degraded banner and for the
/// sections that read them.
#[derive(Default)]
struct Tracker {
    attempted: Mutex<Vec<&'static str>>,
    failed: Mutex<HashSet<&'static str>>,
    failed_inputs: Mutex<Vec<(Input, Option<ListenSource>)>>,
}

/// The service behind a first-round read, from its key.
fn service_of(key: &str) -> Option<ListenSource> {
    if key.starts_with("lb_") {
        Some(ListenSource::Listenbrainz)
    } else if key.starts_with("lfm_") {
        Some(ListenSource::Lastfm)
    } else {
        None
    }
}

impl Tracker {
    /// Run one read of `input` under the task bound. `None` when it failed.
    async fn run<T, E: std::fmt::Display>(
        &self,
        input: Input,
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
        if let Ok(mut inputs) = self.failed_inputs.lock() {
            inputs.push((input, service_of(key)));
        }
        None
    }

    /// Whether any read failed.
    fn any_failed(&self) -> bool {
        self.failed.lock().map(|f| !f.is_empty()).unwrap_or(true)
    }

    /// `Some` when a read of one of `inputs` failed, with the service
    /// behind it when one is known.
    fn failed_among(&self, inputs: &[Input]) -> Option<Option<ListenSource>> {
        let failed = self
            .failed_inputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut hits = failed.iter().filter(|(input, _)| inputs.contains(input));
        let (_, first) = hits.next()?;
        Some(first.or_else(|| hits.find_map(|(_, source)| *source)))
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

/// Run a shelf builder under the task bound; a timeout empties it and
/// is noted on `ask`, so the page is built again soon.
async fn bounded<T: Default>(ask: &Ask, what: &str, build: impl Future<Output = T>) -> T {
    bounded_by(ask, what, TASK_TIMEOUT, build).await
}

/// [`bounded`] with a bound of its own.
async fn bounded_by<T: Default>(
    ask: &Ask,
    what: &str,
    bound: Duration,
    build: impl Future<Output = T>,
) -> T {
    match tokio::time::timeout(bound, build).await {
        Ok(value) => value,
        Err(_) => {
            tracing::warn!(shelf = what, "discover shelf timed out; leaving it empty");
            ask.note_failure();
            T::default()
        }
    }
}

/// Whether a section's rows are on the page: the weekly set and the
/// playlist tool are served by routes of their own.
pub(super) fn on_page(key: SectionKey) -> bool {
    !matches!(key, SectionKey::WeeklySet | SectionKey::PlaylistDiscovery)
}

/// The listening service a section needs, when it names one.
fn required_service(def: &SectionDef) -> Option<ListenSource> {
    match def.requires {
        Some(Requires::Listenbrainz) => Some(ListenSource::Listenbrainz),
        Some(Requires::Lastfm) => Some(ListenSource::Lastfm),
        Some(Requires::Library | Requires::SoundAnalysis) | None => None,
    }
}

/// What the first round fetched.
#[derive(Default)]
struct Shared {
    genres: Option<Vec<(String, i64)>>,
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

/// A shuffler fixed for one day and one purpose, so a rebuild the same
/// day reproduces the same shelves instead of reshuffling them.
pub(in crate::discover) fn daily_shuffler(today: &str, parts: &[&str]) -> Shuffler {
    let digest = md5::compute(format!("{today}:{}", parts.join(":")));
    let mut seed = [0u8; 8];
    seed.copy_from_slice(&digest.0[..8]);
    Shuffler::seeded(u64::from_le_bytes(seed))
}

impl Builder<'_> {
    fn queue_ctx<'b>(&'b self, settings: &'b QueueSettings) -> BuildContext<'b> {
        BuildContext {
            listening: self.listening,
            db: self.queue_db,
            settings,
            ask: self.ask.clone(),
            now_ms: (self.now * 1000.0) as i64,
        }
    }

    /// Whether album popularity comes from Last.fm: the user follows it, or
    /// ListenBrainz refuses popularity reads, and the user can read it.
    fn lastfm_for_popularity(&self, user: &UserMusic) -> bool {
        self.listening.policy_for(user).popularity() == ListenSource::Lastfm
    }

    /// Whether this build blends both services' seed artists for the
    /// user.
    fn blends(&self, user: &UserMusic) -> bool {
        self.listening
            .policy_for(user)
            .blends(self.settings.queue.blend_services)
    }

    /// The badge of shelves grown from the seed artists: both services
    /// when they are blended, the primary for a person who linked one, and
    /// their own use of the app for a person who linked none.
    fn seed_source(&self, user: &UserMusic, primary: ListenSource) -> ShelfSource {
        if self.blends(user) {
            ShelfSource::Blended
        } else if links_listening(user) {
            primary.into()
        } else {
            ShelfSource::Local
        }
    }

    /// Build the page's `wanted` sections, in page order.
    pub async fn build(&self, user_id: &str, wanted: &[&'static SectionDef]) -> Built {
        let policy = self.listening.policy(user_id).await;
        let primary = policy.listening();
        let user = policy.user().clone();
        let lb_user = user.listenbrainz.clone();
        let lfm_user = user.lastfm_username.clone().filter(|_| user.lastfm);
        let needs = |input: Input| wanted.iter().any(|def| def.inputs.contains(&input));
        // The seeds' reads go on an ask of their own, so a failure counts
        // against the sections grown from them.
        let seed_ask = self.ask.watched();
        let (seeds, seeds_found) = if needs(Input::Seeds) {
            let seed_ctx = BuildContext {
                ask: seed_ask.clone(),
                ..self.queue_ctx(&self.settings.queue)
            };
            let found = weighted_seeds(&seed_ctx, user_id, &user, primary).await;
            let count = found.len();
            let used: Vec<ArtistRow> = found
                .into_iter()
                .take(SEEDS)
                .map(|seed| seed.artist)
                .collect();
            (used, count)
        } else {
            (Vec::new(), 0)
        };
        let seeds_failed = seed_ask.met_failure();

        let tracker = Tracker::default();
        let shared = self.first_round(&tracker, lb_user.as_deref(), &needs).await;
        if needs(Input::Genres) {
            bounded(
                &self.ask,
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

        // Each section builds through a builder of its own, made with an
        // ask of its own, which starts out failed when a shared read it
        // needs failed, so it can tell whether everything it stands on
        // answered.
        let input_failure = |def: &SectionDef| {
            tracker
                .failed_among(def.inputs)
                .or_else(|| (seeds_failed && def.inputs.contains(&Input::Seeds)).then_some(None))
        };
        let section_builder = |def: &SectionDef| {
            let ask = self.ask.watched();
            if input_failure(def).is_some() {
                ask.note_failure();
            }
            Builder {
                ask,
                settings: self.settings.clone(),
                today: self.today.clone(),
                ..*self
            }
        };
        let independent = join_all(wanted.iter().map(|def| async {
            let section = section_builder(def);
            let name = def.key.name();
            let content = bounded_by(
                &section.ask,
                &name,
                TASK_TIMEOUT,
                section.section(def.key, &ctx),
            )
            .await;
            (def.key, (content, section.ask.met_failure()))
        }))
        .await;
        let mut built: HashMap<SectionKey, (Option<SectionContent>, bool)> =
            independent.into_iter().collect();

        let mut page = super::empty_page();
        let mut outcomes = Vec::new();
        for def in wanted.iter().filter(|def| on_page(def.key)) {
            let Some((content, failed)) = built.remove(&def.key) else {
                continue;
            };
            let section = content.and_then(|mut content| {
                if feedback::applies(Page::Discover, def.key) {
                    self.feedback.filter_content(&mut content);
                }
                shelves::section(Page::Discover, def.key, content)
            });
            let stale = section.as_ref().and_then(|section| {
                new_releases::stale_since(&section.content, (self.now * 1000.0) as i64)
            });
            let outcome = if failed {
                let source = input_failure(def)
                    .flatten()
                    .or_else(|| required_service(def));
                Some(Outcome::Failed(source))
            } else if let Some(since) = stale {
                Some(Outcome::Dated(since))
            } else if section.is_some() {
                Some(Outcome::Built)
            } else if def.key == SectionKey::DailyMixes && self.mixes_need_taste(user_id).await {
                Some(Outcome::NeedsTaste)
            } else {
                match self.missing_link(def, &user, lb_user.as_deref(), lfm_user.as_deref()) {
                    Some(Some(service)) => Some(Outcome::NeedsLink(service)),
                    // Nothing the person can link fills it: no state.
                    Some(None) => None,
                    None => Some(Outcome::Empty),
                }
            };
            outcomes.extend(outcome.map(|outcome| (def.key, outcome)));
            page.sections.extend(section);
        }
        shelves::dedupe_albums(&mut page.sections);
        page.service_prompts = shelves::service_prompts(
            lb_user.is_some() || !self.listening.listenbrainz_on(),
            user.jellyfin,
            self.settings.download_client,
            self.settings.lastfm_on,
            user.lastfm,
        );
        let sections_failed = outcomes
            .iter()
            .any(|(_, outcome)| matches!(outcome, Outcome::Failed(_)));
        Built {
            page,
            degraded: tracker.degraded(),
            incomplete: tracker.any_failed()
                || self.ask.met_failure()
                || seeds_failed
                || sections_failed,
            seeds_used: seeds.len(),
            seeds_found,
            listenbrainz: lb_user.is_some() && self.listening.listenbrainz_on(),
            lastfm: lfm_user.is_some(),
            outcomes,
        }
    }

    /// Why `def` came back with nothing, when a missing link explains it:
    /// `Some(Some(service))` when linking that service would fill it,
    /// `Some(None)` when the server has what it needs switched off. A
    /// section grown from the person's listening (seed artists, their
    /// genres) needs some listening service linked, though it does not say
    /// so in `requires`.
    fn missing_link(
        &self,
        def: &SectionDef,
        user: &UserMusic,
        lb_user: Option<&str>,
        lfm_user: Option<&str>,
    ) -> Option<Option<ListenSource>> {
        let listenbrainz_on = self.listening.listenbrainz_on();
        let lastfm_on = self.settings.lastfm_on;
        let needs = |on: bool, linked: bool, service| match (on, linked) {
            (false, _) => Some(None),
            (true, false) => Some(Some(service)),
            (true, true) => None,
        };
        let listens = def
            .inputs
            .iter()
            .any(|input| matches!(input, Input::Seeds | Input::Genres));
        match def.requires {
            Some(Requires::Listenbrainz) => needs(
                listenbrainz_on,
                lb_user.is_some(),
                ListenSource::Listenbrainz,
            ),
            Some(Requires::Lastfm) => needs(lastfm_on, lfm_user.is_some(), ListenSource::Lastfm),
            // The bench is built whenever analysis is on and has read a
            // track; otherwise it is simply not on the page.
            Some(Requires::SoundAnalysis) => Some(None),
            _ if listens && lb_user.is_none() && lfm_user.is_none() && !user.jellyfin => {
                if listenbrainz_on {
                    Some(Some(ListenSource::Listenbrainz))
                } else if lastfm_on {
                    Some(Some(ListenSource::Lastfm))
                } else {
                    Some(None)
                }
            }
            _ => None,
        }
    }

    /// One section.
    async fn section(&self, key: SectionKey, ctx: &Ctx<'_>) -> Option<SectionContent> {
        let (user_id, shared) = (ctx.user_id, ctx.shared);
        match key {
            SectionKey::WeeklyExploration => match ctx.lb_user {
                Some(username) => self
                    .weekly_exploration(user_id, username)
                    .await
                    .map(|weekly| SectionContent::Weekly { weekly }),
                None => None,
            },
            SectionKey::DailyMixes => self.daily_mixes(user_id).await,
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
            SectionKey::NewFromYourArtists => self.new_from_your_artists(user_id).await,
            SectionKey::GapsInYourArtists => self.gaps(user_id).await,
            SectionKey::FromYourShelves => self.own_shelves(user_id).await,
            SectionKey::SoundBench => self.sound_bench(user_id).await,
            SectionKey::GenreList => one(genre_list(shared.genres.as_deref())),
            _ => None,
        }
    }

    async fn first_round(
        &self,
        tracker: &Tracker,
        lb_user: Option<&str>,
        needs: &(dyn Fn(Input) -> bool + Sync),
    ) -> Shared {
        let listening = self.listening;
        let ask = &self.ask;
        let genres = async {
            let username = lb_user?;
            if !needs(Input::Genres) {
                return None;
            }
            tracker
                .run(Input::Genres, "lb_genres", listening.genres(username, ask))
                .await
        };
        let library = async {
            if !needs(Input::LibraryAlbums) {
                return Vec::new();
            }
            tracker
                .run(
                    Input::LibraryAlbums,
                    "library_albums",
                    self.library.owned_albums(),
                )
                .await
                .unwrap_or_default()
        };
        let (genres, library_albums) = tokio::join!(genres, library);
        Shared {
            genres,
            library_albums,
        }
    }

    /// Candidate albums from artists similar to `seeds`, one pool per
    /// seed: Last.fm when album popularity comes from it, else ListenBrainz.
    #[allow(clippy::too_many_arguments)]
    async fn similar_pools(
        &self,
        user_id: &str,
        user: &UserMusic,
        seeds: &[ArtistRow],
        excluded: &HashSet<String>,
        similar_limit: usize,
        albums_per: usize,
        ask: &Ask,
    ) -> Vec<Vec<AlbumCard>> {
        let ctx = BuildContext {
            ask: ask.clone(),
            ..self.queue_ctx(&self.settings.queue)
        };
        if self.lastfm_for_popularity(user) {
            lastfm_pools(
                &ctx,
                user_id,
                seeds,
                excluded,
                similar_limit,
                albums_per,
                similar_limit * albums_per,
            )
            .await
        } else {
            similar_artist_pools(&ctx, user_id, seeds, excluded, similar_limit, albums_per).await
        }
    }

    /// The person's Daily Mixes as their job kept them: read from stored
    /// rows only, never built here. A failed read fails the section.
    async fn daily_mixes(&self, user_id: &str) -> Option<SectionContent> {
        match mixes::store::kept(self.queue_db.db(), user_id).await {
            Ok(mixes) => Some(SectionContent::Mixes { mixes }),
            Err(error) => {
                tracing::warn!(%error, "kept mixes unreadable; the section waits");
                self.ask.note_failure();
                None
            }
        }
    }

    /// Whether the Daily Mixes are empty for want of a taste: the person
    /// has no taste group. False when that cannot be told.
    async fn mixes_need_taste(&self, user_id: &str) -> bool {
        match mixes::store::has_groups(self.queue_db.db(), user_id).await {
            Ok(has) => !has,
            Err(error) => {
                tracing::warn!(%error, "taste groups unreadable for the mixes' state");
                false
            }
        }
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
        let source = self.seed_source(user, primary);
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
                        &self.ask,
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
                    source: Some(source),
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

    /// The user's ListenBrainz weekly exploration playlist.
    async fn weekly_exploration(&self, user_id: &str, username: &str) -> Option<WeeklyExploration> {
        weekly_exploration(
            self.listening,
            &self.ask,
            user_id,
            username,
            self.now as i64,
        )
        .await
    }

    /// Gaps in artists you collect, read from the person's pool. A read
    /// that failed fails the section, so the page keeps the one it showed.
    async fn gaps(&self, user_id: &str) -> Option<SectionContent> {
        let now_ms = (self.now * 1000.0) as i64;
        match gaps::section(self.library.db(), user_id, now_ms).await {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(%error, "gaps unreadable; the page keeps its section");
                self.ask.note_failure();
                None
            }
        }
    }

    /// From your shelves, read from the person's own plays and requests. A
    /// read that failed fails the section, so the page keeps the one it
    /// showed.
    async fn own_shelves(&self, user_id: &str) -> Option<SectionContent> {
        let now_ms = (self.now * 1000.0) as i64;
        match shelves_source::section(self.library.db(), user_id, now_ms).await {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(%error, "shelves unreadable; the page keeps its section");
                self.ask.note_failure();
                None
            }
        }
    }

    /// By ear, while sound analysis is on. A read that failed fails the
    /// section, so the page keeps the one it showed.
    async fn sound_bench(&self, user_id: &str) -> Option<SectionContent> {
        let now_ms = (self.now * 1000.0) as i64;
        match super::bench::section(self.sound, user_id, now_ms).await {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(%error, "sound bench unreadable; the page keeps its section");
                self.ask.note_failure();
                None
            }
        }
    }

    /// New from your artists: the newest of the person's list, read from
    /// kept rows. A list that could not be read fails the section, so the
    /// page keeps the one it showed.
    async fn new_from_your_artists(&self, user_id: &str) -> Option<SectionContent> {
        match new_releases::list(self.library.db(), self.listening, user_id).await {
            Ok(listed) => new_releases::sheet(listed),
            Err(error) => {
                tracing::warn!(%error, "new releases unreadable; the page keeps its section");
                self.ask.note_failure();
                None
            }
        }
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

    /// Learn genres for the artists the user's top genres name, before
    /// the sections that score genre overlap (radio) run.
    /// The reads are cached, so a rebuild makes no extra call. A
    /// ListenBrainz user's top genres each name the artists MusicBrainz
    /// tags with them; a Last.fm user's seed artists carry their own tags,
    /// and the tag charts of the genres drawn from them name more.
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
            let tags = self.lastfm_tags(user_id, seeds).await;
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
    ) -> crate::discover::listening::ListeningResult<Vec<String>> {
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

/// How old a weekly exploration playlist may be and still show: a week,
/// since ListenBrainz makes a new one each Monday.
const WEEKLY_MAX_AGE_SECS: i64 = 7 * 86_400;

/// Whether a playlist dated `date` is this week's at `now` (unix seconds).
/// ListenBrainz dates a playlist in RFC 3339, as in
/// `2026-09-14T00:21:44.054723+00:00` (a saved page, 2026-11-22); a date
/// that does not parse is not this week's.
fn this_weeks(date: &str, now: i64) -> bool {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::parse(date.trim(), &Rfc3339)
        .is_ok_and(|made| now - made.unix_timestamp() <= WEEKLY_MAX_AGE_SECS)
}

/// The user's ListenBrainz weekly exploration playlist, with covers from
/// each track's release group where it can be found in time, while it is
/// this week's at `now` (unix seconds). Home shows the same playlist.
pub async fn weekly_exploration(
    listening: &ListeningData,
    ask: &Ask,
    user_id: &str,
    username: &str,
    now: i64,
) -> Option<WeeklyExploration> {
    let playlist = match listening.weekly_playlist(user_id, username, ask).await {
        Ok(Some(playlist)) => playlist,
        Ok(None) => return None,
        Err(cause) => {
            tracing::warn!(%cause, "weekly exploration unavailable");
            return None;
        }
    };
    if !this_weeks(&playlist.date, now) {
        tracing::debug!(date = %playlist.date, "weekly exploration is not this week's; leaving it off");
        return None;
    }
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
async fn owned_album_ids(db: &Db, ids: &[String]) -> Option<HashMap<String, String>> {
    let mut owned = HashMap::new();
    for chunk in ids.chunks(200) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match ownership::owned_albums(db, &refs).await {
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
async fn owned_artist_ids(db: &Db, ids: &[String]) -> Option<HashMap<String, String>> {
    let mut owned = HashMap::new();
    for chunk in ids.chunks(200) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match ownership::owned_artists(db, &refs).await {
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
pub async fn mark_ownership(db: &Db, page: &mut DiscoverResponse) {
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
    let owned_albums = owned_album_ids(db, &album_ids).await;
    let owned_artists = owned_artist_ids(db, &artist_ids).await;
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
            if let SectionContent::Gaps { gaps } = &mut section.content {
                gaps.retain(|gap| !gap.album.in_library);
            }
        }
        // The person's own shelves hold only what the library still has.
        if let SectionContent::OwnShelves { own_shelves } = &mut section.content {
            own_shelves.played_a_lot.retain(|row| row.album.in_library);
            own_shelves.never_played.retain(|row| row.album.in_library);
        }
    }
    new_releases::mark_served(db, &mut page.sections).await;
    page.sections.retain(|section| !section.content.is_empty());
}
