//! Token-bucket rate limiters with priority waiters.
//!
//! Each row of the rate table ([`RATES`](crate::rates::RATES)) gets one
//! [`RateLimiter`]. The bucket refills continuously at `per_second` tokens
//! up to `burst`, and waiters queue by [`RequestPriority`]: a user-facing
//! lookup admitted while a background sync waits jumps ahead of it.
//! Background jobs pass the background priority explicitly at the call
//! site; nothing here guesses it.
//!
//! A limiter also carries what its provider said about load: a cooldown
//! (`Retry-After`, a run of failures) during which nothing is granted, the
//! run of failed calls behind that cooldown, and, for rows that document a
//! concurrency limit, how many requests are in flight. It also counts the
//! calls sent through it ([`CallTally`]). The rates themselves live only
//! in the table.

use std::{
    cmp::Ordering,
    collections::BinaryHeap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::Duration,
};

use serde::Serialize;
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::calls::{CallCount, CallOutcome, CallTally};
use crate::rates::{RATES, Rate, RateRow};

/// Floating-point slack for token comparisons.
const EPSILON: f64 = 1e-9;

/// How long a waiter blocked only by the in-flight cap sleeps before it
/// looks again. A finished request wakes it sooner.
const IN_FLIGHT_RECHECK: Duration = Duration::from_secs(1);

/// How urgent a provider call is. Lower wins ties in the rate limiter.
/// There is no default: every caller names its priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestPriority {
    /// A request the user is waiting on right now.
    UserInitiated = 0,
    /// Cover-art fetch: user-visible but degradable to a placeholder.
    ImageFetch = 1,
    /// Speculative fetch for content about to scroll into view.
    PrefetchVisible = 2,
    /// Explicit background work: syncs, imports, refreshes. Background jobs
    /// pass this level (or `Opportunistic`) at the call site.
    BackgroundSync = 3,
    /// Nice-to-have work that yields to everything else.
    Opportunistic = 4,
}

impl RequestPriority {
    /// Whether someone is looking at the page this call serves.
    #[must_use]
    pub const fn is_user_facing(self) -> bool {
        matches!(self, Self::UserInitiated | Self::ImageFetch)
    }
}

/// One bucket's shape: sustained rate plus burst capacity. A rate of zero
/// or less means unpaced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RatePolicy {
    /// Sustained tokens per second.
    pub per_second: f64,
    /// Bucket capacity: how many tokens may bunch up at once.
    pub burst: u32,
}

impl RatePolicy {
    /// One bucket shape.
    #[must_use]
    pub const fn new(per_second: f64, burst: u32) -> Self {
        Self { per_second, burst }
    }

    fn paced(&self) -> bool {
        self.per_second.is_finite() && self.per_second > 0.0
    }
}

/// Acquiring more tokens than the bucket holds would wait forever.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("cannot acquire {tokens} tokens (capacity: {burst}); the request would wait forever")]
pub struct OverCapacity {
    /// Tokens the caller asked for.
    pub tokens: u32,
    /// The bucket's burst capacity.
    pub burst: u32,
}

/// Why a bounded wait for a request ended without one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refused {
    /// The bucket holds no tokens at all.
    #[error(transparent)]
    OverCapacity(#[from] OverCapacity),
    /// The caller's turn would come later than it can wait.
    #[error("no request free for {wait:?}")]
    TooLong {
        /// About how long the turn would take.
        wait: Duration,
    },
}

/// One queued waiter. Orders as a min-heap on (priority, sequence) so the
/// most urgent, longest-waiting caller grants first.
#[derive(Debug)]
struct Waiter {
    priority: u8,
    sequence: u64,
    tokens: u32,
    /// Whether the grant also takes an in-flight slot.
    slot: bool,
    grant: Option<oneshot::Sender<()>>,
    cancelled: Arc<AtomicBool>,
}

impl PartialEq for Waiter {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
    }
}

