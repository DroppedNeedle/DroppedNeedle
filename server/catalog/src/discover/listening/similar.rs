//! The one answer to "who is like this artist", read by every list that
//! asks: Discover's shelves, the queue, radio, the artist and album pages
//! and the Weekly Mix.
//!
//! ListenBrainz labs' co-listening list and each neighbour's listener
//! count are kept facts ([`KeptFacts`]). The order is worked out on each
//! read: a neighbour's score over its listener count to [`FAME_EXPONENT`],
//! so an artist does not lead by being in everyone's sessions, scaled so
//! the best is 1.0. A neighbour with no count goes after every counted
//! one, in score order.
//!
//! A reader at background priority reads kept rows only: a list or a count
//! not kept is wanted, for the refresh job to read, and a list answers "not
//! yet". A reader a person waits on reads a list nobody has asked about
//! from labs (one at a time, stopping at the first failure) and counts its
//! neighbours in one read, keeping both. No token is sent for either: they
//! are public answers. Labs reads also rest after a failure for every
//! caller: the client holds that rest.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_providers::listenbrainz::{
    self, ArtistPopularity, LABS_SOURCE, ListenBrainzClient, ListenBrainzCredentials,
    SIMILAR_ARTISTS_ALGORITHM, ScoredSimilarArtist,
};
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::{
    CacheKey, IntegrationStatus, ProviderCacheStore, ProviderError, RequestPriority, Source,
    record_current,
};
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore};

use super::network::ListeningNetwork;
use super::rows::{ArtistRow, ScoredArtist};
use super::{Ask, Health, ListeningError};
use crate::discover::facts::{
    Counts, FactKind, FactSource, Kept, KeptFacts, Looked, Neighbour, Notes, WantOrder,
    labs_resting,
};
use crate::discover::ports::BoxFuture;

/// How strongly a neighbour's fame is divided out: its co-listening score
/// over its listener count to this power. The asked artist's own count
/// would scale every row alike, so the scaling to 1.0 leaves it out.
pub const FAME_EXPONENT: f64 = 0.5;

/// The neighbours' facts: their lists and their counts.
const LISTS: (FactKind, FactSource) = (FactKind::Neighbours, FactSource::Labs);
const COUNTS: (FactKind, FactSource) = (FactKind::Listeners, FactSource::ListenBrainz);

impl From<ProviderError> for ListeningError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

/// The two outside reads behind the answer.
pub(crate) trait SimilarReads: Send + Sync + 'static {
    /// How a read fails.
    type Error: From<ProviderError> + std::fmt::Display + Clone + Send + Sync + 'static;
    /// Labs' scored list for one artist, as answered.
    fn labs<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ScoredSimilarArtist>, Self::Error>>;
    /// Listener counts for artists, in as few calls as the batch allows,
    /// sent with no token.
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
    /// How long labs reads still rest after a failure, if they do: no list
    /// read is made meanwhile.
    fn labs_rest(&self) -> Option<Duration>;
}

/// The reads straight from the client, for callers outside the listening
/// data: the artist and album pages.
pub(super) struct ClientReads {
    client: ListenBrainzClient,
}

impl ClientReads {
    pub(super) fn new(client: &ListenBrainzClient) -> Arc<Self> {
        Arc::new(Self {
            client: client.clone(),
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
            self.client
                .artist_popularity(artist_mbids, &ListenBrainzCredentials::default(), priority)
                .await
        })
    }

    fn labs_rest(&self) -> Option<Duration> {
        self.client.labs_rest()
    }
}

/// The reads through the listening network, under an ask's cap and its
/// turn, so one ask's similar reads go one at a time. The count read is
/// noted in and held back by the popularity health.
pub(super) struct NetworkReads {
    pub(super) network: Arc<dyn ListeningNetwork>,
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
            let counts = self.network.artist_listeners(artist_mbids, priority).await;
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

    fn labs_rest(&self) -> Option<Duration> {
        self.network.labs_rest()
    }
}

