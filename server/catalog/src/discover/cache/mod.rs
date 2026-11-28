//! One per-user page cache for discover: the discover page and home's
//! provider shelves.
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
//! keeps the old page and is not tried again for a while, by a visit or
//! the warm cycle, so a dead provider does not cost every visit a build.
//! Times come from the kernel clock.
//!
//! A save that finds the database busy is not a failed build: the page is
//! held in memory and served until a later visit writes it. A clear of the
//! cache wins over a build already running: each build saves under the
//! clear generation it was claimed in, and a save under an older one
//! keeps nothing. A clear also drops the waits after failed builds. A
//! person's "Forget my history" wins the same way over that person's
//! builds alone: each build also carries the person's forget count from
//! its start, and a save under an older count keeps nothing.

mod rows;

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::discover::listening::{ListeningData, UserMusic};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError};

/// Which page a row holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageKind {
    /// The discover page.
    Discover,
    /// Home's provider shelves.
    Home,
}

droppedneedle_kernel::spelled!(PageKind {
    Discover => "discover",
    Home => "home",
});

/// Whether a page built for `saved` may be served to a user who now has
/// `wanted`. Linking or unlinking a service starts the pages over. A page
/// is served from the old MusicBrainz source while the new one builds.
fn fits(saved: &Variant, wanted: &Variant) -> bool {
    saved.listenbrainz == wanted.listenbrainz && saved.lastfm == wanted.lastfm
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

/// How many times each person has forgotten their history since the
/// server started, shared by every page cache and by the forget. Built
/// once by the discover wiring. A count starts at zero after a restart,
/// which is safe: no build outlives the process.
#[derive(Clone, Default)]
pub struct ForgetCounts(Arc<Mutex<HashMap<String, u64>>>);

impl ForgetCounts {
    fn counts(&self) -> MutexGuard<'_, HashMap<String, u64>> {
        // The map stays whole whatever panicked while the lock was held.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The person's count now. A job that keeps rows of theirs reads it
    /// when it begins and keeps nothing once it has moved on.
    pub(crate) fn of(&self, user_id: &str) -> u64 {
        self.counts().get(user_id).copied().unwrap_or(0)
    }

    /// Move the person's count on, so every build of theirs started
    /// before now keeps nothing and records nothing.
    pub(crate) fn bump(&self, user_id: &str) {
        *self.counts().entry(user_id.to_owned()).or_insert(0) += 1;
    }

    /// Whether a build of the person's started under `generation` may
    /// still keep what it built: no forget of theirs came since. Read it
    /// on the writer, inside the write that keeps it.
    pub(crate) fn holds(&self, user_id: &str, generation: Generation) -> bool {
        self.of(user_id) == generation.forgets
    }
}

/// What a build's pages are saved under: the clear generation and the
/// person's forget count when it started. A save keeps nothing once
/// either has moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    clear: i64,
    forgets: u64,
}

/// What became of a page a build tried to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Written.
    Written,
    /// The database was busy; held in memory and written by a later visit.
    Held,
    /// Not kept, and not a failure: the cache was cleared, the person
    /// forgot their history or their account went after the build began.
    Dropped,
    /// Not kept because something failed, with a log line.
    Failed,
}

impl SaveOutcome {
    /// Whether the page is kept, written or held.
    pub fn kept(self) -> bool {
        matches!(self, Self::Written | Self::Held)
    }
}