impl Eq for Waiter {}

impl PartialOrd for Waiter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Waiter {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: BinaryHeap is a max-heap, we want the smallest first.
        (other.priority, other.sequence).cmp(&(self.priority, self.sequence))
    }
}

#[derive(Debug)]
struct BucketState {
    policy: RatePolicy,
    max_in_flight: Option<u32>,
    tokens: f64,
    updated: Instant,
    paused_until: Option<Instant>,
    in_flight: u32,
    failure_run: u32,
    headerless_streak: u32,
    waiters: BinaryHeap<Waiter>,
    sequence: u64,
}

impl BucketState {
    fn refresh(&mut self) {
        let now = Instant::now();
        if let Some(until) = self.paused_until {
            if now < until {
                self.updated = now;
                return;
            }
            // The pause is over: refill from empty, starting when it ended.
            self.paused_until = None;
            self.updated = until;
        }
        if self.policy.paced() {
            let elapsed = now.duration_since(self.updated).as_secs_f64();
            self.tokens =
                f64::from(self.policy.burst).min(self.tokens + elapsed * self.policy.per_second);
        }
        self.updated = now;
    }

    fn paused(&self) -> bool {
        self.paused_until
            .is_some_and(|until| Instant::now() < until)
    }

    fn has_tokens(&self, tokens: u32) -> bool {
        !self.policy.paced() || self.tokens + EPSILON >= f64::from(tokens)
    }

    fn has_slot(&self, slot: bool) -> bool {
        !slot || self.max_in_flight.is_none_or(|max| self.in_flight < max)
    }

    fn take(&mut self, tokens: u32, slot: bool) {
        if self.policy.paced() {
            self.tokens -= f64::from(tokens);
        }
        if slot {
            self.in_flight += 1;
        }
    }

    /// Grant every grantable head waiter in priority order, dropping
    /// cancelled entries.
    fn grant_locked(&mut self) {
        if self.paused() {
            return;
        }
        while let Some(mut head) = self.waiters.pop() {
            if head.cancelled.load(AtomicOrdering::Acquire) {
                continue;
            }
            if !self.has_tokens(head.tokens) || !self.has_slot(head.slot) {
                self.waiters.push(head);
                return;
            }
            let Some(grant) = head.grant.take() else {
                continue;
            };
            // A dropped receiver means the waiter went away; keep its tokens.
            if grant.send(()).is_err() {
                continue;
            }
            self.take(head.tokens, head.slot);
        }
    }

    /// How long a new waiter at `priority` asking for `tokens` would wait:
    /// the rest of any cooldown, then the refill that covers every live
    /// waiter at the same or a higher priority (they go first) and itself.
    /// Lower-priority waiters do not count, since it goes ahead of them. A
    /// wait for an in-flight slot cannot be known and is not counted; the
    /// deadline in [`RateLimiter::acquire_slot_within`] covers it.
    fn turn_wait(&self, tokens: u32, priority: RequestPriority) -> Duration {
        let pause = self.paused_until.map_or(Duration::ZERO, |until| {
            until.saturating_duration_since(Instant::now())
        });
        if !self.policy.paced() {
            return pause;
        }
        let ahead: u32 = self
            .waiters
            .iter()
            .filter(|waiter| {
                waiter.priority <= priority as u8 && !waiter.cancelled.load(AtomicOrdering::Acquire)
            })
            .map(|waiter| waiter.tokens)
            .sum();
        let deficit = (f64::from(ahead.saturating_add(tokens)) - self.tokens).max(0.0);
        pause + Duration::from_secs_f64(deficit / self.policy.per_second)
    }

