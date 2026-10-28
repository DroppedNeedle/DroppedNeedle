//! Provider answers kept in the cache file, behind one helper.
//!
//! Every key starts with its [`Source`] and carries every value that
//! changes the answer (the MusicBrainz source, the user, a setting that
//! picks a different answer), so nothing ever needs clearing when settings
//! change: a changed value is a different key, and the old rows age out.
//! Keys are made only by [`CacheKey`].
//!
//! [`ProviderCacheStore::get_or_fetch`] returns a fresh row as it is. A
//! missing or stale row is fetched once for every caller of the same key
//! and class: a page and an image wait only on a fetch made for their own
//! kind of caller, so neither inherits the other's time budget, and
//! background work joins whatever is already on its way. A page or image
//! caller holding a stale row waits at most a second, then gets the row as
//! it is while the fetch finishes and stores on its own. When the fetch
//! fails, a row still inside its keep time is served and its source is
//! marked degraded. A fetch during which any provider call failed stores
//! nothing, so an outage is never kept as "nothing there".

use std::any::Any;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::cache::{CacheDb, CacheError, CacheKind, CacheRow, StoredRow};
use futures_util::FutureExt as _;
use futures_util::future::{BoxFuture, Shared};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};

use crate::degradation::{self, DegradationContext, IntegrationStatus};
use crate::limiter::RequestPriority;

/// Where a cached answer came from. Every key starts with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ProviderDataSource)]
pub enum Source {
    /// MusicBrainz entities, discographies, searches and tag lists.
    #[serde(rename = "musicbrainz")]
    MusicBrainz,
    /// ListenBrainz statistics and recommendations.
    #[serde(rename = "listenbrainz")]
    ListenBrainz,
    /// Last.fm statistics and details.
    #[serde(rename = "lastfm")]
    LastFm,
    /// TheAudioDB artist and album art.
    #[serde(rename = "audiodb")]
    AudioDb,
    /// Wikidata and Wikipedia biographies.
    Wikidata,
    /// Purchase links.
    Purchase,
    /// Plugin answers.
    Plugins,
    /// YouTube preview searches.
    #[serde(rename = "youtube")]
    YouTube,
    /// LRCLIB lyrics.
    Lrclib,
    /// Deezer and iTunes 30-second previews (titles only, never the
    /// short-lived preview links).
    Previews,
}

impl Source {
    /// Every source.
    pub const ALL: [Self; 10] = [
        Self::MusicBrainz,
        Self::ListenBrainz,
        Self::LastFm,
        Self::AudioDb,
        Self::Wikidata,
        Self::Purchase,
        Self::Plugins,
        Self::YouTube,
        Self::Lrclib,
        Self::Previews,
    ];

    /// The name keys start with and stats report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MusicBrainz => "musicbrainz",
            Self::ListenBrainz => "listenbrainz",
            Self::LastFm => "lastfm",
            Self::AudioDb => "audiodb",
            Self::Wikidata => "wikidata",
            Self::Purchase => "purchase",
            Self::Plugins => "plugins",
            Self::YouTube => "youtube",
            Self::Lrclib => "lrclib",
            Self::Previews => "previews",
        }
    }

    /// The source a stored name stands for.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.as_str() == name)
    }
}

/// A cache key: its source, then the parts that pick the answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    source: Source,
    text: String,
    personal: bool,
}

impl CacheKey {
    /// `source:part:part`. Parts are joined as given.
    #[must_use]
    pub fn new(source: Source, parts: &[&str]) -> Self {
        let mut text = source.as_str().to_owned();
        for part in parts {
            text.push(':');
            text.push_str(part);
        }
        Self {
            source,
            text,
            personal: false,
        }
    }

