//! Discover's one way to the listening services.
//!
//! [`ListeningData`] answers per concept (a sitewide chart, a user's top
//! albums, an artist's similar artists), whichever feature asks: the queue,
//! the discover page, home, radio, the genre page and the charts all read
//! the same rows under the same cache keys, so one endpoint is fetched once
//! for all of them. In front of the network it holds:
//!
//! - the provider cache and its single-flight (`get_or_fetch`). Keys start
//!   with their source and carry every value that changes the answer; a
//!   fetch that met any failure stores nothing. Lists are read in fixed
//!   pages (100 rows, or a floor per list) and cut to what was asked, so
//!   two callers asking for different lengths share one read.
//! - Last.fm readiness: a user who cannot read Last.fm gets empty answers
//!   before the cache is asked, so nothing empty is kept under keys every
//!   user shares.
//! - ListenBrainz popularity health: its "switched off" answer, or a rate
//!   limit met by a background read, marks popularity down for a while,
//!   and [`SourcePolicy`] lets Last.fm stand in.
//! - the build cap: an [`Ask`] made for a build lets at most
//!   [`BUILD_CAP`] of its provider calls run at once.
//! - the kept facts ([`KeptFacts`]): similar artists, listener counts,
//!   album lists and band links are read from the database, never fetched
//!   for a build.
//! - the background read meter: every read made at background priority to
//!   ListenBrainz or the official MusicBrainz service is counted by day,
//!   for the daily ceiling ([`crate::discover::facts::plan`]).
//! - the fresh releases feed ([`fresh`]): read once a day for the install,
//!   kept under one shared key, never read for a build.

pub(crate) mod albums;
pub mod band_links;
pub mod fresh;
pub mod gate;
mod live;
mod network;
mod policy;
mod rows;
pub mod shared;
pub(crate) mod similar;

pub use live::LiveNetwork;
pub use network::{ListeningNetwork, Read};
pub use policy::SourcePolicy;
pub use rows::*;

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::discover::facts::{KeptFacts, MeteredSource};
use crate::discover::ports::ProviderFailure;

/// Provider calls one build may have in flight at once. ListenBrainz and
/// MusicBrainz are paced at one call a second and Last.fm at five, so four
/// calls in flight keep every bucket busy at up to 800 ms a call, while
/// one build never fills the shared waiter queues on its own.
pub const BUILD_CAP: usize = 4;

/// MusicBrainz calls one build may spend matching Last.fm albums to release
/// groups: 30 seconds of its pace. A miss costs two calls. Answers are
/// remembered for good, so the rest are matched by the next builds.
pub const BUILD_LOOKUPS: usize = 30;

/// Rows per ListenBrainz statistics page (the most the endpoints serve).
const PAGE: u32 = droppedneedle_providers::listenbrainz::stats::MAX_STATS_COUNT;
/// Rows of the Last.fm sitewide chart read at once (the charts page up to
/// this many rows).
const LASTFM_CHART_ROWS: u32 = 200;
/// Last.fm albums read per artist.
const LASTFM_ALBUMS_FLOOR: u32 = 10;
/// Last.fm similar artists read per artist.
const LASTFM_SIMILAR_FLOOR: u32 = 25;
/// A Last.fm user's top artists read at once.
const LASTFM_USER_FLOOR: u32 = 25;
/// Rows of a MusicBrainz tag search page (the most it serves).
pub(in crate::discover) const TAG_PAGE: u32 = 100;
/// How long a Last.fm key that was refused leaves a user on ListenBrainz.
const LASTFM_DOWN_FOR: Duration = Duration::from_secs(3_600);
/// How long a popularity refusal, or a rate limit that names no wait,
/// keeps Last.fm standing in.
const POPULARITY_DOWN_FOR: Duration = Duration::from_secs(10 * 60);

const HOUR: Duration = Duration::from_secs(3_600);
// Background reads never take a stale row, so what a build reads is kept
// for hours: a warm build, or the rebuild behind a visit, then finds the
// last one's answers instead of paying every paced read again.
/// Sitewide charts.
const SITEWIDE: Policy = Policy {
    fresh: Duration::from_secs(12 * 3_600),
    miss: HOUR,
};
/// Artist-level lists (similar artists, tags, popularity): they move over
/// weeks, so a warm build reads them from the rows it finds.
const ARTIST: Policy = Policy::LISTENBRAINZ_ARTIST;
/// A user's statistics (top artists and albums, weekly charts and
/// playlists): ListenBrainz recomputes them about once a day, so each warm
/// build reads them once.
const STATS: Policy = Policy {
    fresh: Duration::from_secs(6 * 3_600),
    miss: Duration::from_secs(6 * 3_600),
};
/// What a user changes by listening or a click: latest scrobbles and
/// loved tracks.
const ACTIVITY: Policy = Policy {
    fresh: Duration::from_secs(15 * 60),
    miss: Duration::from_secs(15 * 60),
};
/// A user's slower statistics (genres).
const LISTENING_SLOW: Policy = Policy {
    fresh: Duration::from_secs(12 * 3_600),
    miss: Duration::from_secs(12 * 3_600),
};
/// Tag searches.
const TAGS: Policy = Policy {
    fresh: Duration::from_secs(12 * 3_600),
    miss: Duration::from_secs(12 * 3_600),
};