    fn head_wait(&self) -> Duration {
        let floor = Duration::from_millis(1);
        if let Some(until) = self.paused_until {
            let now = Instant::now();
            if now < until {
                return until.duration_since(now).max(floor);
            }
        }
        let Some(head) = self.waiters.peek() else {
            return floor;
        };
        if !self.has_tokens(head.tokens) {
            let deficit = f64::from(head.tokens) - self.tokens;
            let wait = Duration::from_secs_f64((deficit / self.policy.per_second).max(0.0));
            return wait.max(floor);
        }
        if !self.has_slot(head.slot) {
            return IN_FLIGHT_RECHECK;
        }
        floor
    }
}

/// Marks the queued waiter cancelled if the `acquire` future drops first,
/// so a cancelled caller never consumes a later grant. A slot grant that
/// arrived but was never collected is handed back.
struct Pending<'a> {
    limiter: &'a RateLimiter,
    flag: Arc<AtomicBool>,
    notified: Option<oneshot::Receiver<()>>,
    slot: bool,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.flag.store(true, AtomicOrdering::Release);
        if self.slot
            && let Some(mut notified) = self.notified.take()
            && notified.try_recv().is_ok()
        {
            self.limiter.release_slot();
        }
    }
}

/// An in-flight slot, held from the grant until the answer has been read.
/// Dropping it lets the next waiter in.
#[derive(Debug)]
pub struct InFlight<'a> {
    limiter: &'a RateLimiter,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.limiter.release_slot();
    }
}

/// A token-bucket limiter with priority-ordered waiters.
///
/// Cheap to share (`&self` methods only). The mutex is a plain std mutex
/// held only across microsecond bookkeeping, never across an await.
#[derive(Debug)]
pub struct RateLimiter {
    state: Mutex<BucketState>,
    calls: CallTally,
}

impl RateLimiter {
    /// Build a full bucket of one shape.
    #[must_use]
    pub fn new(policy: RatePolicy) -> Self {
        Self::shaped(policy, None)
    }

    /// Build the bucket of one table row. An operator-set rate starts
    /// unpaced until the first request hands it over with
    /// [`retune`](Self::retune).
    #[must_use]
    pub fn from_row(row: &RateRow) -> Self {
        let per_second = match row.per_second {
            Rate::Fixed(per_second) => per_second,
            Rate::Setting => 0.0,
        };
        Self::shaped(RatePolicy::new(per_second, row.burst), row.max_in_flight)
    }

    fn shaped(policy: RatePolicy, max_in_flight: Option<u32>) -> Self {
        Self {
            state: Mutex::new(BucketState {
                policy,
                max_in_flight,
                tokens: f64::from(policy.burst),
                updated: Instant::now(),
                paused_until: None,
                in_flight: 0,
                failure_run: 0,
                headerless_streak: 0,
                waiters: BinaryHeap::new(),
                sequence: 0,
            }),
            calls: CallTally::new(),
        }
    }

    /// The shape this limiter enforces now.
    #[must_use]
    pub fn policy(&self) -> RatePolicy {
        self.lock().policy
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BucketState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Wait for one token and one in-flight slot at `priority`. The slot is
    /// held until the returned guard drops. Rows without a concurrency
    /// limit hand out slots freely.
    ///
    /// # Errors
    ///
    /// Returns [`OverCapacity`] when the bucket holds no tokens at all.
    pub async fn acquire_slot(
        &self,
        priority: RequestPriority,
    ) -> Result<InFlight<'_>, OverCapacity> {
        match self.wait_for(1, priority, true, None).await {
            Ok(()) => Ok(InFlight { limiter: self }),
            Err(Refused::OverCapacity(error)) => Err(error),
            // An unbounded wait never gives up.
            Err(Refused::TooLong { .. }) => Err(OverCapacity {
                tokens: 1,
                burst: self.policy().burst,
            }),
        }
    }