/// What an artist whose list is not kept answers: "not yet".
fn not_yet() -> ProviderError {
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

/// Read labs' list for `artist` and keep it. While labs rests after a
/// failure no read is made: it answers that rest, and nothing is marked.
/// Any failure of a read that was made marks the list failed, so it backs
/// off.
///
/// # Errors
///
/// The read's failure, or labs' rest; the list kept before stays.
pub(crate) async fn fetch_list<R: SimilarReads + ?Sized>(
    facts: &KeptFacts,
    reads: &R,
    artist: &str,
    priority: RequestPriority,
) -> Result<Kept<Vec<Neighbour>>, R::Error> {
    if let Some(wait) = reads.labs_rest() {
        return Err(R::Error::from(labs_resting(wait)));
    }
    let answered = match reads.labs(artist, priority).await {
        Ok(answered) => answered,
        Err(error) => {
            if let Err(cause) = facts
                .keep_failed(LISTS, &[artist.to_owned()], WantOrder::Seed)
                .await
            {
                tracing::warn!(%cause, "a failed similar-artists read went unrecorded");
            }
            return Err(error);
        }
    };
    let mut seen = HashSet::new();
    let rows: Vec<Neighbour> = answered
        .into_iter()
        .filter(|row| seen.insert(lower(&row.artist_mbid)))
        .map(|row| Neighbour {
            mbid: lower(&row.artist_mbid),
            name: row.name,
            score: row.score as f64,
        })
        .collect();
    let fallback = if rows.is_empty() {
        Kept::Nothing
    } else {
        Kept::Found(rows.clone())
    };
    Ok(facts
        .keep_neighbours(artist, rows)
        .await
        .unwrap_or_else(|cause| {
            tracing::warn!(%cause, "a similar-artists list was read but not kept");
            fallback
        }))
}

/// Count `ids` in one read, which the client cuts into its batches, and
/// keep the counts.
///
/// # Errors
///
/// The read's failure; the counts kept before stay.
pub(crate) async fn fetch_counts<R: SimilarReads + ?Sized>(
    facts: &KeptFacts,
    reads: &R,
    ids: &[String],
    priority: RequestPriority,
) -> Result<HashMap<String, Counts>, R::Error> {
    let found = match reads.listeners(ids, priority).await {
        Ok(found) => found,
        Err(error) => {
            if let Err(cause) = facts.keep_failed(COUNTS, ids, WantOrder::Seed).await {
                tracing::warn!(%cause, "a failed listener count read went unrecorded");
            }
            return Err(error);
        }
    };
    let counts: HashMap<String, Counts> = found
        .into_iter()
        .map(|(id, counts)| {
            let counts = Counts {
                listeners: counts.listeners,
                listens: counts.listen_count,
            };
            (lower(&id), counts)
        })
        .collect();
    if let Err(cause) = facts.keep_listeners(ids, counts.clone()).await {
        tracing::warn!(%cause, "listener counts were read but not kept");
    }
    Ok(counts)
}

/// The list the provider cache kept before lists were kept facts: one
/// neighbour as kept there.
#[derive(Deserialize)]
struct CarriedNeighbour {
    mbid: String,
    name: Option<String>,
    score: i64,
}

/// One artist's counts as the provider cache kept them.
#[derive(Clone, Copy, Deserialize)]
struct CarriedCounts {
    listeners: Option<i64>,
    listens: Option<i64>,
}

fn carried_list_key(artist: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["labs_similar", SIMILAR_ARTISTS_ALGORITHM, artist],
    )
}

fn carried_counts_key(artist: &str) -> CacheKey {
    CacheKey::new(Source::ListenBrainz, &["artist_listeners", artist])
}

/// The list the provider cache holds for `artist` from before, kept as a
/// fact so an upgrade reads nothing again. `None` when it holds none.
async fn carry_list(
    cache: &ProviderCacheStore,
    facts: &KeptFacts,
    artist: &str,
) -> Option<Kept<Vec<Neighbour>>> {
    let carried = cache
        .peek::<Vec<CarriedNeighbour>>(&carried_list_key(artist))
        .await?;
    let rows: Vec<Neighbour> = carried
        .unwrap_or_default()
        .into_iter()
        .map(|row| Neighbour {
            mbid: row.mbid,
            name: row.name,
            score: row.score as f64,
        })
        .collect();
    facts
        .keep_neighbours(artist, rows)
        .await
        .map_err(|cause| tracing::warn!(%cause, "a cached similar-artists list was not carried"))
        .ok()
}

