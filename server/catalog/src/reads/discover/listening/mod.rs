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
//! - ListenBrainz popularity health: its "switched off" answer marks
//!   popularity down for a while, and [`SourcePolicy`] lets Last.fm stand
//!   in.
//! - the build cap: an [`Ask`] made for a build lets at most
//!   [`BUILD_CAP`] of its provider calls run at once.

mod live;
mod network;
mod policy;
mod rows;

pub use live::LiveNetwork;
pub use network::{ListeningNetwork, Read};
pub use policy::SourcePolicy;
pub use rows::*;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, RequestPriority, Source,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::reads::discover::ports::ProviderFailure;

/// Provider calls one build may have in flight at once. ListenBrainz and
/// MusicBrainz are paced at one call a second and Last.fm at five, so four
/// calls in flight keep every bucket busy at up to 800 ms a call, while
/// one build never fills the shared waiter queues on its own.
pub const BUILD_CAP: usize = 4;

/// Rows per ListenBrainz statistics page (the most the endpoints serve).
const PAGE: u32 = droppedneedle_providers::listenbrainz::stats::MAX_STATS_COUNT;
/// Similar artists read per artist; a caller asking for more reads more.
const SIMILAR_FLOOR: usize = 25;
/// An artist's top albums read per artist.
const ARTIST_ALBUMS_FLOOR: usize = 25;
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
pub(in crate::reads::discover) const TAG_PAGE: u32 = 100;
/// How long a popularity refusal keeps Last.fm standing in.
const POPULARITY_DOWN_FOR: Duration = Duration::from_secs(10 * 60);