    /// `source:kind:<hash of parts>`, for free text that would make keys
    /// unbounded. Parts are whitespace-collapsed and lowercased first.
    #[must_use]
    pub fn hashed(source: Source, kind: &str, parts: &[&str]) -> Self {
        let mut hasher = Sha256::new();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                hasher.update(b"\x00");
            }
            let collapsed = part.split_whitespace().collect::<Vec<_>>().join(" ");
            hasher.update(collapsed.to_lowercase().as_bytes());
        }
        Self::new(source, &[kind, &format!("{:x}", hasher.finalize())])
    }

    /// Mark a user's own listening data: kept one day at most, so it does
    /// not sit on disk long after an unlink or an account deletion.
    #[must_use]
    pub fn personal(mut self) -> Self {
        self.personal = true;
        self
    }

    /// The key's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The key's source.
    #[must_use]
    pub const fn source(&self) -> Source {
        self.source
    }

    fn keep(&self) -> Duration {
        if self.personal { PERSONAL_KEEP } else { KEEP }
    }
}

/// How long a row is served without asking again, and kept to serve when
/// the provider fails.
pub const KEEP: Duration = Duration::from_secs(30 * 86_400);
/// The keep time of a user's own listening data.
pub const PERSONAL_KEEP: Duration = Duration::from_secs(86_400);
/// How long a page or image caller with a stale row waits for the refresh.
pub const STALE_WAIT: Duration = Duration::from_secs(1);

/// How long one kind of answer stays fresh, found and not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// A found answer.
    pub fresh: Duration,
    /// A "nothing there" answer.
    pub miss: Duration,
}

const HOUR: u64 = 3_600;
const MB_MISS: Duration = Duration::from_secs(600);

impl Policy {
    /// A MusicBrainz artist, release group or release.
    pub const MUSICBRAINZ_ENTITY: Self = Self {
        fresh: Duration::from_secs(24 * HOUR),
        miss: MB_MISS,
    };
    /// An artist's discography.
    pub const DISCOGRAPHY: Self = Self::MUSICBRAINZ_ENTITY;
    /// The release group of a release: it barely changes.
    pub const RELEASE_TO_GROUP: Self = Self {
        fresh: Duration::from_secs(7 * 24 * HOUR),
        miss: MB_MISS,
    };
    /// Where a merged MusicBrainz id now points: merges are rare and
    /// final.
    pub const MERGED_ID: Self = Self::RELEASE_TO_GROUP;
    /// A search.
    pub const SEARCH: Self = Self {
        fresh: Duration::from_secs(HOUR),
        miss: MB_MISS,
    };
    /// An artist page's discovery sections.
    pub const DISCOVERY: Self = Self {
        fresh: Duration::from_secs(6 * HOUR),
        miss: MB_MISS,
    };
    /// A user's listening statistics.
    pub const LISTENING: Self = Self {
        fresh: Duration::from_secs(300),
        miss: Duration::from_secs(300),
    };
    /// Charts and sitewide lists.
    pub const CHARTS: Self = Self {
        fresh: Duration::from_secs(HOUR),
        miss: MB_MISS,
    };
    /// TheAudioDB art.
    pub const AUDIODB: Self = Self {
        fresh: Duration::from_secs(7 * 24 * HOUR),
        miss: Duration::from_secs(24 * HOUR),
    };
}

/// What a fetch found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched<T> {
    /// An answer, stored.
    Found(T),
    /// The provider has nothing there, stored as such.
    Missing,
    /// An incomplete answer: returned, never stored.
    Partial(T),
}

/// Rows and bytes one source holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceStats {
    /// The source.
    pub source: Source,
    /// Rows.
    pub rows: u64,
    /// Bytes of keys and values.
    pub bytes: u64,
}

/// Which callers share a fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Class {
    Page,
    Image,
    Background,
}

