//! One per-user page cache for discover: the discover page, home's
//! provider shelves, the queue deck, and the Daily Mixes and Top Picks a
//! discover build reuses.
//!
//! Each page is one row of `catalog_cached_pages`, the only copy: nothing
//! is kept in memory over it, so every page survives a restart. A page
//! ages by time alone; a library change never throws one away, because
//! what depends on the library (ownership, home's local shelves) is added
//! when the page is served.
//!
//! A visit gets whatever row there is, however old, and starts a rebuild
//! behind it when the page is missing or past its freshness. One build
//! runs per user and page at a time. A build that fails or finds nothing
//! keeps the old page and is not tried again for a while, so a dead
//! provider does not cost every visit a build. Times come from the kernel
//! clock.
//!
//! A save that finds the database busy is not a failed build: the page is
//! held in memory and served until a later visit writes it. A clear of the
//! cache wins over a build already running: each build saves under the
//! clear generation it was claimed in, and a save under an older one
//! keeps nothing. A clear also drops the waits after failed builds.

mod rows;

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::reads::discover::listening::{ListeningData, UserMusic};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError};

/// Which page a row holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageKind {
    /// The discover page.
    Discover,
    /// Home's provider shelves.
    Home,
    /// The queue deck.
    Queue,
    /// The Daily Mixes a discover build reuses.
    DailyMixes,
    /// The Top Picks a discover build reuses.
    TopPicks,
}

impl PageKind {
    /// The stored name, one of the table's CHECK values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discover => "discover",
            Self::Home => "home",
            Self::Queue => "queue",
            Self::DailyMixes => "daily_mixes",
            Self::TopPicks => "top_picks",
        }
    }

    /// Whether a page built for `saved` may be served to a user who now
    /// has `wanted`. Linking or unlinking a service starts the pages over.
    /// The discover page and home are served from the old MusicBrainz
    /// source while the new one builds; a deck and the memos are not.
    fn fits(self, saved: &Variant, wanted: &Variant) -> bool {
        let services = saved.listenbrainz == wanted.listenbrainz && saved.lastfm == wanted.lastfm;
        match self {
            Self::Discover | Self::Home => services,
            Self::Queue => saved.source == wanted.source,
            Self::DailyMixes | Self::TopPicks => services && saved.source == wanted.source,
        }
    }
}

/// What a page was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    /// The MusicBrainz source's cache namespace.
    pub source: String,
    /// Whether the user had ListenBrainz linked.
    pub listenbrainz: bool,
    /// Whether the user could read Last.fm.
    pub lastfm: bool,
}

impl Variant {
    /// What a page built for the user now is built for.
    pub async fn for_user(listening: &ListeningData, user_id: &str) -> Self {
        Self::of(listening, &listening.accounts(user_id).await)
    }

    /// What a page built now for a user with `music` is built for.
    pub fn of(listening: &ListeningData, music: &UserMusic) -> Self {
        Self {
            source: listening.namespace(),
            listenbrainz: music.listenbrainz.is_some(),
            lastfm: music.lastfm,
        }
    }
}

/// One page as stored.
#[derive(Debug, Clone)]
pub struct Saved<T> {
    /// The page.
    pub value: T,
    /// When it was built, unix milliseconds.
    pub built_at: i64,
}

/// How long a page stays fresh, and how long a failed build waits.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// A page older than this is rebuilt behind the next visit, ms.
    pub fresh_ms: i64,
    /// A failed or empty build is not tried again for this long, ms.
    pub retry_ms: i64,
}

/// A built page whose save found the database busy.
#[derive(Clone)]
struct Pending {
    variant: Variant,
    payload: Vec<u8>,
    built_at: i64,
    generation: i64,
}

/// Builds running, builds that failed and pages not yet written, per
/// user, and the last clear generation seen.
#[derive(Default)]
struct Flights {
    running: HashMap<String, i64>,
    failed: HashMap<String, i64>,
    pending: HashMap<String, Pending>,
    generation: Option<i64>,
}

struct Inner {
    db: Db,
    clock: Arc<dyn Clock>,
    kind: PageKind,
    timing: Timing,
    flights: Mutex<Flights>,
}