    /// Like [`acquire_slot`](Self::acquire_slot), for a caller that can wait
    /// at most `within`. A turn that would come later, counting the bucket
    /// and the waiters ahead at the same or a higher priority, is refused
    /// at once without queueing. A wait that runs past `within` anyway (a
    /// more urgent caller arrived, or the in-flight cap held) gives up then.
    ///
    /// # Errors
    ///
    /// [`Refused::TooLong`] with about how long the turn would take, or
    /// [`Refused::OverCapacity`] when the bucket holds no tokens at all.
    pub async fn acquire_slot_within(
        &self,
        priority: RequestPriority,
        within: Duration,
    ) -> Result<InFlight<'_>, Refused> {
        self.wait_for(1, priority, true, Some(within)).await?;
        Ok(InFlight { limiter: self })
    }

    async fn wait_for(
        &self,
        tokens: u32,
        priority: RequestPriority,
        slot: bool,
        within: Option<Duration>,
    ) -> Result<(), Refused> {
        let (grant, notified) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.lock();
            if state.policy.paced() && tokens > state.policy.burst {
                return Err(Refused::OverCapacity(OverCapacity {
                    tokens,
                    burst: state.policy.burst,
                }));
            }
            state.refresh();
            if state.waiters.is_empty()
                && !state.paused()
                && state.has_tokens(tokens)
                && state.has_slot(slot)
            {
                state.take(tokens, slot);
                return Ok(());
            }
            if let Some(within) = within {
                let wait = state.turn_wait(tokens, priority);
                if wait > within {
                    return Err(Refused::TooLong { wait });
                }
            }
            let sequence = state.sequence;
            state.sequence += 1;
            state.waiters.push(Waiter {
                priority: priority as u8,
                sequence,
                tokens,
                slot,
                grant: Some(grant),
                cancelled: Arc::clone(&cancelled),
            });
            state.grant_locked();
        }
        let deadline = within.map(|within| Instant::now() + within);
        let mut pending = Pending {
            limiter: self,
            flag: cancelled,
            notified: Some(notified),
            slot,
        };
        loop {
            let wait = {
                let mut state = self.lock();
                state.refresh();
                state.grant_locked();
                state.head_wait()
            };
            let Some(notified) = pending.notified.as_mut() else {
                return Ok(());
            };
            // A latched grant is never lost, so check before parking.
            if notified.try_recv().is_ok() {
                pending.notified = None;
                return Ok(());
            }
            let wait = match deadline {
                Some(deadline) if Instant::now() >= deadline => {
                    // Grants are sent under the lock, so with it held a
                    // grant either arrived already, and is used, or never
                    // comes: no token is lost.
                    let state = self.lock();
                    pending.flag.store(true, AtomicOrdering::Release);
                    if notified.try_recv().is_ok() {
                        drop(state);
                        pending.notified = None;
                        return Ok(());
                    }
                    let wait = state.turn_wait(tokens, priority);
                    drop(state);
                    return Err(Refused::TooLong {
                        wait: wait.max(IN_FLIGHT_RECHECK),
                    });
                }
                Some(deadline) => wait.min(deadline - Instant::now()),
                None => wait,
            };
            tokio::select! {
                biased;
                _ = notified => {
                    pending.notified = None;
                    return Ok(());
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    fn release_slot(&self) {
        let mut state = self.lock();
        state.in_flight = state.in_flight.saturating_sub(1);
        state.refresh();
        state.grant_locked();
    }

    /// Whole tokens currently available.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        let mut state = self.lock();
        state.refresh();
        state.tokens.max(0.0) as u32
    }

    /// How long until `tokens` are available, rounded up to whole seconds.
    /// Zero when the bucket already covers the request.
    #[must_use]
    pub fn retry_after(&self, tokens: u32) -> Duration {
        let mut state = self.lock();
        state.refresh();
        let pause = state
            .paused_until
            .map_or(Duration::ZERO, |until| until.duration_since(Instant::now()));
        if pause.is_zero() && state.has_tokens(tokens) {
            return Duration::ZERO;
        }
        let deficit = (f64::from(tokens) - state.tokens).max(0.0);
        let refill = if state.policy.paced() {
            deficit / state.policy.per_second
        } else {
            0.0
        };
        Duration::from_secs_f64((pause.as_secs_f64() + refill).ceil())
    }

    /// Grant nothing for `delay`, then refill from empty, so a provider
    /// that said "slow down" never gets a burst the moment it ends. A
    /// longer cooldown already running is kept.
    pub fn cool_down(&self, delay: Duration) {
        let mut state = self.lock();
        state.refresh();
        let until = Instant::now() + delay;
        if state.paused_until.is_none_or(|current| current < until) {
            state.paused_until = Some(until);
        }
        state.tokens = 0.0;
    }

    /// How much of a cooldown is left; zero when none runs.
    #[must_use]
    pub fn cooling_for(&self) -> Duration {
        self.lock().paused_until.map_or(Duration::ZERO, |until| {
            until.saturating_duration_since(Instant::now())
        })
    }

    /// Change the rate in place for an operator-set row. Tokens are kept,
    /// capped at the burst; zero or less makes the row unpaced.
    pub(crate) fn retune(&self, per_second: f64) {
        let mut state = self.lock();
        if state.policy.per_second == per_second {
            return;
        }
        state.refresh();
        state.policy.per_second = per_second;
        state.tokens = state.tokens.min(f64::from(state.policy.burst));
        state.grant_locked();
    }

    /// Count one failed call in the row's run and return the run's length.
    pub fn note_failure(&self) -> u32 {
        let mut state = self.lock();
        state.failure_run = state.failure_run.saturating_add(1);
        state.failure_run
    }

    /// How many failed calls in a row the row has seen.
    #[must_use]
    pub fn failure_run(&self) -> u32 {
        self.lock().failure_run
    }

    /// The provider answered: its run of failures starts over.
    pub fn note_answer(&self) {
        self.lock().failure_run = 0;
    }

    /// Count one 429 that named no wait and return how many came in a row.
    pub fn note_headerless_limit(&self) -> u32 {
        let mut state = self.lock();
        state.headerless_streak = state.headerless_streak.saturating_add(1);
        state.headerless_streak
    }

    /// A clean answer ends the run of 429s that named no wait.
    pub fn clear_headerless_limits(&self) {
        self.lock().headerless_streak = 0;
    }

    /// Count one call sent through this row.
    pub fn record_call(&self, priority: RequestPriority, outcome: CallOutcome, body_bytes: u64) {
        self.calls.record(priority, outcome, body_bytes);
    }

    /// The calls sent through this row since the server started.
    #[must_use]
    pub fn calls(&self) -> Vec<CallCount> {
        self.calls.counts()
    }
}

/// The limiters of every table row, built once and shared.
#[derive(Debug)]
pub struct LimiterSet {
    limiters: Vec<(&'static str, Arc<RateLimiter>)>,
}

impl LimiterSet {
    /// One limiter per row of [`RATES`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            limiters: RATES
                .iter()
                .map(|row| (row.source, Arc::new(RateLimiter::from_row(row))))
                .collect(),
        }
    }

    /// One limiter per row, none of them paced. Tests only: production
    /// always paces through [`LimiterSet::new`].
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn unpaced() -> Self {
        Self {
            limiters: RATES
                .iter()
                .map(|row| {
                    let unpaced = RateRow {
                        per_second: Rate::Fixed(0.0),
                        ..*row
                    };
                    (row.source, Arc::new(RateLimiter::from_row(&unpaced)))
                })
                .collect(),
        }
    }

    /// One row's limiter, or `None` for providers without a row.
    #[must_use]
    pub fn limiter(&self, source: &str) -> Option<&RateLimiter> {
        self.shared(source).map(AsRef::as_ref)
    }

    /// One row's limiter as a shared handle, for the executor that paces
    /// through it.
    #[must_use]
    pub fn shared(&self, source: &str) -> Option<&Arc<RateLimiter>> {
        self.limiters
            .iter()
            .find(|(name, _)| *name == source)
            .map(|(_, limiter)| limiter)
    }

    /// The limiter an executor paces through for `row`. A row missing from
    /// the table is logged and paced at one request a second, never left
    /// unpaced. Debug builds stop here instead, so building the providers
    /// in a test proves every executor names a row.
    #[must_use]
    pub fn for_executor(&self, row: &str) -> Arc<RateLimiter> {
        debug_assert!(
            self.shared(row).is_some(),
            "an executor names rate row {row:?}, which the table lacks"
        );
        self.shared(row).cloned().unwrap_or_else(|| {
            tracing::error!(row, "no rate row; pacing at one request a second");
            Arc::new(RateLimiter::new(RatePolicy::new(1.0, 1)))
        })
    }

    /// Lowercase names of every row, in table order.
    #[must_use]
    pub fn sources(&self) -> Vec<&'static str> {
        self.limiters.iter().map(|(name, _)| *name).collect()
    }
}

