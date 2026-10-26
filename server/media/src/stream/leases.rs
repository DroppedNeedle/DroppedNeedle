//! Stream concurrency: one keyed fair gate, ported from v2
//! `StreamConcurrencyService`, run as two pools.
//!
//! [`DirectGate`] bounds reads (32 global slots, 8 per principal) and
//! [`TranscodeGate`] bounds ffmpeg jobs (one per CPU, at least two, and 2
//! per principal so a gapless player can open the next track early). Both
//! are the same [`Gate`]: acquirers queue fairly (first-eligible scan, like
//! v2), at most 128 waiters, each waiting at most 5s before the gate answers
//! [`CapacityExhausted`], which every router turns into 429 plus
//! `Retry-After`. The pool is a type parameter, so a lease from one pool
//! cannot be passed where the other is meant.
//!
//! A [`Lease`] holds its gate by `Arc` and releases its slot exactly once,
//! on drop, so cancellation and early returns cannot leak it. State lives
//! behind a std mutex (short critical sections only) so release stays
//! synchronous and `Drop`-safe.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::timeout;

/// Global direct-read slots (v2 `direct_global_limit`).
pub const DIRECT_GLOBAL_LIMIT: usize = 32;
/// Direct-read slots per principal (v2 `direct_principal_limit`).
pub const DIRECT_PRINCIPAL_LIMIT: usize = 8;
/// Fewest ffmpeg jobs the server runs at once. The real global limit is
/// one job per CPU, never below this floor (see [`transcode_global_limit`]).
pub const TRANSCODE_GLOBAL_LIMIT: usize = 2;
/// ffmpeg jobs one principal may hold: the playing track plus the next one,
/// which gapless players (ExoPlayer/Media3, just_audio) open early.
pub const TRANSCODE_PRINCIPAL_LIMIT: usize = 2;
/// Bounded waiter queue of either pool (v2 `max_waiters`).
pub const MAX_WAITERS: usize = 128;
/// Per-acquire wait deadline of either pool (v2 `wait_timeout_seconds`).
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Retry hint every router sends with its 429 (v2 sends 1).
pub const RETRY_AFTER_SECONDS: u64 = 1;

/// Global transcode limit for this machine: one ffmpeg job per CPU, at
/// least [`TRANSCODE_GLOBAL_LIMIT`].
pub fn transcode_global_limit() -> usize {
    std::thread::available_parallelism()
        .map_or(TRANSCODE_GLOBAL_LIMIT, usize::from)
        .max(TRANSCODE_GLOBAL_LIMIT)
}

/// The gate is full or the wait deadline expired. Routers answer 429.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityExhausted;

/// The four numbers that shape one gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateLimits {
    /// Slots across every key.
    pub global: usize,
    /// Slots one key (principal) may hold.
    pub per_key: usize,
    /// Acquires that may wait at once; one more is refused at once.
    pub max_waiters: usize,
    /// How long one acquire waits before giving up.
    pub wait: Duration,
}

/// Which pool a gate guards; it sets the production limits.
pub trait Pool: Send + Sync + 'static {
    /// The limits [`Gate::new`] uses.
    fn limits() -> GateLimits;
}

/// The direct-read pool.
#[derive(Debug)]
pub enum Direct {}

impl Pool for Direct {
    fn limits() -> GateLimits {
        GateLimits {
            global: DIRECT_GLOBAL_LIMIT,
            per_key: DIRECT_PRINCIPAL_LIMIT,
            max_waiters: MAX_WAITERS,
            wait: WAIT_TIMEOUT,
        }
    }
}

/// The ffmpeg pool.
#[derive(Debug)]
pub enum Transcode {}

impl Pool for Transcode {
    fn limits() -> GateLimits {
        GateLimits {
            global: transcode_global_limit(),
            per_key: TRANSCODE_PRINCIPAL_LIMIT,
            max_waiters: MAX_WAITERS,
            wait: WAIT_TIMEOUT,
        }
    }
}

/// The direct-read gate.
pub type DirectGate = Gate<Direct>;
/// A held direct-read slot.
pub type OwnedDirectLease = Lease<Direct>;
/// The ffmpeg gate.
pub type TranscodeGate = Gate<Transcode>;
/// A held ffmpeg slot.
pub type TranscodeLease = Lease<Transcode>;

/// One waiter identity in the fair queue.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Waiter {
    id: u64,
    key: String,
}

/// Gate counters. Every field is touched only under the gate lock.
#[derive(Debug, Default)]
struct GateState {
    active: usize,
    active_by_key: HashMap<String, usize>,
    waiters: VecDeque<Waiter>,
    next_waiter_id: u64,
}

/// Keyed fair gate. Share one by `Arc`.
#[derive(Debug)]
pub struct Gate<P: Pool> {
    state: Mutex<GateState>,
    changed: Notify,
    limits: GateLimits,
    pool: PhantomData<P>,
}