/// Why a listening read failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListeningError {
    /// An outside service failed, with its own typed error.
    Provider(ProviderError),
    /// The user's media server could not be read. The string is a
    /// log-only cause.
    MediaServer(String),
}

impl std::fmt::Display for ListeningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => write!(f, "{error}"),
            Self::MediaServer(cause) => write!(f, "media server: {cause}"),
        }
    }
}

impl From<ListeningError> for ProviderFailure {
    fn from(error: ListeningError) -> Self {
        match error {
            ListeningError::Provider(error) => Self::Provider(error),
            ListeningError::MediaServer(cause) => Self::Failed(cause),
        }
    }
}

/// A listening answer or its typed failure.
pub type ListeningResult<T> = Result<T, ListeningError>;

/// Who is asking: the priority the provider calls go out at, for a
/// build the cap its calls share, and whether a read made for it failed.
#[derive(Debug, Clone)]
pub struct Ask {
    priority: RequestPriority,
    permits: Option<Arc<Semaphore>>,
    /// The ask's similar-artist reads take turns, so none pile up behind a
    /// failing one.
    similar_turns: Arc<tokio::sync::Mutex<()>>,
    failed: Arc<AtomicBool>,
    lookups_left: Option<Arc<AtomicUsize>>,
}

impl Ask {
    /// One read a person is waiting on.
    pub fn person() -> Self {
        Self {
            priority: RequestPriority::UserInitiated,
            permits: None,
            similar_turns: Arc::default(),
            failed: Arc::default(),
            lookups_left: None,
        }
    }

    /// One build: its calls go out at `priority`, at most [`BUILD_CAP`]
    /// at once. Every build makes its own.
    pub fn build(priority: RequestPriority) -> Self {
        Self {
            priority,
            permits: Some(Arc::new(Semaphore::new(BUILD_CAP))),
            similar_turns: Arc::default(),
            failed: Arc::default(),
            lookups_left: Some(Arc::new(AtomicUsize::new(BUILD_LOOKUPS))),
        }
    }

    /// The same ask, under the same cap, with a failure record of its own,
    /// so one shelf can tell whether its own reads all answered.
    pub fn watched(&self) -> Self {
        Self {
            priority: self.priority,
            permits: self.permits.clone(),
            similar_turns: Arc::clone(&self.similar_turns),
            failed: Arc::default(),
            lookups_left: self.lookups_left.clone(),
        }
    }

    /// Spend one of the build's album lookups; false when none are left.
    /// A person's read has no such budget.
    pub fn take_lookup(&self) -> bool {
        self.lookups_left.as_ref().is_none_or(|left| {
            left.try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        })
    }

    /// Whether a read made through this ask failed, or was dropped before
    /// it answered (a caller's time bound).
    pub fn met_failure(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Record a failed read the caller made outside the listening data.
    pub fn note_failure(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }

    /// The priority this ask's calls go out at.
    pub fn priority(&self) -> RequestPriority {
        self.priority
    }

    async fn permit(permits: Option<Arc<Semaphore>>) -> Option<OwnedSemaphorePermit> {
        match permits {
            Some(permits) => permits.acquire_owned().await.ok(),
            None => None,
        }
    }
}

/// ListenBrainz popularity health: down for a while after it switched a
/// popularity read off or rate limited one.
#[derive(Default)]
struct Health {
    popularity_down_until: Mutex<Option<Instant>>,
    lastfm_down_until: Mutex<HashMap<String, Instant>>,
}

impl Health {
    /// Note a popularity read made at `priority`. A rate limit counts only
    /// for a background read: a person's read is also turned down by our
    /// own pacing, which says nothing about ListenBrainz. Without this a
    /// 429 storm costs every build its retries, longer than a shelf's
    /// bound, and the shelves time out empty.
    fn note<T>(&self, result: &ListeningResult<T>, priority: RequestPriority) {
        let down_for = match result {
            Err(ListeningError::Provider(ProviderError::Disabled { .. })) => POPULARITY_DOWN_FOR,
            Err(ListeningError::Provider(ProviderError::RateLimited { retry_after, .. }))
                if !priority.is_user_facing() =>
            {
                retry_after.unwrap_or(POPULARITY_DOWN_FOR)
            }
            _ => return,
        };
        if let Ok(mut until) = self.popularity_down_until.lock() {
            let end = Instant::now() + down_for;
            *until = Some(until.map_or(end, |current| current.max(end)));
        }
    }