impl Inner {
    fn flights(&self) -> MutexGuard<'_, Flights> {
        // The maps stay whole whatever panicked while the lock was held.
        self.flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Note the clear generation read now. After a clear, the waits after
    /// failed builds and the pages held for an older generation go.
    fn note_generation(&self, generation: i64) {
        let mut flights = self.flights();
        if flights.generation == Some(generation) {
            return;
        }
        if flights.generation.is_some() {
            flights.failed.clear();
        }
        flights
            .pending
            .retain(|_, pending| pending.generation == generation);
        flights.generation = Some(generation);
    }

    /// Write a page; a busy database holds it for a later try.
    async fn write(&self, user_id: &str, pending: Pending) -> bool {
        let row = rows::Row {
            variant: pending.variant.clone(),
            payload: pending.payload.clone(),
            built_at: pending.built_at,
        };
        match rows::save(&self.db, user_id, self.kind, row, pending.generation).await {
            Ok(()) => {
                let mut flights = self.flights();
                if flights
                    .pending
                    .get(user_id)
                    .is_some_and(|held| held.built_at <= pending.built_at)
                {
                    flights.pending.remove(user_id);
                }
                true
            }
            Err(DbError::Busy { .. }) => {
                tracing::debug!(
                    page = self.kind.as_str(),
                    "database busy; page held for a later save"
                );
                let mut flights = self.flights();
                let newer = flights
                    .pending
                    .get(user_id)
                    .is_some_and(|held| held.built_at > pending.built_at);
                if !newer {
                    flights.pending.insert(user_id.to_owned(), pending);
                }
                true
            }
            Err(error) => {
                tracing::warn!(%error, page = self.kind.as_str(), "page not saved");
                self.flights().pending.remove(user_id);
                false
            }
        }
    }
}

/// One kind of page, one row per user.
pub struct UserPageCache<T> {
    inner: Arc<Inner>,
    page: PhantomData<fn() -> T>,
}

impl<T> Clone for UserPageCache<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            page: PhantomData,
        }
    }
}

/// The right to run the user's one build. Dropped without
/// [`Claim::finish`] (a build that panicked or was stopped), it counts as
/// a failed build.
pub struct Claim {
    inner: Arc<Inner>,
    user_id: String,
    generation: i64,
    finished: bool,
}

impl Claim {
    /// The clear generation the build was claimed in, which every page it
    /// keeps is saved under.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// End the build; a failed one waits before the next try.
    fn finish(mut self, ok: bool) {
        self.release(ok);
        self.finished = true;
    }

