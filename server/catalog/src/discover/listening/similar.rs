//! The one answer to "who is like this artist", read by every list that
//! asks: Discover's shelves, the queue, radio, the artist and album pages
//! and the Weekly Mix.
//!
//! ListenBrainz labs' co-listening list is kept under one key per artist,
//! and each neighbour's listener count under one key per neighbour. The
//! order is worked out on each read: a neighbour's score over its listener
//! count to [`FAME_EXPONENT`], so an artist does not lead by being in
//! everyone's sessions, scaled so the best is 1.0. A neighbour with no
//! count goes after every counted one, in score order. A failed read never
//! replaces a kept list or count, and nor does an empty list for an artist
//! with one kept: the kept row answers until a read succeeds.
//!
//! A pass over several artists reads their lists one at a time, stops
//! asking at its first failed list read (kept lists still answer), and
//! counts the neighbours of all of them in one read. Labs reads also rest
//! after a failure for every caller: the client holds that rest.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_providers::listenbrainz::{
    self, ArtistPopularity, LABS_SOURCE, ListenBrainzClient, SIMILAR_ARTISTS_ALGORITHM,
    ScoredSimilarArtist,
};
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::{
    CacheKey, Fetched, IntegrationStatus, Policy, ProviderCacheStore, ProviderError,
    RequestPriority, Source, record_current,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore};

use super::network::ListeningNetwork;
use super::rows::{ArtistRow, ScoredArtist};
use super::shared::Lender;
use super::{Ask, Health, ListeningError};
use crate::discover::ports::BoxFuture;

/// How strongly a neighbour's fame is divided out: its co-listening score
/// over its listener count to this power. The asked artist's own count
/// would scale every row alike, so the scaling to 1.0 leaves it out.
pub const FAME_EXPONENT: f64 = 0.5;

const DAY: u64 = 86_400;
/// An artist's list: it moves over weeks.
const LISTS: Policy = Policy::LISTENBRAINZ_ARTIST;
/// An artist's listener count: it moves over months.
const COUNTS: Policy = Policy {
    fresh: Duration::from_secs(30 * DAY),
    miss: Duration::from_secs(DAY),
};

/// One neighbour as kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Neighbour {
    mbid: String,
    name: Option<String>,
    score: i64,
}

/// One artist's counts as kept: `None` where ListenBrainz has none.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Listeners {
    listeners: Option<i64>,
    listens: Option<i64>,
}

impl From<ArtistPopularity> for Listeners {
    fn from(counts: ArtistPopularity) -> Self {
        Self {
            listeners: counts.listeners,
            listens: counts.listen_count,
        }
    }
}

fn list_key(artist: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["labs_similar", SIMILAR_ARTISTS_ALGORITHM, artist],
    )
}

fn listeners_key(artist: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["artist_listeners", &artist.trim().to_ascii_lowercase()],
    )
}

impl From<ProviderError> for ListeningError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

/// The two outside reads behind the answer.
pub(super) trait SimilarReads: Send + Sync + 'static {
    /// How a read fails.
    type Error: From<ProviderError> + std::fmt::Display + Clone + Send + Sync + 'static;
    /// Labs' scored list for one artist, as answered.
    fn labs<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ScoredSimilarArtist>, Self::Error>>;
    /// Listener counts for artists, in as few calls as the batch allows.
    fn listeners<'a>(
        &'a self,
        artist_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ArtistPopularity>, Self::Error>>;
    /// The turn these reads take one at a time, when they share one.
    fn turns(&self) -> Option<&Mutex<()>> {
        None
    }
    /// Whether ListenBrainz refuses popularity reads right now, so no
    /// count read is sent.
    fn counts_down(&self) -> bool {
        false
    }
}

/// The reads straight from the client, for callers outside the listening
/// data: the artist and album pages, the precache and the Weekly Mix. The
/// count read lends `lender`'s link.
pub(super) struct ClientReads {
    client: ListenBrainzClient,
    lender: Lender,
}

impl ClientReads {
    pub(super) fn new(client: &ListenBrainzClient, lender: &Lender) -> Arc<Self> {
        Arc::new(Self {
            client: client.clone(),
            lender: lender.clone(),
        })
    }
}

impl SimilarReads for ClientReads {
    type Error = ProviderError;