    /// Note a Last.fm read made with `user_id`'s key: a refusal means the
    /// key stopped working, for that user alone.
    fn note_lastfm<T>(&self, user_id: &str, result: &ListeningResult<T>) {
        let refused = matches!(
            result,
            Err(ListeningError::Provider(
                ProviderError::Unauthorized { .. }
                    | ProviderError::Forbidden { .. }
                    | ProviderError::Rejected { .. }
            ))
        );
        if refused && let Ok(mut down) = self.lastfm_down_until.lock() {
            let now = Instant::now();
            down.retain(|_, until| *until > now);
            down.insert(user_id.to_owned(), now + LASTFM_DOWN_FOR);
        }
    }

    /// Whether `user_id`'s Last.fm key was refused a short while ago.
    fn lastfm_down(&self, user_id: &str) -> bool {
        self.lastfm_down_until
            .lock()
            .map(|down| {
                down.get(user_id)
                    .is_some_and(|until| Instant::now() < *until)
            })
            .unwrap_or(false)
    }

    fn popularity_down(&self) -> bool {
        self.popularity_down_until
            .lock()
            .map(|until| until.is_some_and(|until| Instant::now() < until))
            .unwrap_or(false)
    }
}

/// Notes a read as failed on its ask unless it settles with an answer;
/// a read dropped before it answers counts as failed too.
struct Watch<'a> {
    ask: &'a Ask,
    answered: bool,
}

impl<'a> Watch<'a> {
    fn new(ask: &'a Ask) -> Self {
        Self {
            ask,
            answered: false,
        }
    }

    fn settle(mut self, answered: bool) {
        self.answered = answered;
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        if !self.answered {
            self.ask.note_failure();
        }
    }
}

type Network = Arc<dyn ListeningNetwork>;

/// The listening reads Discover makes, cached and coalesced.
#[derive(Clone)]
pub struct ListeningData {
    network: Network,
    cache: ProviderCacheStore,
    facts: KeptFacts,
    health: Arc<Health>,
    fresh: fresh::FreshFeedReads,
}

fn lower(text: &str) -> String {
    text.trim().to_lowercase()
}

fn artist_key(artist: &ArtistRow) -> String {
    artist
        .mbid
        .as_deref()
        .map(lower)
        .unwrap_or_else(|| lower(&artist.name))
}

fn cut<T>(rows: Vec<T>, count: usize) -> Vec<T> {
    rows.into_iter().take(count).collect()
}

/// The albums of `rows` that have a title and may be offered as albums by
/// `artist_mbid`.
fn offered(rows: Vec<RankedAlbum>, artist_mbid: &str) -> Vec<RankedAlbum> {
    gate::offered(rows, artist_mbid)
        .into_iter()
        .filter(|row| !row.album.title.is_empty())
        .collect()
}

impl ListeningData {
    /// The reads over `network`, cached in `cache`, the kept facts read
    /// from `facts`.
    pub fn new(
        network: Arc<dyn ListeningNetwork>,
        cache: ProviderCacheStore,
        facts: KeptFacts,
    ) -> Self {
        let fresh = fresh::FreshFeedReads::new(Arc::clone(&network), cache.clone(), facts.clone());
        Self {
            network,
            cache,
            facts,
            health: Arc::default(),
            fresh,
        }
    }

    /// The kept facts these reads read.
    pub fn facts(&self) -> &KeptFacts {
        &self.facts
    }

    /// The fresh releases feed these reads keep.
    pub fn fresh(&self) -> &fresh::FreshFeedReads {
        &self.fresh
    }

    /// What a read to `source` at `priority` is counted against, if
    /// anything: background reads to ListenBrainz, and to MusicBrainz on
    /// the official service.
    fn metered(&self, source: Source, priority: RequestPriority) -> Option<MeteredSource> {
        if priority.is_user_facing() {
            return None;
        }
        match source {
            Source::ListenBrainz => Some(MeteredSource::ListenBrainz),
            Source::MusicBrainz if self.source().mode == "official" => {
                Some(MeteredSource::MusicBrainz)
            }
            _ => None,
        }
    }

