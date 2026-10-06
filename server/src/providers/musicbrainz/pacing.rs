//! Process-wide MusicBrainz pacing.
//!
//! Every MusicBrainz client in the process paces through state shared in
//! [`Providers`]: the official service takes the `musicbrainz` row of the
//! limiter table (1 req/s, no burst), BrainzMash takes its own
//! `brainzmash` row (10 req/s), and a self-hosted mirror takes the rate its
//! owner set in Settings (`0` means unpaced, as the mirror guide says).
//! Two clients built from the same [`MbPacing`] can never exceed the policy
//! together, which a private gate per client could not promise.
//!
//! BrainzMash also cools down after a 429: the cooldown honors
//! `Retry-After` or backs off exponentially with jitter (v2
//! `_BrainzMashScheduler`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{BRAINZMASH_MAX_COOLDOWN_SECS, MbSource};
use crate::providers::limiter::{RateLimiter, RatePolicy};
use crate::providers::{Providers, RequestPriority};

/// Longest BrainzMash cooldown a user request waits out. Past it the
/// request fails at once and the page answers from what it has; the
/// matching catalog retry budget is the same 2.5 s.
pub const USER_COOLDOWN_WAIT: Duration = Duration::from_millis(2500);

/// BrainzMash cooldown base (v2 `_BRAINZMASH_COOLDOWN_BASE_SECONDS`).
const BRAINZMASH_COOLDOWN_BASE_SECS: f64 = 1.0;

/// MusicBrainz pacing state shared by every client: the BrainzMash
/// cooldown and the limiter for the configured mirror.
#[derive(Debug, Default)]
pub struct MbPacingState {
    cooldown: BrainzMashCooldown,
    /// The mirror limiter and the rate it was built for; rebuilt when the
    /// owner changes the rate.
    mirror: Mutex<Option<(f64, Arc<RateLimiter>)>>,
}

impl MbPacingState {
    fn mirror_limiter(&self, rate_per_sec: f64) -> Arc<RateLimiter> {
        let mut slot = self
            .mirror
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match slot.as_ref() {
            Some((rate, limiter)) if *rate == rate_per_sec => limiter.clone(),
            _ => {
                let limiter = Arc::new(RateLimiter::new(RatePolicy::new(rate_per_sec, 1)));
                *slot = Some((rate_per_sec, limiter.clone()));
                limiter
            }
        }
    }
}

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

    /// Wait for one slot on `source` at `priority`. A user request does
    /// not sit out a BrainzMash cooldown longer than
    /// [`USER_COOLDOWN_WAIT`]: it gets the time left back instead, so the
    /// page answers from what it has.
    pub async fn acquire(
        &self,
        source: &MbSource,
        priority: RequestPriority,
    ) -> Result<(), Duration> {
        match source {
            MbSource::Official { .. } => {
                take(self.providers.limiter("musicbrainz"), priority).await;
            }
            MbSource::BrainzMash { .. } => {
                let wait = self.cooldown().remaining();
                if priority == RequestPriority::UserInitiated && wait > USER_COOLDOWN_WAIT {
                    return Err(wait);
                }
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                take(self.providers.limiter("brainzmash"), priority).await;
            }
            MbSource::Mirror { rate_per_sec, .. }
                if rate_per_sec.is_finite() && *rate_per_sec > 0.0 =>
            {
                let limiter = self.providers.musicbrainz.mirror_limiter(*rate_per_sec);
                take(Some(&limiter), priority).await;
            }
            // Rate 0 is the owner's "unlimited" for their own mirror.
            MbSource::Mirror { .. } => {}
        }
        Ok(())
    }

    /// The shared BrainzMash cooldown.
    #[must_use]
    pub fn cooldown(&self) -> &BrainzMashCooldown {
        &self.providers.musicbrainz.cooldown
    }
}

/// Take one token, when the source is paced at all.
async fn take(limiter: Option<&RateLimiter>, priority: RequestPriority) {
    if let Some(limiter) = limiter
        && let Err(error) = limiter.acquire_with_priority(1, priority).await
    {
        // One token never exceeds a burst of one or more; log rather than
        // fail the call if the table is ever misconfigured.
        tracing::error!(%error, "musicbrainz pacing refused a token");
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
