//! The fresh releases feed: every release ListenBrainz knows of from the
//! last 90 days, sitewide, read once a day for the whole install and kept
//! in the provider cache under one shared key.
//!
//! The feed is catalogue data, the same for everyone: it is read with no
//! token, and each person's list is cut from it by their own artists
//! ([`crate::discover::new_releases`]). One job kind ([`KIND`], one key for
//! the install) makes the read, queued by the demand tick and a person's
//! visit through [`Due`](crate::discover::due::Due) while
//! [`FreshFeedReads::due`] says the kept feed is a day old. It reads at
//! background priority, counts against the day's background reads, and
//! reads nothing while ListenBrainz reads are off, nobody has been seen in
//! seven days or the day's reads are spent. A read that fails or answers
//! nothing keeps the feed it had, and is tried once more an hour later that
//! UTC day.
//!
//! Readers never dial out: a page build and the list's route take what is
//! kept ([`FreshFeedReads::kept`]), and say how old it is.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use droppedneedle_kernel::jobs::BoxFuture as JobFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, RequestPriority, Source,
};
use serde::{Deserialize, Serialize};

use super::network::ListeningNetwork;
use super::rows::FreshRow;
use crate::discover::facts::{DAY_MS, KeptFacts, MeteredSource, ReadError, plan};

/// The feed job kind.
pub const KIND: JobKind = JobKind::new("discover.fresh_feed");
/// Its one key: the feed is the install's.
pub const KEY: &str = "install";

/// How feed jobs run: one read, done rows kept a day.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// How many days the feed covers.
pub const FEED_DAYS: u32 = 90;
/// A feed read longer ago than this is shown as of its date: the daily
/// read has missed at least once.
pub const STALE_AFTER_MS: i64 = 36 * 3_600_000;
/// How long after a failed read the one retry of the day may go.
const RETRY_AFTER_MS: i64 = 3_600_000;
/// The most reads one UTC day makes, the retry included.
const TRIES_A_DAY: u32 = 2;
/// How long a decoded feed is reused before the cache is asked again.
const MEMO_MS: i64 = 10 * 60_000;
/// How long a read serves before the next one is due, ms: a day.
const FRESH_MS: i64 = 24 * 3_600_000;
/// A read is fresh a day; the cache's keep time serves it after that.
const POLICY: Policy = Policy {
    fresh: Duration::from_secs(24 * 3_600),
    miss: Duration::from_secs(24 * 3_600),
};
/// The read goes out in the background.
const PRIORITY: RequestPriority = RequestPriority::BackgroundSync;

/// The feed as kept: when it was read and its rows, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreshFeed {
    /// When ListenBrainz answered it, unix ms.
    pub read_at: i64,
    /// Its releases.
    pub rows: Vec<FreshRow>,
}

/// The shared key the feed is kept under: nobody's own.
fn key() -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["fresh_releases", &FEED_DAYS.to_string()],
    )
}

/// The reads made on one UTC day.
#[derive(Debug, Default)]
struct Tries {
    day: i64,
    made: u32,
    /// When the day's last read failed, if it did.
    failed_at: Option<i64>,
}