/// The counts the provider cache holds for `ids` from before, kept as
/// facts.
async fn carry_counts(cache: &ProviderCacheStore, facts: &KeptFacts, ids: &[String]) {
    let keys: Vec<CacheKey> = ids.iter().map(|id| carried_counts_key(id)).collect();
    let held = cache.peek_many::<CarriedCounts>(&keys).await;
    let mut asked = Vec::new();
    let mut found = HashMap::new();
    for (id, held) in ids.iter().zip(held) {
        let Some(held) = held else { continue };
        asked.push(id.clone());
        if let Some(counts) = held {
            let counts = Counts {
                listeners: counts.listeners,
                listens: counts.listens,
            };
            found.insert(id.clone(), counts);
        }
    }
    if !asked.is_empty()
        && let Err(cause) = facts.keep_listeners(&asked, found).await
    {
        tracing::warn!(%cause, "cached listener counts were not carried");
    }
}

/// Every neighbour of `artist_mbid`, best first.
///
/// # Errors
///
/// "Not yet" when no list is kept, or the read's failure.
pub(super) async fn neighbours<R: SimilarReads>(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    reads: Arc<R>,
    artist_mbid: &str,
    priority: RequestPriority,
) -> Result<Vec<ScoredArtist>, R::Error> {
    let artist = [artist_mbid.to_owned()];
    neighbours_each(facts, cache, reads, &artist, priority)
        .await
        .0
        .pop()
        .unwrap_or_else(|| Ok(Vec::new()))
}