impl Default for LimiterSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_row_that_holds_no_tokens_fails_instead_of_waiting_forever() {
        let limiter = RateLimiter::new(RatePolicy::new(1.0, 0));
        let error = limiter
            .acquire_slot(RequestPriority::UserInitiated)
            .await
            .expect_err("a burst of 0 never grants");
        assert_eq!(
            error,
            OverCapacity {
                tokens: 1,
                burst: 0
            }
        );
    }

    #[tokio::test]
    async fn user_waiter_jumps_ahead_of_background_waiter() {
        use tokio::sync::Mutex as AsyncMutex;

        let limiter = Arc::new(RateLimiter::new(RatePolicy::new(1.0, 1)));
        drop(
            limiter
                .acquire_slot(RequestPriority::UserInitiated)
                .await
                .expect("first token is free"),
        );
        assert_eq!(limiter.remaining(), 0);

        let order = Arc::new(AsyncMutex::new(Vec::new()));
        let background = {
            let limiter = Arc::clone(&limiter);
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                drop(
                    limiter
                        .acquire_slot(RequestPriority::BackgroundSync)
                        .await
                        .expect("background acquires"),
                );
                order.lock().await.push("background");
            })
        };
        // Let the background caller queue first.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let user = {
            let limiter = Arc::clone(&limiter);
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                drop(
                    limiter
                        .acquire_slot(RequestPriority::UserInitiated)
                        .await
                        .expect("user acquires"),
                );
                order.lock().await.push("user");
            })
        };
        background.await.expect("background joins");
        user.await.expect("user joins");
        assert_eq!(*order.lock().await, ["user", "background"]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_rounds_up_to_whole_seconds() {
        let limiter = RateLimiter::new(RatePolicy::new(0.5, 2));
        assert_eq!(limiter.retry_after(1), Duration::ZERO);
        for _ in 0..2 {
            drop(
                limiter
                    .acquire_slot(RequestPriority::UserInitiated)
                    .await
                    .expect("burst token"),
            );
        }
        // 0.5/s with an empty bucket: one token needs 2s.
        assert_eq!(limiter.retry_after(1), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn the_in_flight_cap_holds_until_the_slot_is_released() {
        let row = RateRow {
            source: "test",
            per_second: Rate::Fixed(100.0),
            burst: 5,
            max_in_flight: Some(1),
            verified_on: "2026-10-20",
            basis: "test",
            url: "",
        };
        let limiter = Arc::new(RateLimiter::from_row(&row));
        let first = limiter
            .acquire_slot(RequestPriority::UserInitiated)
            .await
            .expect("first slot");
        let second = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move {
                let _slot = limiter
                    .acquire_slot(RequestPriority::UserInitiated)
                    .await
                    .expect("second slot");
                Instant::now()
            })
        };
        tokio::time::sleep(Duration::from_secs(3)).await;
        let released = Instant::now();
        drop(first);
        let granted = second.await.expect("joins");
        assert!(granted >= released, "the second waits for the first");
    }
}
