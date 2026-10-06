//! Process-wide MusicBrainz pacing.
//!
//! Every MusicBrainz client in the process paces through the one shared
//! limiter set in [`Providers`]: the official service and self-hosted
//! mirrors take the `musicbrainz` row (1 req/s, no burst), BrainzMash takes
//! its own `brainzmash` row (10 req/s). Two clients built from the same
//! [`MbPacing`] can never exceed the policy together, which a private gate
//! per client could not promise.
//!
//! BrainzMash also cools down after a 429: the cooldown honors
//! `Retry-After` or backs off exponentially with jitter, and the state is
//! shared through [`Providers`] the same way (v2 `_BrainzMashScheduler`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::BRAINZMASH_MAX_COOLDOWN_SECS;
use crate::providers::{Providers, RequestPriority};

/// BrainzMash cooldown base (v2 `_BRAINZMASH_COOLDOWN_BASE_SECONDS`).
const BRAINZMASH_COOLDOWN_BASE_SECS: f64 = 1.0;

/// Pacing handle shared by every MusicBrainz client. Clone is cheap.
#[derive(Debug, Clone)]
pub struct MbPacing {
    providers: Arc<Providers>,
}

impl MbPacing {
    /// Pace through the shared provider deps.
    #[must_use]
    pub fn new(providers: Arc<Providers>) -> Self {
        Self { providers }
    }

    /// Wait for one slot: the BrainzMash cooldown first when that source is
    /// active, then one token from the source's limiter at `priority`.
    pub async fn acquire(&self, brainzmash: bool, priority: RequestPriority) {
        let source = if brainzmash {
            let wait = self.cooldown().remaining();
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            "brainzmash"
        } else {
            "musicbrainz"
        };
        if let Some(limiter) = self.providers.limiter(source)
            && let Err(error) = limiter.acquire_with_priority(1, priority).await
        {
            // One token never exceeds a burst of one or more; log rather
            // than send unpaced if the table is ever misconfigured.
            tracing::error!(%error, source, "musicbrainz pacing refused a token");
        }
    }

    /// The shared BrainzMash cooldown.
    #[must_use]
    pub fn cooldown(&self) -> &BrainzMashCooldown {
        &self.providers.brainzmash_cooldown
    }
}

/// BrainzMash cooldown state. Mutexes are never held across an await.
#[derive(Debug, Default)]
pub struct BrainzMashCooldown {
    state: Mutex<CooldownState>,
}

#[derive(Debug, Default)]
struct CooldownState {
    until: Option<tokio::time::Instant>,
    consecutive_no_retry_after: u32,
}

impl BrainzMashCooldown {
    fn lock(&self) -> std::sync::MutexGuard<'_, CooldownState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Record one 429 and return the bounded delay selected for it.
    pub fn note_cooldown(&self, retry_after_secs: Option<f64>) -> f64 {
        let mut state = self.lock();
        let delay = match retry_after_secs {
            Some(seconds) if seconds.is_finite() && seconds >= 0.0 => {
                state.consecutive_no_retry_after = 0;
                seconds.min(BRAINZMASH_MAX_COOLDOWN_SECS)
            }
            _ => {
                state.consecutive_no_retry_after =
                    state.consecutive_no_retry_after.saturating_add(1);
                let exponent = state.consecutive_no_retry_after.saturating_sub(1).min(10);
                let base = (BRAINZMASH_COOLDOWN_BASE_SECS * f64::from(1u32 << exponent))
                    .min(BRAINZMASH_MAX_COOLDOWN_SECS);
                base * (0.5 + 0.5 * jitter())
            }
        };
        state.until = Some(tokio::time::Instant::now() + Duration::from_secs_f64(delay));
        delay
    }

    /// Record one success, clearing any cooldown.
    pub fn note_success(&self) {
        let mut state = self.lock();
        state.until = None;
        state.consecutive_no_retry_after = 0;
    }

    /// Remaining cooldown, or zero when attempts may proceed.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        match self.lock().until {
            Some(until) => until.saturating_duration_since(tokio::time::Instant::now()),
            None => Duration::ZERO,
        }
    }
}

/// Jitter in [0, 1] from nanotime entropy (v2 uses `random.random`; only
/// the distribution shape matters for backoff).
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(500_000_000);
    f64::from(nanos % 1_000_000) / 1_000_000.0
}