/// Every neighbour of each of `artist_mbids`, best first, in their order:
/// one pass. A list not kept is wanted and answers "not yet", unless a
/// person waits on the read: then lists are read from labs one at a time,
/// and after the first that fails the rest answer "not yet". The neighbours
/// not counted are counted in one read for a person, and wanted otherwise.
/// The flag is false when a list or a count was not had: the order is then
/// not one to keep.
pub(super) async fn neighbours_each<R: SimilarReads>(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    reads: Arc<R>,
    artist_mbids: &[String],
    priority: RequestPriority,
) -> (Vec<Result<Vec<ScoredArtist>, R::Error>>, bool) {
    let now = facts.now();
    let asked: Vec<String> = artist_mbids.iter().map(|artist| lower(artist)).collect();
    let mut unique = HashSet::new();
    let valid: Vec<String> = asked
        .iter()
        .filter(|artist| is_valid_mbid(artist) && unique.insert((*artist).clone()))
        .cloned()
        .collect();
    let mut lists = match facts.neighbours(&valid).await {
        Ok(lists) => lists,
        Err(cause) => {
            tracing::warn!(%cause, "kept similar-artists lists unreadable");
            HashMap::new()
        }
    };
    let person = priority.is_user_facing();
    let mut failures: HashMap<String, R::Error> = HashMap::new();
    let mut stopped = false;
    for artist in &valid {
        let looked = lists.entry(artist.clone()).or_insert_with(Looked::missing);
        if !matches!(looked.kept, Kept::Missing) {
            continue;
        }
        if let Some(kept) = carry_list(cache, facts, artist).await {
            looked.kept = kept;
            continue;
        }
        if !person || stopped {
            continue;
        }
        let _turn = turn(&*reads).await;
        // Another read may have kept the list while this one waited.
        if let Ok(mut again) = facts.neighbours(std::slice::from_ref(artist)).await
            && let Some(again) = again.remove(artist)
            && !matches!(again.kept, Kept::Missing)
        {
            looked.kept = again.kept;
            continue;
        }
        match fetch_list(facts, &*reads, artist, priority).await {
            Ok(kept) => looked.kept = kept,
            Err(error) => {
                stopped = true;
                failures.insert(artist.clone(), error);
            }
        }
    }

    let mut seen = HashSet::new();
    let ids: Vec<String> = valid
        .iter()
        .filter_map(|artist| match &lists.get(artist)?.kept {
            Kept::Found(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .filter(|row| seen.insert(row.mbid.clone()))
        .map(|row| row.mbid.clone())
        .collect();
    let (counts, counted) = listeners(facts, cache, &*reads, &ids, priority).await;

    let mut notes = Notes::default();
    for artist in &valid {
        if let Some(looked) = lists.get(artist) {
            notes.saw(now, (LISTS.0, LISTS.1, WantOrder::Seed), artist, looked);
        }
    }
    for (id, looked) in &counts {
        notes.saw(now, (COUNTS.0, COUNTS.1, WantOrder::Seed), id, looked);
    }
    if let Err(cause) = facts.note(notes).await {
        tracing::warn!(%cause, "what a similar-artists read saw went unnoted");
    }

    let counts: HashMap<String, Counts> = counts
        .into_iter()
        .filter_map(|(id, looked)| match looked.kept {
            Kept::Found(counts) => Some((id, counts)),
            _ => None,
        })
        .collect();
    let mut whole = counted;
    let answers = asked
        .iter()
        .map(|artist| {
            if !is_valid_mbid(artist) {
                return Ok(Vec::new());
            }
            if let Some(error) = failures.get(artist) {
                whole = false;
                return Err(error.clone());
            }
            match lists.get(artist).map(|looked| &looked.kept) {
                Some(Kept::Found(rows)) => Ok(rank(rows.clone(), &counts)),
                Some(Kept::Nothing) => Ok(Vec::new()),
                _ => {
                    whole = false;
                    Err(R::Error::from(not_yet()))
                }
            }
        })
        .collect();
    (answers, whole)
}

/// The kept counts of `ids`, the ones kept in the provider cache from
/// before carried over, and for a person the rest read in one read. The
/// flag says whether every id has one, a count or "none there".
async fn listeners<R: SimilarReads + ?Sized>(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    reads: &R,
    ids: &[String],
    priority: RequestPriority,
) -> (HashMap<String, Looked<Counts>>, bool) {
    if ids.is_empty() {
        return (HashMap::new(), true);
    }
    let mut counts = match facts.listeners(ids).await {
        Ok(counts) => counts,
        Err(cause) => {
            tracing::warn!(%cause, "kept listener counts unreadable");
            ids.iter()
                .map(|id| (id.clone(), Looked::missing()))
                .collect()
        }
    };
    let missing = |counts: &HashMap<String, Looked<Counts>>| -> Vec<String> {
        ids.iter()
            .filter(|id| {
                counts
                    .get(*id)
                    .is_none_or(|looked| matches!(looked.kept, Kept::Missing))
            })
            .cloned()
            .collect()
    };
    let uncounted = missing(&counts);
    if !uncounted.is_empty() {
        carry_counts(cache, facts, &uncounted).await;
        if let Ok(again) = facts.listeners(&uncounted).await {
            counts.extend(again);
        }
    }
    let uncounted = missing(&counts);
    if !uncounted.is_empty() && priority.is_user_facing() && !reads.counts_down() {
        let _turn = turn(reads).await;
        match fetch_counts(facts, reads, &uncounted, priority).await {
            Ok(_) => {
                if let Ok(again) = facts.listeners(&uncounted).await {
                    counts.extend(again);
                }
            }
            Err(error) => {
                tracing::debug!(%error, "listener counts unread; kept counts answer");
                record_current(listenbrainz::SOURCE, IntegrationStatus::Degraded, false);
            }
        }
    }
    let counted = missing(&counts).is_empty();
    (counts, counted)
}

/// `rows` best first: counted neighbours by score over listeners to
/// [`FAME_EXPONENT`], scaled so the best is 1.0, then the uncounted in
/// score order, scaled below the last counted one. Ties keep labs' order.
fn rank(rows: Vec<Neighbour>, counts: &HashMap<String, Counts>) -> Vec<ScoredArtist> {
    let raw_best = rows.iter().map(|row| row.score).fold(1.0_f64, f64::max);
    let mut counted = Vec::new();
    let mut uncounted = Vec::new();
    for row in rows {
        let count = counts.get(&row.mbid).copied();
        match count.and_then(|count| count.listeners) {
            Some(listeners) => {
                let value = row.score / (listeners.max(1) as f64).powf(FAME_EXPONENT);
                counted.push((value, row, count));
            }
            None => uncounted.push((row, count)),
        }
    }
    counted.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.score.total_cmp(&a.1.score)));
    uncounted.sort_by(|a, b| b.0.score.total_cmp(&a.0.score));
    let best = counted.first().map_or(0.0, |(value, _, _)| *value);
    let scaled = |value: f64| if best > 0.0 { value / best } else { 0.0 };
    let floor = counted.last().map_or(1.0, |(value, _, _)| scaled(*value));
    let shown = |row: Neighbour, score: f64, count: Option<Counts>| ScoredArtist {
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
        let score = floor * row.score / raw_best;
        ranked.push(shown(row, score, count));
    }
    ranked
}

/// How much of an artist's answer is kept, fresh or stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    /// No list, or a neighbour not counted: the refresh job has to read.
    No,
    /// Labs has nothing for the artist.
    Nothing,
    /// A list, with a count, or a "nothing there", for every neighbour.
    Listed,
}