/// A built page whose save found the database busy.
#[derive(Clone)]
struct Pending {
    variant: Variant,
    payload: Vec<u8>,
    built_at: i64,
    generation: Generation,
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
    forgets: ForgetCounts,
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
            .retain(|_, pending| pending.generation.clear == generation);
        flights.generation = Some(generation);
    }

    /// Write a page; a busy database holds it for a later try.
    async fn write(&self, user_id: &str, pending: Pending) -> SaveOutcome {
        let row = rows::Row {
            variant: pending.variant.clone(),
            payload: pending.payload.clone(),
            built_at: pending.built_at,
        };
        let saved = rows::save(
            &self.db,
            user_id,
            self.kind,
            row,
            pending.generation,
            self.forgets.clone(),
        )
        .await;
        match saved {
            Ok(written) => {
                let mut flights = self.flights();
                if flights
                    .pending
                    .get(user_id)
                    .is_some_and(|held| held.built_at <= pending.built_at)
                {
                    flights.pending.remove(user_id);
                }
                if written {
                    SaveOutcome::Written
                } else {
                    SaveOutcome::Dropped
                }
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
                SaveOutcome::Held
            }
            Err(error) => {
                tracing::warn!(%error, page = self.kind.as_str(), "page not saved");
                self.flights().pending.remove(user_id);
                SaveOutcome::Failed
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
    generation: Generation,
    finished: bool,
}

impl Claim {
    /// What the build was claimed under, which every page it keeps is
    /// saved under.
    pub fn generation(&self) -> Generation {
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
    /// The `kind` pages in `db`, aged by `clock`, kept from builds that
    /// started before a person's forget by `forgets`.
    pub fn new(
        db: Db,
        clock: Arc<dyn Clock>,
        kind: PageKind,
        timing: Timing,
        forgets: ForgetCounts,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                clock,
                kind,
                timing,
                forgets,
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
            if self.inner.forgets.holds(user_id, held.generation) {
                self.inner.write(user_id, held).await;
            } else {
                // Built before the person forgot their history.
                self.inner.flights().pending.remove(user_id);
            }
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
        if !fits(&variant, wanted) {
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

    /// What a build of the user's page started now saves under, for a
    /// build outside [`Self::claim`]. `None`, with a log line, when the
    /// clear generation cannot be read.
    pub async fn generation(&self, user_id: &str) -> Option<Generation> {
        let forgets = self.inner.forgets.of(user_id);
        match rows::generation(&self.inner.db).await {
            Ok(clear) => {
                self.inner.note_generation(clear);
                Some(Generation { clear, forgets })
            }
            Err(error) => {
                tracing::warn!(%error, page = self.inner.kind.as_str(), "page generation unreadable");
                None
            }
        }
    }

    /// Keep `value` as the user's page, built now for `variant` under
    /// `generation`. A busy database holds it in memory until a later
    /// visit writes it.
    pub async fn save(
        &self,
        user_id: &str,
        variant: &Variant,
        value: &T,
        generation: Generation,
    ) -> SaveOutcome {
        let payload = match serde_json::to_vec(value) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(%error, page = self.inner.kind.as_str(), "page does not encode");
                return SaveOutcome::Failed;
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

    /// Claim the user's one build under the generation now; `None`
    /// while another runs, within the retry wait after a failed build
    /// (the warm cycle claims without asking [`Self::due`]; a refresh drops
    /// the wait first), or when the generation cannot be read.
    pub async fn claim(&self, user_id: &str) -> Option<Claim> {
        let generation = self.generation(user_id).await?;
        let now = self.now_ms();
        let retry_ms = self.inner.timing.retry_ms;
        let mut flights = self.inner.flights();
        if flights.running.contains_key(user_id)
            || flights
                .failed
                .get(user_id)
                .is_some_and(|at| now - at < retry_ms)
        {
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
    /// the cache was cleared or the person forgot their history since the
    /// claim (no retry wait: the next visit builds); `None` (failed, empty,
    /// or built for a source that changed meanwhile) keeps the old page and
    /// starts the retry wait. True when a page was kept or held.
    pub async fn run(
        &self,
        claim: Claim,
        variant: &Variant,
        build: impl Future<Output = Option<T>>,
    ) -> bool {
        let outcome = match build.await {
            Some(page) => {
                self.save(&claim.user_id, variant, &page, claim.generation)
                    .await
            }
            None => SaveOutcome::Failed,
        };
        claim.finish(outcome != SaveOutcome::Failed);
        outcome.kept()
    }
}

/// Drop every saved page of every user (the admin's "Clear provider
/// data") and move to the next clear generation, so a build already
/// running keeps nothing. Each page is built again on its next visit or
/// warm pass. Returns how many went.
pub async fn clear_all(db: &Db) -> Result<usize, DbError> {
    rows::clear_all(db).await
}

/// Drop every saved page of one user (their "Forget my history"). Their
/// forget count moves on inside the same write, so every build of theirs
/// claimed before it commits keeps nothing; other people's builds and
/// pages are left alone. Returns how many pages went.
pub async fn forget_user(db: &Db, forgets: &ForgetCounts, user_id: &str) -> Result<usize, DbError> {
    rows::forget_user(db, user_id, forgets.clone()).await
}

/// One user's saved page as stored, undecoded.
#[derive(Debug, Clone)]
pub struct StoredPage {
    /// The user's id.
    pub user_id: String,
    /// The page as saved.
    pub payload: Vec<u8>,
    /// When it was built, unix milliseconds.
    pub built_at: i64,
}

/// Every user's saved page of kind `kind`, by user.
pub async fn stored(db: &Db, kind: PageKind) -> Result<Vec<StoredPage>, DbError> {
    rows::of_kind(db, kind).await
}