    fn labs<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ScoredSimilarArtist>, ProviderError>> {
        Box::pin(self.client.scored_similar_artists(artist_mbid, priority))
    }

    fn listeners<'a>(
        &'a self,
        artist_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ArtistPopularity>, ProviderError>> {
        Box::pin(async move {
            let creds = self.lender.credentials().await;
            self.client
                .artist_popularity(artist_mbids, &creds, priority)
                .await
        })
    }
}

/// The reads through the listening network, under an ask's cap and its
/// turn, so one ask's similar reads go one at a time. The count read lends
/// `user_id`'s link, and is noted in and held back by the popularity
/// health.
pub(super) struct NetworkReads {
    pub(super) network: Arc<dyn ListeningNetwork>,
    pub(super) user_id: String,
    pub(super) permits: Option<Arc<Semaphore>>,
    pub(super) turns: Arc<Mutex<()>>,
    pub(super) health: Arc<Health>,
}

impl SimilarReads for NetworkReads {
    type Error = ListeningError;

    fn labs<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ScoredSimilarArtist>, ListeningError>> {
        Box::pin(async move {
            let _permit = Ask::permit(self.permits.clone()).await;
            self.network.scored_similar(artist_mbid, priority).await
        })
    }

    fn listeners<'a>(
        &'a self,
        artist_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ArtistPopularity>, ListeningError>> {
        Box::pin(async move {
            let _permit = Ask::permit(self.permits.clone()).await;
            let counts = self
                .network
                .artist_listeners(&self.user_id, artist_mbids, priority)
                .await;
            self.health.note(&counts, priority);
            counts
        })
    }

    fn turns(&self) -> Option<&Mutex<()>> {
        Some(&self.turns)
    }

    fn counts_down(&self) -> bool {
        self.health.popularity_down()
    }
}

/// What an artist the pass no longer asks for answers when nothing is kept.
fn not_asked() -> ProviderError {
    ProviderError::Unavailable {
        provider: LABS_SOURCE,
        retry_after: None,
    }
}

fn lower(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

async fn turn<R: SimilarReads + ?Sized>(reads: &R) -> Option<tokio::sync::MutexGuard<'_, ()>> {
    match reads.turns() {
        Some(turns) => Some(turns.lock().await),
        None => None,
    }
}

/// Every neighbour of `artist_mbid`, best first.
///
/// # Errors
///
/// The read's failure when no list is kept to stand in.
pub(super) async fn neighbours<R: SimilarReads>(
    cache: &ProviderCacheStore,
    reads: Arc<R>,
    artist_mbid: &str,
    priority: RequestPriority,
) -> Result<Vec<ScoredArtist>, R::Error> {
    let artist = [artist_mbid.to_owned()];
    neighbours_each(cache, reads, &artist, priority)
        .await
        .0
        .pop()
        .unwrap_or_else(|| Ok(Vec::new()))
}

/// Every neighbour of each of `artist_mbids`, best first, in their order:
/// one pass. The lists are read one at a time; after the first that fails,
/// the rest answer only from kept lists. The neighbours of all of them are
/// counted in one read, which the client cuts into its batches. The flag is
/// false when a neighbour's count could not be had: the order is then not
/// one to keep, and the pass notes ListenBrainz as degraded, so no answer
/// built from it is kept as a good one.
pub(super) async fn neighbours_each<R: SimilarReads>(
    cache: &ProviderCacheStore,
    reads: Arc<R>,
    artist_mbids: &[String],
    priority: RequestPriority,
) -> (Vec<Result<Vec<ScoredArtist>, R::Error>>, bool) {
    let mut lists = Vec::with_capacity(artist_mbids.len());
    let mut failed = false;
    for artist in artist_mbids {
        let artist = lower(artist);
        let found = if failed {
            match cache.peek::<Vec<Neighbour>>(&list_key(&artist)).await {
                Some(kept) => Ok(kept.unwrap_or_default()),
                None => Err(R::Error::from(not_asked())),
            }
        } else {
            list(cache, &reads, &artist, priority).await
        };
        failed |= found.is_err();
        lists.push(found);
    }
    let mut seen = HashSet::new();
    let ids: Vec<String> = lists
        .iter()
        .flatten()
        .flatten()
        .filter(|row| seen.insert(row.mbid.clone()))
        .map(|row| row.mbid.clone())
        .collect();
    let (counts, counted) = listeners(cache, &*reads, &ids, priority).await;
    if !counted {
        record_current(listenbrainz::SOURCE, IntegrationStatus::Degraded, false);
    }
    let ranked = lists
        .into_iter()
        .map(|found| found.map(|rows| rank(rows, &counts)))
        .collect();
    (ranked, counted)
}

