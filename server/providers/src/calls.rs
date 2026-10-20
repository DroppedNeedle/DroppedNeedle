//! Outbound provider calls counted since the server started.
//!
//! Each rate row's limiter keeps one [`CallTally`]. The executor records
//! every request it puts on the wire there, retries included, by the
//! caller's priority and the class of what came back, with the body bytes
//! read. A request turned down before it was sent is not a call. The
//! counts live in memory and start over with the process.

use std::sync::Mutex;

use serde::Serialize;
use tokio::time::Instant;

use crate::error::ProviderError;
use crate::limiter::RequestPriority;

/// The window behind the calls-per-minute figure, in one-second buckets.
const WINDOW_SECS: usize = 60;

/// What one call came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    /// An answer the provider's rules accept.
    Ok,
    /// The provider has no such thing (404).
    NotFound,
    /// The provider asked us to slow down.
    RateLimited,
    /// Any other failure: a refusal, a server error, a broken or oversized
    /// answer, or no answer at all.
    Failed,
}

impl CallOutcome {
    /// The class of one attempt's result.
    #[must_use]
    pub fn of<T>(result: &Result<T, ProviderError>) -> Self {
        match result {
            Ok(_) => Self::Ok,
            Err(ProviderError::NotFound { .. }) => Self::NotFound,
            Err(ProviderError::RateLimited { .. }) => Self::RateLimited,
            Err(_) => Self::Failed,
        }
    }
}

/// The calls of one priority and outcome on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallCount {
    /// The caller's priority.
    pub priority: RequestPriority,
    /// What came back.
    pub outcome: CallOutcome,
    /// Calls since the server started.
    pub calls: u64,
    /// Calls in the last minute.
    pub last_minute: u64,
    /// Answer body bytes read since the server started.
    pub body_bytes: u64,
}

#[derive(Debug)]
struct Entry {
    priority: RequestPriority,
    outcome: CallOutcome,
    calls: u64,
    body_bytes: u64,
    /// Calls per second for the last minute: the second each slot holds,
    /// counted from the tally's start, and its count.
    recent: [(u64, u32); WINDOW_SECS],
}

/// One row's call counts. A short lock per call, never held across an
/// await.
#[derive(Debug)]
pub struct CallTally {
    started: Instant,
    entries: Mutex<Vec<Entry>>,
}

impl Default for CallTally {
    fn default() -> Self {
        Self::new()
    }
}

impl CallTally {
    /// Nothing counted yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            entries: Mutex::new(Vec::new()),
        }
    }

    fn second(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Count one call sent at `priority` that ended in `outcome` after
    /// reading `body_bytes`.
    pub fn record(&self, priority: RequestPriority, outcome: CallOutcome, body_bytes: u64) {
        let second = self.second();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let index = match entries
            .iter()
            .position(|entry| entry.priority == priority && entry.outcome == outcome)
        {
            Some(index) => index,
            None => {
                entries.push(Entry {
                    priority,
                    outcome,
                    calls: 0,
                    body_bytes: 0,
                    recent: [(0, 0); WINDOW_SECS],
                });
                entries.len() - 1
            }
        };
        let entry = &mut entries[index];
        entry.calls = entry.calls.saturating_add(1);
        entry.body_bytes = entry.body_bytes.saturating_add(body_bytes);
        let slot = &mut entry.recent[(second % WINDOW_SECS as u64) as usize];
        if slot.0 == second {
            slot.1 = slot.1.saturating_add(1);
        } else {
            *slot = (second, 1);
        }
    }

    /// Every priority and outcome counted so far.
    #[must_use]
    pub fn counts(&self) -> Vec<CallCount> {
        let second = self.second();
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        entries
            .iter()
            .map(|entry| CallCount {
                priority: entry.priority,
                outcome: entry.outcome,
                calls: entry.calls,
                last_minute: entry
                    .recent
                    .iter()
                    .filter(|(at, count)| {
                        *count > 0 && second.saturating_sub(*at) < WINDOW_SECS as u64
                    })
                    .map(|(_, count)| u64::from(*count))
                    .sum(),
                body_bytes: entry.body_bytes,
            })
            .collect()
    }
}