impl<P: Pool> Gate<P> {
    /// Gate with its pool's production limits.
    pub fn new() -> Self {
        Self::with_limits(P::limits())
    }

    /// Gate with explicit limits, for tests that need tiny pools.
    pub fn with_limits(limits: GateLimits) -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            changed: Notify::new(),
            limits,
            pool: PhantomData,
        }
    }

    /// Take a slot for one key, waiting fairly before giving up with
    /// [`CapacityExhausted`].
    pub async fn acquire_owned(self: &Arc<Self>, key: &str) -> Result<Lease<P>, CapacityExhausted> {
        self.take(key).await?;
        Ok(Lease {
            gate: Arc::clone(self),
            key: key.to_owned(),
        })
    }

    /// Wait fairly for a slot and count it taken. The caller wraps it in a
    /// lease at once, with no await in between, so the slot cannot leak.
    async fn take(&self, key: &str) -> Result<(), CapacityExhausted> {
        let id = {
            let mut state = self.lock();
            if state.waiters.len() >= self.limits.max_waiters {
                return Err(CapacityExhausted);
            }
            let id = state.next_waiter_id;
            state.next_waiter_id += 1;
            state.waiters.push_back(Waiter {
                id,
                key: key.to_owned(),
            });
            id
        };
        let mut guard = CancelGuard {
            gate: self,
            id,
            disarmed: false,
        };
        let outcome = timeout(self.limits.wait, async {
            loop {
                let changed = self.changed.notified();
                {
                    let state = self.lock();
                    if self.is_eligible(&state, id) {
                        break;
                    }
                }
                changed.await;
            }
        })
        .await;
        guard.disarmed = true;
        let mut state = self.lock();
        state.waiters.retain(|waiter| waiter.id != id);
        self.changed.notify_waiters();
        match outcome {
            Ok(()) => {
                state.active += 1;
                *state.active_by_key.entry(key.to_owned()).or_insert(0) += 1;
                Ok(())
            }
            Err(_) => Err(CapacityExhausted),
        }
    }

    /// Slots currently held.
    pub fn active(&self) -> usize {
        self.lock().active
    }

    /// Waiters currently queued.
    pub fn waiter_count(&self) -> usize {
        self.lock().waiters.len()
    }

    /// True when waiter `id` is the first eligible waiter: a global slot is
    /// free, its own key has room, and no earlier waiter is eligible ahead
    /// of it.
    fn is_eligible(&self, state: &GateState, id: u64) -> bool {
        if state.active >= self.limits.global {
            return false;
        }
        let has_room =
            |key: &str| state.active_by_key.get(key).copied().unwrap_or(0) < self.limits.per_key;
        for waiter in &state.waiters {
            if waiter.id == id {
                return has_room(&waiter.key);
            }
            if has_room(&waiter.key) {
                return false;
            }
        }
        false
    }

    /// Free one slot. Called exactly once per lease, from `Drop`.
    fn release(&self, key: &str) {
        let mut state = self.lock();
        let count = state.active_by_key.get(key).copied().unwrap_or(0);
        if count > 1 {
            state.active_by_key.insert(key.to_owned(), count - 1);
        } else {
            state.active_by_key.remove(key);
        }
        state.active = state.active.saturating_sub(1);
        self.changed.notify_waiters();
    }

    /// Lock the gate state, tolerating a poisoned mutex.
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl<P: Pool> Default for Gate<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// One held slot that owns its gate handle. Dropping releases the slot
/// exactly once.
#[derive(Debug)]
pub struct Lease<P: Pool> {
    gate: Arc<Gate<P>>,
    key: String,
}

impl Lease<Direct> {
    /// Keep this slot held until `chunks` ends or is dropped, so the gate
    /// bounds response bodies in flight, not only opens.
    pub fn hold(self, chunks: super::routes::ChunkStream) -> super::routes::ChunkStream {
        use futures_util::StreamExt as _;
        chunks
            .map(move |chunk| {
                let _held = &self;
                chunk
            })
            .boxed()
    }
}

impl<P: Pool> Drop for Lease<P> {
    fn drop(&mut self) {
        self.gate.release(&self.key);
    }
}

/// Removes a queued waiter when its acquire is cancelled. Timeouts take the
/// normal path (the guard is disarmed first); only a dropped future (which
/// skips the code below the wait) triggers the guard.
struct CancelGuard<'a, P: Pool> {
    gate: &'a Gate<P>,
    id: u64,
    disarmed: bool,
}

impl<P: Pool> Drop for CancelGuard<'_, P> {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let mut state = self.gate.lock();
        state.waiters.retain(|waiter| waiter.id != self.id);
        self.gate.changed.notify_waiters();
    }
}