/// One artist's kept list, read from labs when none is fresh. Nothing else
/// is read in its fetch, so a failed count read never stops a good list
/// being kept.
async fn list<R: SimilarReads>(
    cache: &ProviderCacheStore,
    reads: &Arc<R>,
    artist: &str,
    priority: RequestPriority,
) -> Result<Vec<Neighbour>, R::Error> {
    if !is_valid_mbid(artist) {
        return Ok(Vec::new());
    }
    let key = list_key(artist);
    let (store, reads, artist, kept_key) = (
        cache.clone(),
        Arc::clone(reads),
        artist.to_owned(),
        key.clone(),
    );
    let found = cache
        .get_or_fetch(&key, LISTS, priority, move |priority| async move {
            let answered = {
                let _turn = turn(&*reads).await;
                // Another fetch may have kept the list while this one waited
                // for its turn: what it kept answers, and is not kept again.
                if store.fresh(&kept_key).await
                    && let Some(kept) = store.peek::<Vec<Neighbour>>(&kept_key).await
                {
                    return Ok(Fetched::Partial(kept.unwrap_or_default()));
                }
                reads.labs(&artist, priority).await?
            };
            let mut seen = HashSet::new();
            let rows: Vec<Neighbour> = answered
                .into_iter()
                .filter(|row| seen.insert(lower(&row.artist_mbid)))
                .map(|row| Neighbour {
                    mbid: lower(&row.artist_mbid),
                    name: row.name,
                    score: row.score,
                })
                .collect();
            if rows.is_empty() {
                // A retired dataset may answer a known artist with nothing.
                // The list kept for it is kept again, fresh, so it never ages
                // out while labs answers nothing for the artist.
                return Ok(match store.peek::<Vec<Neighbour>>(&kept_key).await {
                    Some(Some(kept)) if !kept.is_empty() => Fetched::Found(kept),
                    _ => Fetched::Missing,
                });
            }
            Ok::<_, R::Error>(Fetched::Found(rows))
        })
        .await?;
    Ok(found.unwrap_or_default())
}

/// The counts of `ids`: fresh rows as kept, the rest in one read. While
/// popularity is down, or when that read fails, every kept count answers,
/// fresh or stale, and the flag says whether each id had one.
async fn listeners<R: SimilarReads + ?Sized>(
    cache: &ProviderCacheStore,
    reads: &R,
    ids: &[String],
    priority: RequestPriority,
) -> (HashMap<String, Listeners>, bool) {
    if ids.is_empty() {
        return (HashMap::new(), true);
    }
    if !reads.counts_down() {
        let fetched = cache
            .fetch_each(ids, listeners_key, COUNTS, |missing| async move {
                let _turn = turn(reads).await;
                // Another pass of this ask may have counted some of them
                // while this one waited for its turn.
                let (mut counted, missing) =
                    cache.held_each::<Listeners>(&missing, listeners_key).await;
                if missing.is_empty() {
                    return Ok(counted);
                }
                let counts = reads.listeners(&missing, priority).await?;
                counted.extend(
                    counts
                        .into_iter()
                        .map(|(id, counts)| (lower(&id), Listeners::from(counts))),
                );
                Ok::<_, R::Error>(counted)
            })
            .await;
        match fetched {
            Ok(found) => return (found, true),
            Err(error) => tracing::debug!(%error, "listener counts unread; kept counts answer"),
        }
    }
    let keys: Vec<CacheKey> = ids.iter().map(|id| listeners_key(id)).collect();
    let kept = cache.peek_many::<Listeners>(&keys).await;
    let every_id = kept.iter().all(Option::is_some);
    let counts = kept
        .into_iter()
        .zip(ids)
        .filter_map(|(kept, id)| Some((id.clone(), kept.flatten()?)))
        .collect();
    (counts, every_id)
}