    /// One answer under `key`, fetched through the cache.
    async fn lookup<T, F, Fut>(
        &self,
        key: CacheKey,
        policy: Policy,
        ask: &Ask,
        fetch: F,
    ) -> ListeningResult<Option<T>>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        F: FnOnce(Network, RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = ListeningResult<Fetched<T>>> + Send + 'static,
    {
        let watch = Watch::new(ask);
        let network = Arc::clone(&self.network);
        let permits = ask.permits.clone();
        let source = key
            .as_str()
            .split(':')
            .next()
            .and_then(Source::parse)
            .and_then(|source| self.metered(source, ask.priority));
        let facts = self.facts.clone();
        let found = self
            .cache
            .get_or_fetch(&key, policy, ask.priority, move |priority| async move {
                let _permit = Ask::permit(permits).await;
                let fetched = fetch(network, priority).await;
                if let Some(source) = source {
                    facts.count_read(source, 1).await;
                }
                fetched
            })
            .await;
        watch.settle(found.is_ok());
        found
    }

    /// A list under `key`, fetched through the cache. A clean empty answer
    /// is kept for the policy's "nothing there" time.
    async fn list<T, F, Fut>(
        &self,
        key: CacheKey,
        policy: Policy,
        ask: &Ask,
        fetch: F,
    ) -> ListeningResult<Vec<T>>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        F: FnOnce(Network, RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = ListeningResult<Vec<T>>> + Send + 'static,
    {
        let found = self
            .lookup(key, policy, ask, move |network, priority| async move {
                fetch(network, priority).await.map(|rows| {
                    if rows.is_empty() {
                        Fetched::Missing
                    } else {
                        Fetched::Found(rows)
                    }
                })
            })
            .await?;
        Ok(found.unwrap_or_default())
    }

    /// `count` rows from `offset` of a list read in [`PAGE`]-row pages,
    /// each cached under `key(page offset)`.
    async fn paged<T, K, F, Fut>(
        &self,
        key: K,
        policy: Policy,
        ask: &Ask,
        count: u32,
        offset: u32,
        fetch: F,
    ) -> ListeningResult<Vec<T>>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        K: Fn(u32) -> CacheKey,
        F: Fn(Network, u32, RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = ListeningResult<Vec<T>>> + Send + 'static,
    {
        let first = offset / PAGE * PAGE;
        let end = offset.saturating_add(count);
        let mut rows = Vec::new();
        let mut at = first;
        while at < end {
            let fetch = fetch.clone();
            let page = self
                .list(key(at), policy, ask, move |network, priority| {
                    fetch(network, at, priority)
                })
                .await?;
            let full = page.len() >= PAGE as usize;
            rows.extend(page);
            if !full {
                break;
            }
            at += PAGE;
        }
        Ok(rows
            .into_iter()
            .skip((offset - first) as usize)
            .take(count as usize)
            .collect())
    }

    /// One uncached read under the ask's cap, counted when it goes to
    /// `source` in the background.
    async fn call<T>(
        &self,
        ask: &Ask,
        source: Option<Source>,
        read: impl Future<Output = ListeningResult<T>>,
    ) -> ListeningResult<T> {
        let watch = Watch::new(ask);
        let _permit = Ask::permit(ask.permits.clone()).await;
        let result = read.await;
        if let Some(source) = source.and_then(|source| self.metered(source, ask.priority)) {
            self.facts.count_read(source, 1).await;
        }
        watch.settle(result.is_ok());
        result
    }

    /// The MusicBrainz source in use.
    pub fn source(&self) -> SourceContext {
        self.network.source()
    }

    /// The cache namespace of the MusicBrainz source in use.
    pub fn namespace(&self) -> String {
        self.source().namespace()
    }

    /// The user's accounts.
    pub async fn accounts(&self, user_id: &str) -> UserMusic {
        self.network.accounts(user_id).await
    }

    /// The user's accounts with the services' health: which one answers.
    /// While the user's Last.fm key is being refused they read as having
    /// no Last.fm, so shelves fill from ListenBrainz instead of failing.
    pub async fn policy(&self, user_id: &str) -> SourcePolicy {
        let mut user = self.accounts(user_id).await;
        user.lastfm &= !self.health.lastfm_down(user_id);
        SourcePolicy::new(user, self.popularity_down())
    }

    /// The policy for accounts already read, with the services' health.
    pub fn policy_for(&self, user: &UserMusic) -> SourcePolicy {
        SourcePolicy::new(user.clone(), self.popularity_down())
    }

    /// Whether ListenBrainz refuses popularity reads right now.
    pub fn popularity_down(&self) -> bool {
        self.health.popularity_down()
    }

    /// Whether ListenBrainz reads are switched on (false while their
    /// setting does not read). While they are off every ListenBrainz read
    /// here answers empty before the cache, so no empty answer is kept for
    /// when they come back on.
    pub fn listenbrainz_on(&self) -> bool {
        self.network.listenbrainz_ready()
    }

    fn listenbrainz_off(&self) -> bool {
        !self.listenbrainz_on()
    }

    /// The sitewide ListenBrainz artist chart.
    pub async fn sitewide_artists(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let key = |at: u32| {
            CacheKey::new(
                Source::ListenBrainz,
                &["sitewide_artists", range.as_str(), &at.to_string()],
            )
        };
        self.paged(
            key,
            SITEWIDE,
            ask,
            count,
            offset,
            move |network, at, priority| async move {
                network.sitewide_artists(range, PAGE, at, priority).await
            },
        )
        .await
    }

    /// The sitewide ListenBrainz release-group chart.
    pub async fn sitewide_albums(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ListenedAlbum>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let key = |at: u32| {
            CacheKey::new(
                Source::ListenBrainz,
                &["sitewide_albums", range.as_str(), &at.to_string()],
            )
        };
        self.paged(
            key,
            SITEWIDE,
            ask,
            count,
            offset,
            move |network, at, priority| async move {
                network.sitewide_albums(range, PAGE, at, priority).await
            },
        )
        .await
    }

    /// A ListenBrainz user's top artists.
    pub async fn user_artists(
        &self,
        username: &str,
        range: StatsRange,
        count: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let user = lower(username);
        let name = username.to_owned();
        let key = |at: u32| {
            CacheKey::new(
                Source::ListenBrainz,
                &["user_artists", &user, range.as_str(), &at.to_string()],
            )
            .personal()
        };
        self.paged(key, STATS, ask, count, 0, move |network, at, priority| {
            let name = name.clone();
            async move { network.user_artists(&name, range, PAGE, at, priority).await }
        })
        .await
    }

    /// A ListenBrainz user's top release groups.
    pub async fn user_albums(
        &self,
        username: &str,
        range: StatsRange,
        count: u32,
        offset: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ListenedAlbum>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let user = lower(username);
        let name = username.to_owned();
        let key = |at: u32| {
            CacheKey::new(
                Source::ListenBrainz,
                &["user_albums", &user, range.as_str(), &at.to_string()],
            )
            .personal()
        };
        self.paged(
            key,
            STATS,
            ask,
            count,
            offset,
            move |network, at, priority| {
                let name = name.clone();
                async move { network.user_albums(&name, range, PAGE, at, priority).await }
            },
        )
        .await
    }

    /// Artists like one artist, best first, `limit` at most: the one
    /// answer ([`similar`]), from the kept facts. A list not kept answers
    /// "not yet" unless a person waits on the read.
    pub async fn similar_artists(
        &self,
        user_id: &str,
        artist_mbid: &str,
        limit: usize,
        ask: &Ask,
    ) -> ListeningResult<Vec<ScoredArtist>> {
        let artist = [artist_mbid.to_owned()];
        self.similar_artists_each(user_id, &artist, limit, ask)
            .await
            .pop()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    /// [`Self::similar_artists`] for each of `artist_mbids`, in their order,
    /// as one pass: it stops asking at its first failed list read, and
    /// counts every neighbour in one read.
    pub async fn similar_artists_each(
        &self,
        _user_id: &str,
        artist_mbids: &[String],
        limit: usize,
        ask: &Ask,
    ) -> Vec<ListeningResult<Vec<ScoredArtist>>> {
        // While ListenBrainz reads are off the kept lists still answer and
        // nothing is read, for a person too; an artist with none kept has
        // nothing to wait for.
        let off = self.listenbrainz_off();
        let priority = if off {
            RequestPriority::BackgroundSync
        } else {
            ask.priority
        };
        let watch = Watch::new(ask);
        let reads = Arc::new(similar::NetworkReads {
            network: Arc::clone(&self.network),
            permits: ask.permits.clone(),
            turns: Arc::clone(&ask.similar_turns),
            health: Arc::clone(&self.health),
        });
        let (found, counted) =
            similar::neighbours_each(&self.facts, &self.cache, reads, artist_mbids, priority).await;
        // An order built without every list and count is not kept for long:
        // the build that read it is tried again soon.
        watch.settle(off || (counted && found.iter().all(Result::is_ok)));
        found
            .into_iter()
            .map(|rows| match rows {
                Err(_) if off => Ok(Vec::new()),
                rows => rows.map(|rows| cut(rows, limit)),
            })
            .collect()
    }

    /// The albums `artist_mbid` may be offered by ([`albums`]), `count` at
    /// most, for anyone asking: no person's token is read or lent for it.
    /// While ListenBrainz reads are off the list is in date order, newest
    /// first. A list not kept yet answers empty, and the read counts as
    /// failed, so a shelf that had albums keeps them.
    pub async fn artist_albums(
        &self,
        _user_id: &str,
        artist_mbid: &str,
        count: usize,
        ask: &Ask,
    ) -> ListeningResult<Vec<RankedAlbum>> {
        let watch = Watch::new(ask);
        let reads = Arc::new(albums::NetworkReads {
            network: Arc::clone(&self.network),
            permits: ask.permits.clone(),
            health: Arc::clone(&self.health),
        });
        let namespace = self.namespace();
        let found = albums::artist_albums(
            &self.facts,
            &self.cache,
            reads,
            (&namespace, artist_mbid),
            self.listenbrainz_on(),
            ask.priority,
        )
        .await;
        watch.settle(found.as_ref().is_ok_and(Option::is_some));
        Ok(cut(offered(found?.unwrap_or_default(), artist_mbid), count))
    }

    /// A ListenBrainz user's genres with their listens, most listened
    /// first.
    pub async fn genres(&self, username: &str, ask: &Ask) -> ListeningResult<Vec<(String, i64)>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let key = CacheKey::new(Source::ListenBrainz, &["genres", &lower(username)]).personal();
        let name = username.to_owned();
        self.list(
            key,
            LISTENING_SLOW,
            ask,
            move |network, priority| async move { network.genres(&name, priority).await },
        )
        .await
    }

    /// A ListenBrainz user's loved recordings, newest first.
    pub async fn loved(
        &self,
        username: &str,
        count: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<LovedTrack>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let key = CacheKey::new(Source::ListenBrainz, &["loved", &lower(username)]).personal();
        let name = username.to_owned();
        let rows = self
            .list(key, ACTIVITY, ask, move |network, priority| async move {
                network.loved(&name, PAGE, priority).await
            })
            .await?;
        Ok(cut(rows, count as usize))
    }

    /// The user's newest weekly exploration playlist, `None` when there is
    /// none.
    pub async fn weekly_playlist(
        &self,
        user_id: &str,
        username: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<WeeklyPlaylist>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let key = CacheKey::new(Source::ListenBrainz, &["weekly", &lower(username)]).personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.lookup(key, STATS, ask, move |network, priority| async move {
            Ok(
                match network.weekly_playlist(&user, &name, priority).await? {
                    Some(playlist) => Fetched::Found(playlist),
                    None => Fetched::Missing,
                },
            )
        })
        .await
    }

    /// Release groups of recordings, keyed by lowercased recording id.
    pub async fn recording_groups(
        &self,
        user_id: &str,
        recording_mbids: &[String],
        ask: &Ask,
    ) -> ListeningResult<HashMap<String, String>> {
        if self.listenbrainz_off() {
            return Ok(Default::default());
        }
        let watch = Watch::new(ask);
        let found = self
            .cache
            .fetch_each(
                recording_mbids,
                droppedneedle_providers::listenbrainz::recording_group_key,
                Policy::RECORDING_TO_GROUP,
                |missing| async move {
                    let _permit = Ask::permit(ask.permits.clone()).await;
                    let found = self
                        .network
                        .recording_groups(user_id, &missing, ask.priority)
                        .await;
                    if let Some(source) = self.metered(Source::ListenBrainz, ask.priority) {
                        self.facts.count_read(source, 1).await;
                    }
                    found
                },
            )
            .await;
        watch.settle(found.is_ok());
        Ok(found?
            .into_iter()
            .map(|(recording, group)| (lower(&recording), group))
            .collect())
    }

    /// A Last.fm user's top artists over three months.
    pub async fn lastfm_user_artists(
        &self,
        user_id: &str,
        username: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let read = limit.max(LASTFM_USER_FLOOR);
        let key = CacheKey::new(
            Source::LastFm,
            &[
                "user_artists",
                &lower(username),
                "3month",
                &read.to_string(),
            ],
        )
        .personal();
        let (user, name, health) = (
            user_id.to_owned(),
            username.to_owned(),
            Arc::clone(&self.health),
        );
        let rows = self
            .list(key, STATS, ask, move |network, priority| async move {
                let found = network
                    .lastfm_user_artists(&user, &name, "3month", read, priority)
                    .await;
                health.note_lastfm(&user, &found);
                found
            })
            .await?;
        Ok(cut(rows, limit as usize))
    }

    /// A Last.fm user's top albums over `period`.
    pub async fn lastfm_user_albums(
        &self,
        user_id: &str,
        username: &str,
        period: &'static str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<LastFmChartAlbum>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(
            Source::LastFm,
            &["user_albums", &lower(username), period, &limit.to_string()],
        )
        .personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.list(key, STATS, ask, move |network, priority| async move {
            network
                .lastfm_user_albums(&user, &name, period, limit, priority)
                .await
        })
        .await
    }

    /// Artists Last.fm finds similar to one artist, by match.
    pub async fn lastfm_similar(
        &self,
        user_id: &str,
        artist: &ArtistRow,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ScoredArtist>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let read = limit.max(LASTFM_SIMILAR_FLOOR);
        let key = CacheKey::new(
            Source::LastFm,
            &["similar_artists", &artist_key(artist), &read.to_string()],
        );
        let (user, artist, health) = (user_id.to_owned(), artist.clone(), Arc::clone(&self.health));
        let rows = self
            .list(key, ARTIST, ask, move |network, priority| async move {
                let found = network.lastfm_similar(&user, &artist, read, priority).await;
                health.note_lastfm(&user, &found);
                found
            })
            .await?;
        Ok(cut(rows, limit as usize))
    }

    /// An artist's top albums on Last.fm.
    pub async fn lastfm_artist_albums(
        &self,
        user_id: &str,
        artist: &ArtistRow,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<LastFmAlbum>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let read = limit.max(LASTFM_ALBUMS_FLOOR);
        let key = CacheKey::new(
            Source::LastFm,
            &["artist_albums", &artist_key(artist), &read.to_string()],
        );
        let (user, artist, health) = (user_id.to_owned(), artist.clone(), Arc::clone(&self.health));
        let rows = self
            .list(key, ARTIST, ask, move |network, priority| async move {
                let found = network
                    .lastfm_artist_albums(&user, &artist, read, priority)
                    .await;
                health.note_lastfm(&user, &found);
                found
            })
            .await?;
        Ok(cut(rows, limit as usize))
    }

    /// The Last.fm sitewide artist chart.
    pub async fn lastfm_chart_artists(
        &self,
        user_id: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(
            Source::LastFm,
            &["chart_artists", &LASTFM_CHART_ROWS.to_string()],
        );
        let user = user_id.to_owned();
        let rows = self
            .list(key, SITEWIDE, ask, move |network, priority| async move {
                network
                    .lastfm_chart_artists(&user, LASTFM_CHART_ROWS, priority)
                    .await
            })
            .await?;
        Ok(cut(rows, limit as usize))
    }

    /// Last.fm artist facts (tags and bio), `None` when Last.fm is off or
    /// does not know the artist.
    pub async fn lastfm_artist(
        &self,
        user_id: &str,
        artist: &str,
        mbid: Option<&str>,
        ask: &Ask,
    ) -> ListeningResult<Option<LastFmFacts>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let id = mbid.map_or_else(|| lower(artist), lower);
        let key = CacheKey::hashed(Source::LastFm, "artist_facts", &[&id]);
        let (user, artist, mbid) = (
            user_id.to_owned(),
            artist.to_owned(),
            mbid.map(str::to_owned),
        );
        self.lookup(key, ARTIST, ask, move |network, priority| async move {
            Ok(
                match network
                    .lastfm_artist(&user, &artist, mbid.as_deref(), priority)
                    .await?
                {
                    Some(facts) => Fetched::Found(facts),
                    None => Fetched::Missing,
                },
            )
        })
        .await
    }

    /// The user's Last.fm artists this week.
    pub async fn lastfm_weekly_artists(
        &self,
        user_id: &str,
        username: &str,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(Source::LastFm, &["weekly_artists", &lower(username)]).personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.list(key, STATS, ask, move |network, priority| async move {
            network.lastfm_weekly_artists(&user, &name, priority).await
        })
        .await
    }

    /// The user's Last.fm albums this week.
    pub async fn lastfm_weekly_albums(
        &self,
        user_id: &str,
        username: &str,
        ask: &Ask,
    ) -> ListeningResult<Vec<LastFmChartAlbum>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(Source::LastFm, &["weekly_albums", &lower(username)]).personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.list(key, STATS, ask, move |network, priority| async move {
            network.lastfm_weekly_albums(&user, &name, priority).await
        })
        .await
    }