struct Inner {
    network: Arc<dyn ListeningNetwork>,
    cache: ProviderCacheStore,
    facts: KeptFacts,
    /// The feed last decoded and when it was looked up.
    memo: Mutex<Option<(i64, Option<Arc<FreshFeed>>)>>,
    tries: Mutex<Tries>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The feed's reads: its one read a day and what readers take from the
/// cache. Clones share the memo and the day's tries.
#[derive(Clone)]
pub struct FreshFeedReads {
    inner: Arc<Inner>,
}

impl FreshFeedReads {
    /// The reads over `network`, kept in `cache`, counted in `facts`.
    pub fn new(
        network: Arc<dyn ListeningNetwork>,
        cache: ProviderCacheStore,
        facts: KeptFacts,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                network,
                cache,
                facts,
                memo: Mutex::new(None),
                tries: Mutex::new(Tries::default()),
            }),
        }
    }

    /// The kept feed, fresh or stale, `None` when none is kept. Never
    /// dials out.
    pub async fn kept(&self) -> Option<Arc<FreshFeed>> {
        let now = self.inner.facts.now();
        if let Some((at, feed)) = lock(&self.inner.memo).as_ref()
            && now - at < MEMO_MS
        {
            return feed.clone();
        }
        let feed = self
            .inner
            .cache
            .peek::<FreshFeed>(&key())
            .await
            .flatten()
            .map(Arc::new);
        *lock(&self.inner.memo) = Some((now, feed.clone()));
        feed
    }

    /// Whether the feed job has a read to make: the kept feed (by its own
    /// read time, from the memo, so a pending read loads it at most once
    /// every ten minutes) is a day old or missing, ListenBrainz reads are on, someone was seen in
    /// seven days, the day's reads have room, and today has made no read,
    /// or one that failed over an hour ago. A failed check is logged and
    /// answers no.
    pub(crate) async fn due(&self) -> bool {
        let inner = &self.inner;
        if !inner.network.listenbrainz_ready() || !self.try_left() {
            return false;
        }
        let now = inner.facts.now();
        if self
            .kept()
            .await
            .is_some_and(|feed| now - feed.read_at < FRESH_MS)
        {
            return false;
        }
        match plan::room(&inner.facts).await {
            Ok(room) => room > 0,
            Err(error) => {
                tracing::warn!(%error, "the day's reads unreadable; the fresh releases not read");
                false
            }
        }
    }

    /// Whether today may still make a read.
    fn try_left(&self) -> bool {
        let now = self.inner.facts.now();
        let tries = lock(&self.inner.tries);
        if tries.day != now.div_euclid(DAY_MS) || tries.made == 0 {
            return true;
        }
        tries.made < TRIES_A_DAY && tries.failed_at.is_some_and(|at| now - at >= RETRY_AFTER_MS)
    }

    /// Note one read made at `now`, and whether it failed.
    fn note_try(&self, now: i64, failed: bool) {
        let mut tries = lock(&self.inner.tries);
        let today = now.div_euclid(DAY_MS);
        if tries.day != today {
            *tries = Tries {
                day: today,
                ..Tries::default()
            };
        }
        tries.made += 1;
        tries.failed_at = failed.then_some(now);
    }

    /// The job's read: the feed asked for once, kept when it answered.
    /// Answers whether a new feed is kept. A failure, an empty answer
    /// (never stored, so it cannot replace a kept feed) and a write the
    /// cache dropped all keep the old one and count as a failed try.
    pub async fn refresh(&self) -> bool {
        if !self.due().await {
            return false;
        }
        let inner = &self.inner;
        let now = inner.facts.now();
        let network = Arc::clone(&inner.network);
        let facts = inner.facts.clone();
        let found = inner
            .cache
            .get_or_fetch(&key(), POLICY, PRIORITY, move |priority| async move {
                let read = network.sitewide_fresh_releases(FEED_DAYS, priority).await;
                if !read.as_ref().is_err_and(ReadError::unsent) {
                    facts.count_read(MeteredSource::ListenBrainz, 1).await;
                }
                read.map(|rows| {
                    let empty = rows.is_empty();
                    let feed = FreshFeed { read_at: now, rows };
                    if empty {
                        Fetched::Partial(feed)
                    } else {
                        Fetched::Found(feed)
                    }
                })
            })
            .await;
        let read = match found {
            Ok(Some(feed)) if feed.read_at == now && !feed.rows.is_empty() => Some(feed),
            // The read failed or answered nothing; the cache served the feed
            // it kept, if any.
            Ok(_) => {
                tracing::warn!("the fresh releases unread or empty; the kept ones stay");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "the fresh releases unread; the kept ones stay");
                None
            }
        };
        // A write the cache dropped (off, or its queue full) leaves the old
        // feed: the read is tried again within the day. A queued write is
        // already visible here.
        let answered = read.is_some();
        let kept = answered && inner.cache.fresh(&key()).await;
        self.note_try(now, !kept);
        let Some(feed) = read.filter(|_| kept) else {
            if answered {
                tracing::warn!("the fresh releases read but not kept; trying again later");
            }
            return false;
        };
        tracing::info!(releases = feed.rows.len(), "fresh releases read");
        *lock(&inner.memo) = Some((now, Some(Arc::new(feed))));
        true
    }
}

impl JobHandler for FreshFeedReads {
    fn run(&self, job: JobCtx) -> JobFuture<'static, Outcome> {
        let reads = self.clone();
        Box::pin(async move {
            reads.refresh().await;
            match reads.inner.facts.finish(move |tx| job.complete(tx)).await {
                Ok(()) => Outcome::Done,
                Err(error) if error.is_busy() => Outcome::Busy,
                // Another worker holds the job: its run reads the feed.
                Err(JobError::LeaseLost) => Outcome::Done,
                Err(error) => {
                    tracing::warn!(%error, "the fresh releases job not finished; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_FRESH_FEED_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}