const HOUR: Duration = Duration::from_secs(3_600);
/// Sitewide charts and artist-level lists.
const SITEWIDE: Policy = Policy::CHARTS;
/// Artist-level lists (similar artists, an artist's albums, tags).
const ARTIST: Policy = Policy {
    fresh: HOUR,
    miss: HOUR,
};
/// A user's statistics that change by the listen.
const LISTENING: Policy = Policy::LISTENING;
/// A user's slower statistics (genres, similar users, fresh releases).
const LISTENING_SLOW: Policy = Policy {
    fresh: HOUR,
    miss: HOUR,
};
/// Tag searches.
const TAGS: Policy = Policy {
    fresh: Duration::from_secs(2 * 3_600),
    miss: Duration::from_secs(2 * 3_600),
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

/// Who is asking: the priority the provider calls go out at, and for a
/// build, the cap its calls share.
#[derive(Debug, Clone)]
pub struct Ask {
    priority: RequestPriority,
    permits: Option<Arc<Semaphore>>,
}

impl Ask {
    /// One read a person is waiting on.
    pub fn person() -> Self {
        Self {
            priority: RequestPriority::UserInitiated,
            permits: None,
        }
    }

    /// One build: its calls go out at `priority`, at most [`BUILD_CAP`]
    /// at once. Every build makes its own.
    pub fn build(priority: RequestPriority) -> Self {
        Self {
            priority,
            permits: Some(Arc::new(Semaphore::new(BUILD_CAP))),
        }
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
/// popularity read off.
#[derive(Default)]
struct Health {
    popularity_down_until: Mutex<Option<Instant>>,
}

impl Health {
    fn note<T>(&self, result: &ListeningResult<T>) {
        if matches!(
            result,
            Err(ListeningError::Provider(ProviderError::Disabled { .. }))
        ) && let Ok(mut until) = self.popularity_down_until.lock()
        {
            *until = Some(Instant::now() + POPULARITY_DOWN_FOR);
        }
    }

    fn popularity_down(&self) -> bool {
        self.popularity_down_until
            .lock()
            .map(|until| until.is_some_and(|until| Instant::now() < until))
            .unwrap_or(false)
    }
}

type Network = Arc<dyn ListeningNetwork>;

/// The listening reads Discover makes, cached and coalesced.
#[derive(Clone)]
pub struct ListeningData {
    network: Network,
    cache: ProviderCacheStore,
    health: Arc<Health>,
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

impl ListeningData {
    /// The reads over `network`, cached in `cache`.
    pub fn new(network: Arc<dyn ListeningNetwork>, cache: ProviderCacheStore) -> Self {
        Self {
            network,
            cache,
            health: Arc::default(),
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
        let network = Arc::clone(&self.network);
        let permits = ask.permits.clone();
        self.cache
            .get_or_fetch(&key, policy, ask.priority, move |priority| async move {
                let _permit = Ask::permit(permits).await;
                fetch(network, priority).await
            })
            .await
    }

    /// A list under `key`, fetched through the cache.
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
                fetch(network, priority).await.map(Fetched::Found)
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

    /// One uncached read under the ask's cap.
    async fn call<T>(&self, ask: &Ask, read: impl Future<Output = T>) -> T {
        let _permit = Ask::permit(ask.permits.clone()).await;
        read.await
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
    pub async fn policy(&self, user_id: &str) -> SourcePolicy {
        SourcePolicy::new(self.accounts(user_id).await, self.popularity_down())
    }

    /// The policy for accounts already read, with the services' health.
    pub fn policy_for(&self, user: &UserMusic) -> SourcePolicy {
        SourcePolicy::new(user.clone(), self.popularity_down())
    }

    /// Whether ListenBrainz refuses popularity reads right now.
    pub fn popularity_down(&self) -> bool {
        self.health.popularity_down()
    }

    /// The sitewide ListenBrainz artist chart.
    pub async fn sitewide_artists(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<ArtistRow>> {
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
        let user = lower(username);
        let name = username.to_owned();
        let key = |at: u32| {
            CacheKey::new(
                Source::ListenBrainz,
                &["user_artists", &user, range.as_str(), &at.to_string()],
            )
            .personal()
        };
        self.paged(
            key,
            LISTENING,
            ask,
            count,
            0,
            move |network, at, priority| {
                let name = name.clone();
                async move { network.user_artists(&name, range, PAGE, at, priority).await }
            },
        )
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
            LISTENING,
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

    /// Artists ListenBrainz finds similar to one artist, most listened
    /// first. `user_id` lends its token.
    pub async fn similar_artists(
        &self,
        user_id: &str,
        artist_mbid: &str,
        limit: usize,
        ask: &Ask,
    ) -> ListeningResult<Vec<ScoredArtist>> {
        let read = limit.max(SIMILAR_FLOOR);
        let key = CacheKey::new(
            Source::ListenBrainz,
            &["similar_artists", &lower(artist_mbid), &read.to_string()],
        );
        let (user, mbid) = (user_id.to_owned(), artist_mbid.to_owned());
        let rows = self
            .list(key, ARTIST, ask, move |network, priority| async move {
                network.similar_artists(&user, &mbid, read, priority).await
            })
            .await?;
        Ok(cut(rows, limit))
    }

    /// One artist's most played release groups, empty while ListenBrainz
    /// popularity is down. `user_id` lends its token.
    pub async fn artist_albums(
        &self,
        user_id: &str,
        artist_mbid: &str,
        count: usize,
        ask: &Ask,
    ) -> ListeningResult<Vec<RankedAlbum>> {
        if self.popularity_down() {
            return Ok(Vec::new());
        }
        let read = count.max(ARTIST_ALBUMS_FLOOR);
        let key = CacheKey::new(
            Source::ListenBrainz,
            &["artist_albums", &lower(artist_mbid), &read.to_string()],
        );
        let (user, mbid, health) = (
            user_id.to_owned(),
            artist_mbid.to_owned(),
            Arc::clone(&self.health),
        );
        let rows = self
            .list(key, ARTIST, ask, move |network, priority| async move {
                let found = network.artist_albums(&user, &mbid, read, priority).await;
                health.note(&found);
                found
            })
            .await?;
        Ok(cut(rows, count))
    }

    /// A ListenBrainz user's genres with their listens, most listened
    /// first.
    pub async fn genres(&self, username: &str, ask: &Ask) -> ListeningResult<Vec<(String, i64)>> {
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

    /// Recent releases by artists the user listens to.
    pub async fn fresh_releases(
        &self,
        username: &str,
        ask: &Ask,
    ) -> ListeningResult<Vec<AlbumRow>> {
        let key = CacheKey::new(Source::ListenBrainz, &["fresh", &lower(username)]).personal();
        let name = username.to_owned();
        self.list(
            key,
            LISTENING_SLOW,
            ask,
            move |network, priority| async move { network.fresh_releases(&name, priority).await },
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
        let key = CacheKey::new(Source::ListenBrainz, &["loved", &lower(username)]).personal();
        let name = username.to_owned();
        let rows = self
            .list(key, LISTENING, ask, move |network, priority| async move {
                network.loved(&name, PAGE, priority).await
            })
            .await?;
        Ok(cut(rows, count as usize))
    }

    /// Users whose listening is like this user's, most similar first.
    pub async fn similar_users(&self, username: &str, ask: &Ask) -> ListeningResult<Vec<String>> {
        let key =
            CacheKey::new(Source::ListenBrainz, &["similar_users", &lower(username)]).personal();
        let name = username.to_owned();
        self.list(
            key,
            LISTENING_SLOW,
            ask,
            move |network, priority| async move { network.similar_users(&name, priority).await },
        )
        .await
    }

    /// The user's newest weekly exploration playlist, `None` when there is
    /// none.
    pub async fn weekly_playlist(
        &self,
        user_id: &str,
        username: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<WeeklyPlaylist>> {
        let key = CacheKey::new(Source::ListenBrainz, &["weekly", &lower(username)]).personal();
        let (user, name) = (user_id.to_owned(), username.to_owned());
        self.lookup(key, LISTENING, ask, move |network, priority| async move {
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
        self.call(
            ask,
            self.network
                .recording_groups(user_id, recording_mbids, ask.priority),
        )
        .await
    }

    /// Listen counts per release group, each cached on its own; empty
    /// for the uncached ones while popularity is down.
    pub async fn listen_counts(
        &self,
        user_id: &str,
        release_group_mbids: &[String],
        ask: &Ask,
    ) -> ListeningResult<HashMap<String, i64>> {
        let key_of =
            |mbid: &str| CacheKey::new(Source::ListenBrainz, &["rg_popularity", &lower(mbid)]);
        let mut counts = HashMap::new();
        let mut missing = Vec::new();
        for mbid in release_group_mbids {
            let hit = self
                .cache
                .get_bytes(&key_of(mbid))
                .await
                .and_then(|bytes| serde_json::from_slice::<i64>(&bytes).ok());
            match hit {
                Some(count) => {
                    counts.insert(mbid.clone(), count);
                }
                None => missing.push(mbid.clone()),
            }
        }
        if missing.is_empty() || self.popularity_down() {
            return Ok(counts);
        }
        let fetched = self
            .call(
                ask,
                self.network.listen_counts(user_id, &missing, ask.priority),
            )
            .await;
        self.health.note(&fetched);
        for (mbid, count) in fetched? {
            if let Ok(bytes) = serde_json::to_vec(&count) {
                self.cache.set_bytes(&key_of(&mbid), bytes, HOUR);
            }
            if let Some(asked) = missing
                .iter()
                .find(|asked| asked.eq_ignore_ascii_case(&mbid))
            {
                counts.insert(asked.clone(), count);
            }
        }
        Ok(counts)
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
        let (user, name) = (user_id.to_owned(), username.to_owned());
        let rows = self
            .list(key, LISTENING, ask, move |network, priority| async move {
                network
                    .lastfm_user_artists(&user, &name, "3month", read, priority)
                    .await
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
        self.list(key, LISTENING, ask, move |network, priority| async move {
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
        let (user, artist) = (user_id.to_owned(), artist.clone());
        let rows = self
            .list(key, ARTIST, ask, move |network, priority| async move {
                network.lastfm_similar(&user, &artist, read, priority).await
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
        let (user, artist) = (user_id.to_owned(), artist.clone());
        let rows = self
            .list(key, ARTIST, ask, move |network, priority| async move {
                network
                    .lastfm_artist_albums(&user, &artist, read, priority)
                    .await
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

    /// Last.fm album facts, `None` when Last.fm is off or does not know it.
    pub async fn lastfm_album(
        &self,
        user_id: &str,
        artist: &str,
        album: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<LastFmFacts>> {
        if !self.network.lastfm_ready(user_id).await {
            return Ok(Default::default());
        }
        let key = CacheKey::hashed(Source::LastFm, "album_facts", &[artist, album]);
        let (user, artist, album) = (user_id.to_owned(), artist.to_owned(), album.to_owned());
        self.lookup(key, ARTIST, ask, move |network, priority| async move {
            Ok(
                match network
                    .lastfm_album(&user, &artist, &album, priority)
                    .await?
                {
                    Some(facts) => Fetched::Found(facts),
                    None => Fetched::Missing,
                },
            )
        })
        .await
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
        self.list(key, LISTENING, ask, move |network, priority| async move {
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
        self.list(key, LISTENING, ask, move |network, priority| async move {
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
        self.list(key, LISTENING, ask, move |network, priority| async move {
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
        self.list(key, LISTENING, ask, move |network, priority| async move {
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
        self.call(ask, self.network.release_group(mbid, ask.priority))
            .await
    }

    /// Artist facts for a card.
    pub async fn artist(&self, mbid: &str, ask: &Ask) -> ListeningResult<Option<ArtistFacts>> {
        self.call(ask, self.network.artist(mbid, ask.priority))
            .await
    }

    /// A YouTube link from a release's URL relationships.
    pub async fn release_video(
        &self,
        release_mbid: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<String>> {
        self.call(ask, self.network.release_video(release_mbid, ask.priority))
            .await
    }

    /// The first recordings on a release, in track order.
    pub async fn release_recordings(
        &self,
        release_mbid: &str,
        limit: usize,
        ask: &Ask,
    ) -> ListeningResult<Vec<String>> {
        self.call(
            ask,
            self.network
                .release_recordings(release_mbid, limit, ask.priority),
        )
        .await
    }

    /// A YouTube link from a recording's URL relationships.
    pub async fn recording_video(
        &self,
        recording_mbid: &str,
        ask: &Ask,
    ) -> ListeningResult<Option<String>> {
        self.call(
            ask,
            self.network.recording_video(recording_mbid, ask.priority),
        )
        .await
    }

    /// The Wikipedia intro behind a Wikipedia or Wikidata link.
    pub async fn wikipedia_extract(&self, url: &str, ask: &Ask) -> ListeningResult<Option<String>> {
        self.call(ask, self.network.wikipedia_extract(url, ask.priority))
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
        self.call(ask, self.network.jellyfin_artists(user_id, list, limit))
            .await
    }

    /// The user's most played Jellyfin artists with their play data.
    pub async fn jellyfin_plays(
        &self,
        user_id: &str,
        limit: u32,
        ask: &Ask,
    ) -> ListeningResult<Vec<PlayedArtist>> {
        self.call(ask, self.network.jellyfin_plays(user_id, limit))
            .await
    }
}
