//! Direct-stream concurrency leases: a fair gate ported from v2
//! `StreamConcurrencyService`.
//!
//! One gate bounds direct reads (32 global slots, 8 per principal); the
//! transcode pool lives in [`transcode::LocalTranscodeGate`] with the v2
//! transcode defaults, and the two pools never starve each other. Acquirers
//! queue fairly (first-eligible scan, like v2): at most 128 waiters, each
//! waiting at most 5s before the gateway answers 429 + `Retry-After`.
//!
//! [`DirectLease`] releases its slot exactly once, on drop, so cancellation
//! cannot leak a slot. State lives behind a std mutex (short critical
//! sections only) so release stays synchronous and `Drop`-safe.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::timeout;

/// Global direct-read slots (v2 `direct_global_limit`).
pub const DIRECT_GLOBAL_LIMIT: usize = 32;
/// Direct-read slots per principal (v2 `direct_principal_limit`).
pub const DIRECT_PRINCIPAL_LIMIT: usize = 8;
/// Bounded waiter queue (v2 `max_waiters`).
pub const DIRECT_MAX_WAITERS: usize = 128;
/// Per-acquire wait deadline (v2 `wait_timeout_seconds`).
pub const DIRECT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// The gate is full or the wait deadline expired. The gateway answers 429.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityExhausted;

/// One waiter identity in the fair queue.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Waiter {
    id: u64,
    principal: String,
}

/// Gate counters. Every field is touched only under the gate lock.
#[derive(Debug, Default)]
struct GateState {
    active: usize,
    active_by_principal: HashMap<String, usize>,
    waiters: VecDeque<Waiter>,
    next_waiter_id: u64,
}

/// Fair direct-read gate. Clone shares one gate (wrap in `Arc`).
#[derive(Debug)]
pub struct DirectGate {
    state: Mutex<GateState>,
    changed: Notify,
    global_limit: usize,
    principal_limit: usize,
    max_waiters: usize,
    wait_timeout: Duration,
}

impl DirectGate {
    /// Gate with the v2 direct-pool defaults.
    pub fn new() -> Self {
        Self::with_limits(
            DIRECT_GLOBAL_LIMIT,
            DIRECT_PRINCIPAL_LIMIT,
            DIRECT_MAX_WAITERS,
            DIRECT_WAIT_TIMEOUT,
        )
    }

    /// Gate with explicit limits, for briefs that need tiny pools.
    pub fn with_limits(
        global_limit: usize,
        principal_limit: usize,
        max_waiters: usize,
        wait_timeout: Duration,
    ) -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            changed: Notify::new(),
            global_limit,
            principal_limit,
            max_waiters,
            wait_timeout,
        }
    }

    /// Take a direct slot for one principal, waiting fairly before giving up
    /// with [`CapacityExhausted`].
    pub async fn acquire(&self, principal: &str) -> Result<DirectLease<'_>, CapacityExhausted> {
        let id = {
            let mut state = self.lock();
            if state.waiters.len() >= self.max_waiters {
                return Err(CapacityExhausted);
            }
            let id = state.next_waiter_id;
            state.next_waiter_id += 1;
            state.waiters.push_back(Waiter {
                id,
                principal: principal.to_owned(),
            });
            id
        };
        let _cancel_guard = CancelGuard {
            gate: self,
            id,
            disarmed: false,
        };
        let outcome = timeout(self.wait_timeout, async {
            loop {
                {
                    let state = self.lock();
                    if self.is_eligible(&state, id) {
                        break;
                    }
                }
                self.changed.notified().await;
            }
        })
        .await;
        let mut guard = _cancel_guard;
        guard.disarmed = true;
        let mut state = self.lock();
        match outcome {
            Ok(()) => {
                state.waiters.retain(|waiter| waiter.id != id);
                state.active += 1;
                *state
                    .active_by_principal
                    .entry(principal.to_owned())
                    .or_insert(0) += 1;
                self.changed.notify_waiters();
                Ok(DirectLease {
                    gate: self,
                    principal: principal.to_owned(),
                })
            }
            Err(_) => {
                state.waiters.retain(|waiter| waiter.id != id);
                self.changed.notify_waiters();
                Err(CapacityExhausted)
            }
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
    /// free, its own principal has room, and no earlier waiter is eligible
    /// ahead of it.
    fn is_eligible(&self, state: &GateState, id: u64) -> bool {
        if state.active >= self.global_limit {
            return false;
        }
        let has_room = |principal: &str| {
            state
                .active_by_principal
                .get(principal)
                .copied()
                .unwrap_or(0)
                < self.principal_limit
        };
        for waiter in &state.waiters {
            if waiter.id == id {
                return has_room(&waiter.principal);
            }
            if has_room(&waiter.principal) {
                return false;
            }
        }
        false
    }

    /// Free one slot. Called exactly once per lease, from `Drop`.
    fn release(&self, principal: &str) {
        let mut state = self.lock();
        let count = state
            .active_by_principal
            .get(principal)
            .copied()
            .unwrap_or(0);
        if count > 1 {
            state
                .active_by_principal
                .insert(principal.to_owned(), count - 1);
        } else {
            state.active_by_principal.remove(principal);
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

impl Default for DirectGate {
    fn default() -> Self {
        Self::new()
    }
}

/// One held direct slot. Dropping releases the slot exactly once, so
/// cancellation and early returns cannot leak it.
#[derive(Debug)]
pub struct DirectLease<'a> {
    gate: &'a DirectGate,
    principal: String,
}

impl Drop for DirectLease<'_> {
    fn drop(&mut self) {
        self.gate.release(&self.principal);
    }
}

/// Removes a queued waiter when its acquire is cancelled. Timeouts take the
/// normal path (the guard is disarmed first); only a dropped future — which
/// skips the code below the wait — triggers the guard.
#[derive(Debug)]
struct CancelGuard<'a> {
    gate: &'a DirectGate,
    id: u64,
    disarmed: bool,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let mut state = self.gate.lock();
        state.waiters.retain(|waiter| waiter.id != self.id);
        self.gate.changed.notify_waiters();
    }
}