/// How much of `artist_mbid`'s answer is kept. Never dials out.
pub(crate) async fn held(facts: &KeptFacts, artist_mbid: &str) -> Held {
    let artist = lower(artist_mbid);
    if !is_valid_mbid(&artist) {
        return Held::Nothing;
    }
    let rows = match facts
        .neighbours(std::slice::from_ref(&artist))
        .await
        .ok()
        .and_then(|mut lists| lists.remove(&artist))
        .map(|looked| looked.kept)
    {
        Some(Kept::Found(rows)) => rows,
        Some(Kept::Nothing) => return Held::Nothing,
        _ => return Held::No,
    };
    let ids: Vec<String> = rows.into_iter().map(|row| row.mbid).collect();
    let counted = facts.listeners(&ids).await.is_ok_and(|counts| {
        counts
            .values()
            .all(|looked| !matches!(looked.kept, Kept::Missing))
    });
    if counted { Held::Listed } else { Held::No }
}

/// Want `artist_mbids`' lists and their neighbours' counts with `order`,
/// for the refresh job, carrying what the provider cache kept from before.
/// Never dials out.
pub(crate) async fn want(
    facts: &KeptFacts,
    cache: &ProviderCacheStore,
    artist_mbids: &[String],
    order: WantOrder,
) {
    let now = facts.now();
    let artists: Vec<String> = artist_mbids
        .iter()
        .map(|artist| lower(artist))
        .filter(|artist| is_valid_mbid(artist))
        .collect();
    let Ok(mut lists) = facts.neighbours(&artists).await else {
        return;
    };
    let mut notes = Notes::default();
    let mut ids = Vec::new();
    for artist in &artists {
        let Some(looked) = lists.get_mut(artist) else {
            continue;
        };
        if matches!(looked.kept, Kept::Missing)
            && let Some(kept) = carry_list(cache, facts, artist).await
        {
            looked.kept = kept;
        }
        if let Kept::Found(rows) = &looked.kept {
            ids.extend(rows.iter().map(|row| row.mbid.clone()));
        }
        notes.saw(now, (LISTS.0, LISTS.1, order), artist, looked);
    }
    if !ids.is_empty() {
        carry_counts(cache, facts, &ids).await;
        if let Ok(counts) = facts.listeners(&ids).await {
            for (id, looked) in &counts {
                notes.saw(now, (COUNTS.0, COUNTS.1, order), id, looked);
            }
        }
    }
    if let Err(cause) = facts.note(notes).await {
        tracing::warn!(%cause, "the precache's similar-artists wants went unnoted");
    }
}