/// `rows` best first: counted neighbours by score over listeners to
/// [`FAME_EXPONENT`], scaled so the best is 1.0, then the uncounted in
/// score order, scaled below the last counted one. Ties keep labs' order.
fn rank(rows: Vec<Neighbour>, counts: &HashMap<String, Listeners>) -> Vec<ScoredArtist> {
    let raw_best = rows.iter().map(|row| row.score).max().unwrap_or(1).max(1) as f64;
    let mut counted = Vec::new();
    let mut uncounted = Vec::new();
    for row in rows {
        let count = counts.get(&row.mbid).copied();
        match count.and_then(|count| count.listeners) {
            Some(listeners) => {
                let value = row.score as f64 / (listeners.max(1) as f64).powf(FAME_EXPONENT);
                counted.push((value, row, count));
            }
            None => uncounted.push((row, count)),
        }
    }
    counted.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.score.cmp(&a.1.score)));
    uncounted.sort_by(|a, b| b.0.score.cmp(&a.0.score));
    let best = counted.first().map_or(0.0, |(value, _, _)| *value);
    let scaled = |value: f64| if best > 0.0 { value / best } else { 0.0 };
    let floor = counted.last().map_or(1.0, |(value, _, _)| scaled(*value));
    let shown = |row: Neighbour, score: f64, count: Option<Listeners>| ScoredArtist {
        score,
        artist: ArtistRow {
            name: row.name.unwrap_or_default(),
            mbid: Some(row.mbid),
            listen_count: count.and_then(|count| count.listens),
        },
    };
    let mut ranked = Vec::with_capacity(counted.len() + uncounted.len());
    for (value, row, count) in counted {
        ranked.push(shown(row, scaled(value), count));
    }
    for (row, count) in uncounted {
        let score = floor * row.score as f64 / raw_best;
        ranked.push(shown(row, score, count));
    }
    ranked
}

/// How much of an artist's answer is kept, fresh or stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    /// No list: a read would ask labs.
    No,
    /// Labs has nothing for the artist.
    Nothing,
    /// A list, with a count, or a "nothing there", for every neighbour.
    Listed,
}

/// How much of `artist_mbid`'s answer is kept. Never dials out.
pub(crate) async fn held(cache: &ProviderCacheStore, artist_mbid: &str) -> Held {
    let artist = lower(artist_mbid);
    if !is_valid_mbid(&artist) {
        return Held::Nothing;
    }
    let rows = match cache.peek::<Vec<Neighbour>>(&list_key(&artist)).await {
        None => return Held::No,
        Some(None) => return Held::Nothing,
        Some(Some(rows)) => rows,
    };
    let keys: Vec<CacheKey> = rows.iter().map(|row| listeners_key(&row.mbid)).collect();
    let counted = cache
        .peek_many::<Listeners>(&keys)
        .await
        .iter()
        .all(Option::is_some);
    if counted { Held::Listed } else { Held::No }
}

/// For a background pass: `artist_mbid`'s list, read from labs when none
/// is fresh, without counting it. `None` when labs has nothing for the
/// artist, else the neighbours with no fresh count.
///
/// # Errors
///
/// The read's failure when no list is kept to stand in.
pub(crate) async fn warm_list(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    artist_mbid: &str,
    priority: RequestPriority,
) -> Result<Option<Vec<String>>, ProviderError> {
    let reads = ClientReads::new(client, &Lender::Anonymous);
    let rows = list(cache, &reads, &lower(artist_mbid), priority).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let ids: Vec<String> = rows.into_iter().map(|row| row.mbid).collect();
    let (_, missing) = cache.held_each::<Listeners>(&ids, listeners_key).await;
    Ok(Some(missing))
}

/// For a background pass: count `artist_mbids` in as few reads as the
/// batch allows, lending `lender`'s link. False when the read failed.
pub(crate) async fn warm_counts(
    cache: &ProviderCacheStore,
    client: &ListenBrainzClient,
    lender: &Lender,
    artist_mbids: &[String],
    priority: RequestPriority,
) -> bool {
    let reads = ClientReads::new(client, lender);
    cache
        .fetch_each(artist_mbids, listeners_key, COUNTS, |missing| async move {
            reads.listeners(&missing, priority).await.map(|counts| {
                counts
                    .into_iter()
                    .map(|(id, counts)| (lower(&id), Listeners::from(counts)))
                    .collect::<HashMap<_, _>>()
            })
        })
        .await
        .map_err(|error| tracing::debug!(%error, "precache: listener counts unread"))
        .is_ok()
}