    /// The user's latest Last.fm scrobbles, as albums.
    pub async fn lastfm_recent_albums(
        &self,
        user_id: &str,
        username: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<LastFmChartAlbum>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(
            Source::LastFm,
            &["recent", &lower(username), &limit.to_string()],
        )
        .personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.list(key, ACTIVITY, ask, move |network, priority| async move {
            network
                .lastfm_recent_albums(&user, &name, limit, priority)
                .await
        })
        .await
    }

    /// The most played artists carrying one Last.fm tag.
    pub async fn lastfm_tag_artists(
        &self,
        user_id: &str,
        tag: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(
            Source::LastFm,
            &["tag_artists", &lower(tag), &limit.to_string()],
        );
        let (user, tag) = (user_id.to_owned(), tag.to_owned());
        self.list(key, ARTIST, ask, move |network, priority| async move {
            network
                .lastfm_tag_artists(&user, &tag, limit, priority)
                .await
        })
        .await
    }

    /// A Last.fm user's loved tracks.
    pub async fn lastfm_loved(
        &self,
        user_id: &str,
        username: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<LovedTrack>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::new(
            Source::LastFm,
            &["loved", &lower(username), &limit.to_string()],
        )
        .personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.list(key, ACTIVITY, ask, move |network, priority| async move {
            network.lastfm_loved(&user, &name, limit, priority).await
        })
        .await
    }