impl Class {
    const fn of(priority: RequestPriority) -> Self {
        match priority {
            RequestPriority::UserInitiated => Self::Page,
            RequestPriority::ImageFetch => Self::Image,
            _ => Self::Background,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Note {
    source: &'static str,
    status: IntegrationStatus,
    deterministic: bool,
}

/// What a finished fetch left: its answer (none if it panicked) and what
/// the provider calls inside it recorded.
struct Landed {
    outcome: Option<Box<dyn Any + Send + Sync>>,
    notes: Vec<Note>,
}

type Flight = Shared<BoxFuture<'static, Arc<Landed>>>;
type Flights = HashMap<(String, Class), (u64, Flight)>;

/// The provider answers in the cache file. Cloning shares it.
#[derive(Clone)]
pub struct ProviderCacheStore {
    db: CacheDb,
    clock: Arc<dyn Clock>,
    flights: Arc<Mutex<Flights>>,
    next: Arc<AtomicU64>,
}

impl std::fmt::Debug for ProviderCacheStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCacheStore")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

fn lock(flights: &Mutex<Flights>) -> MutexGuard<'_, Flights> {
    flights
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn decode<T: DeserializeOwned>(row: &StoredRow) -> Option<Option<T>> {
    match row.kind {
        CacheKind::Missing => Some(None),
        CacheKind::Found => serde_json::from_slice(row.value.as_deref()?).ok().map(Some),
    }
}

fn duration_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Record a fetch's notes in the caller's own request, with failures
/// lowered to "degraded" when a kept row was served in their place.
fn relay(notes: &[Note], lowered: bool) {
    for note in notes {
        let status = if lowered && note.status == IntegrationStatus::Error {
            IntegrationStatus::Degraded
        } else {
            note.status
        };
        degradation::record_current(note.source, status, note.deterministic);
    }
}

fn notes_of(context: &DegradationContext) -> Vec<Note> {
    let deterministic = context.deterministic_sources();
    context
        .summary()
        .into_iter()
        .map(|(source, status)| Note {
            source,
            status,
            deterministic: deterministic.contains(source),
        })
        .collect()
}

impl ProviderCacheStore {
    /// The store over `db`, with times from `clock`.
    #[must_use]
    pub fn new(db: CacheDb, clock: Arc<dyn Clock>) -> Self {
        Self {
            db,
            clock,
            flights: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Whether the cache file is working.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.db.is_on()
    }

    async fn kept(&self, key: &CacheKey) -> Option<StoredRow> {
        let now = self.clock.now_ms();
        self.db
            .get(key.as_str())
            .await
            .filter(|row| row.keep_until > now)
    }

    /// The answer under `key`, fetched with `fetch` at `priority` when no
    /// fresh row is held. `None` is "the provider has nothing there".
    /// `fetch` is cloned so a caller can still answer when the shared fetch
    /// ends without one.
    pub async fn get_or_fetch<T, E, F, Fut>(
        &self,
        key: &CacheKey,
        policy: Policy,
        priority: RequestPriority,
        fetch: F,
    ) -> Result<Option<T>, E>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        E: Clone + Send + Sync + 'static,
        F: FnOnce(RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = Result<Fetched<T>, E>> + Send + 'static,
    {
        let kept = self.kept(key).await;
        if let Some(row) = &kept
            && row.fresh_until > self.clock.now_ms()
            && let Some(answer) = decode::<T>(row)
        {
            return Ok(answer);
        }
        let stale = kept.as_ref().and_then(decode::<T>);
        let class = Class::of(priority);
        // A spare copy answers this caller on its own if the shared fetch
        // ends without an answer (a panic, or the runtime shutting down).
        let spare = fetch.clone();
        let flight = self.flight(key, policy, class, priority, fetch);
        let landed = match (&stale, class) {
            (Some(answer), Class::Page | Class::Image) => {
                match tokio::time::timeout(STALE_WAIT, flight).await {
                    Ok(landed) => landed,
                    Err(_) => return Ok(answer.clone()),
                }
            }
            _ => flight.await,
        };
        let outcome = landed
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.downcast_ref::<Result<Fetched<T>, E>>());
        let Some(outcome) = outcome else {
            // The shared fetch ended without an answer, or another site
            // shares the key with another type: answer on our own, uncached.
            if let Some(answer) = stale {
                return Ok(answer);
            }
            return spare(priority).await.map(|fetched| match fetched {
                Fetched::Found(value) | Fetched::Partial(value) => Some(value),
                Fetched::Missing => None,
            });
        };
        match (outcome, stale) {
            (Ok(fetched), _) => {
                relay(&landed.notes, false);
                Ok(match fetched {
                    Fetched::Found(value) | Fetched::Partial(value) => Some(value.clone()),
                    Fetched::Missing => None,
                })
            }
            (Err(_), Some(answer)) => {
                relay(&landed.notes, true);
                if landed
                    .notes
                    .iter()
                    .all(|note| note.status == IntegrationStatus::Ok)
                {
                    degradation::record_current(
                        key.source().as_str(),
                        IntegrationStatus::Degraded,
                        false,
                    );
                }
                Ok(answer)
            }
            (Err(error), None) => {
                relay(&landed.notes, false);
                Err(error.clone())
            }
        }
    }

    /// Join the fetch already on its way for `key` that this class may
    /// share, or start one with `fetch`.
    fn flight<T, E, F, Fut>(
        &self,
        key: &CacheKey,
        policy: Policy,
        class: Class,
        priority: RequestPriority,
        fetch: F,
    ) -> Flight
    where
        T: Serialize + Send + Sync + 'static,
        E: Send + Sync + 'static,
        F: FnOnce(RequestPriority) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Fetched<T>, E>> + Send + 'static,
    {
        let mut flights = lock(&self.flights);
        let joinable: &[Class] = match class {
            Class::Page => &[Class::Page],
            Class::Image => &[Class::Image],
            Class::Background => &[Class::Page, Class::Image, Class::Background],
        };
        for joined in joinable {
            if let Some((_, flight)) = flights.get(&(key.as_str().to_owned(), *joined)) {
                return flight.clone();
            }
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let store = self.clone();
        let owned = key.clone();
        let task = async move {
            let (outcome, context) =
                degradation::scoped(AssertUnwindSafe(fetch(priority)).catch_unwind()).await;
            let notes = notes_of(&context);
            let outcome = match outcome {
                Ok(result) => {
                    if notes
                        .iter()
                        .all(|note| note.status == IntegrationStatus::Ok)
                    {
                        store.keep_answer(&owned, policy, &result);
                    }
                    Some(Box::new(result) as Box<dyn Any + Send + Sync>)
                }
                Err(_) => {
                    tracing::error!(key = owned.as_str(), "a provider cache fetch panicked");
                    None
                }
            };
            let mut flights = lock(&store.flights);
            let slot = (owned.as_str().to_owned(), class);
            if flights.get(&slot).is_some_and(|(held, _)| *held == id) {
                flights.remove(&slot);
            }
            Arc::new(Landed { outcome, notes })
        };
        // One task per fetch, so a caller that stops waiting (a page served
        // its stale row) never cancels the refresh the others share.
        let handle = tokio::spawn(task);
        let flight = async move {
            handle.await.unwrap_or_else(|_| {
                Arc::new(Landed {
                    outcome: None,
                    notes: Vec::new(),
                })
            })
        }
        .boxed()
        .shared();
        flights.insert((key.as_str().to_owned(), class), (id, flight.clone()));
        flight
    }

    fn keep_answer<T: Serialize, E>(
        &self,
        key: &CacheKey,
        policy: Policy,
        result: &Result<Fetched<T>, E>,
    ) {
        let (kind, value, fresh) = match result {
            Ok(Fetched::Found(value)) => match serde_json::to_vec(value) {
                Ok(bytes) => (CacheKind::Found, Some(bytes), policy.fresh),
                Err(error) => {
                    tracing::warn!(%error, key = key.as_str(), "provider answer did not encode; not stored");
                    return;
                }
            },
            Ok(Fetched::Missing) => (CacheKind::Missing, None, policy.miss),
            Ok(Fetched::Partial(_)) | Err(_) => return,
        };
        self.store(key, kind, value, fresh);
    }

    fn store(&self, key: &CacheKey, kind: CacheKind, value: Option<Vec<u8>>, fresh: Duration) {
        let now = self.clock.now_ms();
        let keep_until = now.saturating_add(duration_ms(key.keep()));
        let fresh_until = now.saturating_add(duration_ms(fresh)).min(keep_until);
        self.db.put(CacheRow {
            key: key.as_str().to_owned(),
            source: key.source().as_str(),
            kind,
            fetched_at: now,
            fresh_until,
            keep_until,
            value,
        });
    }

    /// Any answer held for `key` inside its keep time, fresh or stale.
    /// Never dials out. `Some(None)` is a stored "nothing there".
    pub async fn peek<T: DeserializeOwned>(&self, key: &CacheKey) -> Option<Option<T>> {
        self.kept(key).await.as_ref().and_then(decode)
    }

    /// The bytes of a fresh found row.
    pub async fn get_bytes(&self, key: &CacheKey) -> Option<Vec<u8>> {
        let now = self.clock.now_ms();
        self.kept(key)
            .await
            .filter(|row| row.fresh_until > now && row.kind == CacheKind::Found)
            .and_then(|row| row.value)
    }

    /// Store bytes as a found row, fresh for `ttl`. A zero `ttl` stores
    /// nothing.
    pub fn set_bytes(&self, key: &CacheKey, bytes: Vec<u8>, ttl: Duration) {
        if !ttl.is_zero() {
            self.store(key, CacheKind::Found, Some(bytes), ttl);
        }
    }

    /// Delete the rows under `keys` and wait for the commit, so a read
    /// right after fetches again.
    pub async fn forget(&self, keys: &[CacheKey]) {
        let keys = keys.iter().map(|key| key.as_str().to_owned()).collect();
        if let Err(error) = self.db.delete(keys).await {
            tracing::warn!(%error, "cached provider answers were not deleted");
        }
    }

    /// Delete every row. Answers how many went.
    pub async fn clear(&self) -> Result<u64, CacheError> {
        self.db.clear().await
    }

    /// Delete rows past their keep time.
    pub async fn sweep(&self) -> Result<u64, CacheError> {
        self.db.sweep(self.clock.now_ms()).await
    }

    /// Rows and bytes per source.
    pub async fn stats(&self) -> Vec<SourceStats> {
        self.db
            .stats()
            .await
            .into_iter()
            .filter_map(|usage| {
                Some(SourceStats {
                    source: Source::parse(&usage.source)?,
                    rows: usage.rows,
                    bytes: usage.bytes,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::clock::ManualClock;

    fn store() -> (ProviderCacheStore, ManualClock) {
        let clock = ManualClock::at(1_000_000);
        (
            ProviderCacheStore::new(CacheDb::scratch(), Arc::new(clock.clone())),
            clock,
        )
    }

    async fn settle(store: &ProviderCacheStore) {
        store.forget(&[]).await;
    }

    fn key() -> CacheKey {
        CacheKey::new(Source::MusicBrainz, &["ns", "artist", "a"])
    }

    /// A fetch during which a provider call failed stores nothing, so an
    /// outage is never kept as "nothing there"; a clean answer is kept.
    #[tokio::test]
    async fn a_fetch_that_met_a_failure_stores_nothing() {
        let (store, _) = store();
        let failed = store
            .get_or_fetch(
                &key(),
                Policy::MUSICBRAINZ_ENTITY,
                RequestPriority::BackgroundSync,
                |_| async {
                    degradation::record_current("musicbrainz", IntegrationStatus::Error, false);
                    Ok::<_, String>(Fetched::<String>::Missing)
                },
            )
            .await;
        assert_eq!(failed, Ok(None));
        settle(&store).await;
        assert_eq!(store.peek::<String>(&key()).await, None);

        let found = store
            .get_or_fetch(
                &key(),
                Policy::MUSICBRAINZ_ENTITY,
                RequestPriority::BackgroundSync,
                |_| async { Ok::<_, String>(Fetched::Found("answer".to_owned())) },
            )
            .await;
        assert_eq!(found, Ok(Some("answer".to_owned())));
        settle(&store).await;
        assert_eq!(
            store.peek::<String>(&key()).await,
            Some(Some("answer".to_owned()))
        );
    }
}