    fn release(&self, ok: bool) {
        let now = self.inner.clock.now_ms();
        let mut flights = self.inner.flights();
        flights.running.remove(&self.user_id);
        if ok {
            flights.failed.remove(&self.user_id);
        } else {
            flights.failed.insert(self.user_id.clone(), now);
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.finished {
            self.release(false);
        }
    }
}

impl<T: Serialize + DeserializeOwned> UserPageCache<T> {
    /// The `kind` pages in `db`, aged by `clock`.
    pub fn new(db: Db, clock: Arc<dyn Clock>, kind: PageKind, timing: Timing) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                clock,
                kind,
                timing,
                flights: Mutex::default(),
            }),
            page: PhantomData,
        }
    }

    /// Now, unix milliseconds, from the kernel clock.
    pub fn now_ms(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    /// The user's page when there is one built for `wanted`, however old:
    /// the saved row, or a page held while the database was busy (whose
    /// save is tried again first). A row that cannot be read counts as
    /// missing, with a log line. A row that does not decode (one saved in
    /// an older shape) is deleted, with one log line, and counts as missing.
    pub async fn load(&self, user_id: &str, wanted: &Variant) -> Option<Saved<T>> {
        let held = self.inner.flights().pending.get(user_id).cloned();
        if let Some(held) = held {
            self.inner.write(user_id, held).await;
        }
        let (generation, row) = match rows::load(&self.inner.db, user_id, self.inner.kind).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(%error, page = self.inner.kind.as_str(), "saved page unreadable");
                return None;
            }
        };
        self.inner.note_generation(generation);
        let held = self.inner.flights().pending.get(user_id).cloned();
        let (variant, payload, built_at) = match (held, row) {
            (Some(held), _) => (held.variant, held.payload, held.built_at),
            (None, Some(row)) => (row.variant, row.payload, row.built_at),
            (None, None) => return None,
        };
        if !self.inner.kind.fits(&variant, wanted) {
            return None;
        }
        match serde_json::from_slice(&payload) {
            Ok(value) => Some(Saved { value, built_at }),
            Err(error) => {
                tracing::warn!(%error, page = self.inner.kind.as_str(), "saved page does not decode; dropping it");
                self.inner.flights().pending.remove(user_id);
                if let Err(error) = rows::forget(&self.inner.db, user_id, self.inner.kind).await {
                    tracing::warn!(%error, page = self.inner.kind.as_str(), "undecodable page not dropped");
                }
                None
            }
        }
    }

    /// The clear generation now, which a build outside [`Self::claim`]
    /// saves under. `None`, with a log line, when it cannot be read.
    pub async fn generation(&self) -> Option<i64> {
        match rows::generation(&self.inner.db).await {
            Ok(generation) => {
                self.inner.note_generation(generation);
                Some(generation)
            }
            Err(error) => {
                tracing::warn!(%error, page = self.inner.kind.as_str(), "page generation unreadable");
                None
            }
        }
    }

    /// Keep `value` as the user's page, built now for `variant` in clear
    /// `generation`. A busy database holds it in memory until a later
    /// visit writes it. False, with a log line, when it could not be kept.
    pub async fn save(&self, user_id: &str, variant: &Variant, value: &T, generation: i64) -> bool {
        let payload = match serde_json::to_vec(value) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, page = self.inner.kind.as_str(), "page does not encode");
                return false;
            }
        };
        let pending = Pending {
            variant: variant.clone(),
            payload,
            built_at: self.now_ms(),
            generation,
        };
        self.inner.write(user_id, pending).await
    }

    /// Drop the user's page, and the wait after a failed build.
    pub async fn forget(&self, user_id: &str) {
        {
            let mut flights = self.inner.flights();
            flights.failed.remove(user_id);
            flights.pending.remove(user_id);
        }
        if let Err(error) = rows::forget(&self.inner.db, user_id, self.inner.kind).await {
            tracing::warn!(%error, page = self.inner.kind.as_str(), "page not dropped");
        }
    }

    /// Drop the wait after a failed build, so the next visit builds.
    pub fn retry_now(&self, user_id: &str) {
        self.inner.flights().failed.remove(user_id);
    }

    /// When the user's running build started, unix milliseconds.
    pub fn building(&self, user_id: &str) -> Option<i64> {
        self.inner.flights().running.get(user_id).copied()
    }

    /// Whether a visit that found `saved` should start a build: none is
    /// running, the page is missing or past its freshness, and no build
    /// failed within the retry wait.
    pub fn due(&self, user_id: &str, saved: Option<&Saved<T>>) -> bool {
        let now = self.now_ms();
        let timing = self.inner.timing;
        let flights = self.inner.flights();
        if flights.running.contains_key(user_id) {
            return false;
        }
        if flights
            .failed
            .get(user_id)
            .is_some_and(|at| now - at < timing.retry_ms)
        {
            return false;
        }
        saved.is_none_or(|saved| now - saved.built_at > timing.fresh_ms)
    }

    /// Claim the user's one build in the clear generation now; `None`
    /// while another runs or when the generation cannot be read.
    pub async fn claim(&self, user_id: &str) -> Option<Claim> {
        let generation = self.generation().await?;
        let now = self.now_ms();
        let mut flights = self.inner.flights();
        if flights.running.contains_key(user_id) {
            return None;
        }
        flights.running.insert(user_id.to_owned(), now);
        Some(Claim {
            inner: Arc::clone(&self.inner),
            user_id: user_id.to_owned(),
            generation,
            finished: false,
        })
    }

    /// Run a claimed build. A page it returns replaces the saved one, unless
    /// the cache was cleared since the claim; `None` (failed, empty, or
    /// built for a source that changed meanwhile) keeps the old page and
    /// starts the retry wait. True when a page was kept or held.
    pub async fn run(
        &self,
        claim: Claim,
        variant: &Variant,
        build: impl Future<Output = Option<T>>,
    ) -> bool {
        let kept = match build.await {
            Some(page) => {
                self.save(&claim.user_id, variant, &page, claim.generation)
                    .await
            }
            None => false,
        };
        claim.finish(kept);
        kept
    }
}

/// Drop every saved page of every user (the admin's "Clear provider
/// data") and move to the next clear generation, so a build already
/// running keeps nothing. Each page is built again on its next visit or
/// warm pass. Returns how many went.
pub async fn clear_all(db: &Db) -> Result<usize, DbError> {
    rows::clear_all(db).await
}