    /// `limit` release groups carrying a MusicBrainz tag from `offset`,
    /// with the release types discovery leaves out already dropped.
    pub async fn tag_albums(
        &self,
        tag: &str,
        limit: u32,
        offset: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<TagAlbum>> {
        let key = CacheKey::new(
            Source::MusicBrainz,
            &[
                &self.namespace(),
                "tag_albums",
                &lower(tag),
                &offset.to_string(),
            ],
        );
        let tag = tag.to_owned();
        let rows = self
            .list(key, TAGS, ask, move |network, priority| async move {
                network.tag_albums(&tag, TAG_PAGE, offset, priority).await
            })
            .await?;
        Ok(cut(rows, limit as usize))
    }

    /// `limit` artists carrying a MusicBrainz tag from `offset`.
    pub async fn tag_artists(
        &self,
        tag: &str,
        limit: u32,
        offset: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        let key = CacheKey::new(
            Source::MusicBrainz,
            &[
                &self.namespace(),
                "tag_artists",
                &lower(tag),
                &offset.to_string(),
            ],
        );
        let tag = tag.to_owned();
        let rows = self
            .list(key, TAGS, ask, move |network, priority| async move {
                network.tag_artists(&tag, TAG_PAGE, offset, priority).await
            })
            .await?;
        Ok(cut(rows, limit as usize))
    }

    /// The release group a release belongs to. `None` is a definite miss.
    pub async fn release_group_of(
        &self,
        release_mbid: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<String>> {
        let key = CacheKey::new(
            Source::MusicBrainz,
            &[&self.namespace(), "release_group_of", &lower(release_mbid)],
        );
        let release = release_mbid.to_owned();
        self.lookup(
            key,
            Policy::RELEASE_TO_GROUP,
            ask,
            move |network, priority| async move {
                Ok(match network.release_group_of(&release, priority).await? {
                    Some(group) => Fetched::Found(group),
                    None => Fetched::Missing,
                })
            },
        )
        .await
    }

    /// Release-group facts for a card. `None` is a definite miss. Not
    /// cached here: the card that reads it is.
    pub async fn release_group(
        &self,
        mbid: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<ReleaseGroupFacts>> {
        let source = Some(Source::MusicBrainz);
        self.call(ask, source, self.network.release_group(mbid, ask.priority))
            .await
    }

    /// Artists from the user's Jellyfin account.
    pub async fn jellyfin_artists(
        &self,
        user_id: &str,
        list: JellyfinList,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
        self.call(
            ask,
            None,
            self.network.jellyfin_artists(user_id, list, limit),
        )
        .await
    }

    /// The user's most played Jellyfin artists with their play data.
    pub async fn jellyfin_plays(
        &self,
        user_id: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<PlayedArtist>> {
        self.call(ask, None, self.network.jellyfin_plays(user_id, limit))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused Last.fm key sends that user's popularity back to
    /// ListenBrainz and leaves every other user alone.
    #[test]
    fn a_refused_lastfm_key_goes_back_to_listenbrainz() {
        let refused: ListeningResult<()> =
            Err(ListeningError::Provider(ProviderError::Unauthorized {
                provider: "lastfm",
            }));
        let health = Health::default();
        health.note_lastfm("ada", &refused);
        assert!(health.lastfm_down("ada"));
        assert!(!health.lastfm_down("bob"));

        let policy = |lastfm: bool| {
            SourcePolicy::new(
                UserMusic {
                    lastfm,
                    lastfm_username: Some("ada".to_owned()),
                    primary: ListenSource::Lastfm,
                    ..UserMusic::default()
                },
                false,
            )
            .popularity()
        };
        assert_eq!(policy(true), ListenSource::Lastfm);
        assert_eq!(
            policy(!health.lastfm_down("ada")),
            ListenSource::Listenbrainz
        );
    }

    /// A build and every shelf it watches share one budget of MusicBrainz
    /// album lookups; a person's read has none.
    #[test]
    fn a_build_spends_at_most_its_lookups_across_its_shelves() {
        let build = Ask::build(RequestPriority::BackgroundSync);
        let shelf = build.watched();
        let spent = (0..BUILD_LOOKUPS * 2)
            .filter(|at| if at % 2 == 0 { &build } else { &shelf }.take_lookup())
            .count();
        assert_eq!(spent, BUILD_LOOKUPS);
        assert!(!build.take_lookup() && !shelf.take_lookup());

        let person = Ask::person();
        assert!((0..BUILD_LOOKUPS * 2).all(|_| person.take_lookup()));
    }

    #[test]
    fn a_background_rate_limit_lets_lastfm_stand_in_and_a_person_read_does_not() {
        let limited: ListeningResult<()> =
            Err(ListeningError::Provider(ProviderError::RateLimited {
                provider: "listenbrainz",
                retry_after: None,
            }));

        let health = Health::default();
        health.note(&limited, RequestPriority::UserInitiated);
        assert!(
            !health.popularity_down(),
            "our own pacing declined a person's read"
        );

        health.note(&limited, RequestPriority::BackgroundSync);
        assert!(health.popularity_down());
    }
}
