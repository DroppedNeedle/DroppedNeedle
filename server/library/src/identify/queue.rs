//! Queue policy: priorities, leases, and the bounded backoff ladder.

/// New or changed albums identify first.
pub const PRIORITY_NEW_OR_CHANGED: u32 = 20;
/// A curator asked for another look.
pub const PRIORITY_REVIEW_RETRY: u32 = 30;
/// Historical backlog fills in behind live work.
pub const PRIORITY_HISTORICAL_BACKLOG: u32 = 40;
/// Supporting maintenance runs last.
pub const PRIORITY_SUPPORTING_MAINTENANCE: u32 = 50;

/// How long one worker holds a claimed job before it can be reclaimed.
pub const LEASE_SECONDS: u64 = 60;

/// The ladder is 30, 60, 120 ... doubling per
/// deferral, capped so it always equals the largest delay the formula can
/// schedule under the ten-attempt terminal bound: 30 through 7,680 s,
/// 15,330 s cumulative before attempt ten terminalizes. A bounded-retry
/// window, never a provider-health timeout.
pub const MAX_DEFERRAL_ATTEMPTS: u32 = 10;
pub const MAX_BACKOFF_SECONDS: u64 = 30 * 2_u64.pow(MAX_DEFERRAL_ATTEMPTS - 2);

/// Backoff before the next attempt after `deferrals` consecutive deferrals
/// (1-based: the first deferral waits 30 s).
pub fn backoff_secs(deferrals: u32) -> u64 {
    if deferrals == 0 {
        return 0;
    }
    let shift = deferrals.saturating_sub(1).min(30);
    (30_u64.saturating_mul(2_u64.pow(shift))).min(MAX_BACKOFF_SECONDS)
}

/// True once the job has used its whole bounded window.
pub fn terminally_deferred(attempts: u32) -> bool {
    attempts >= MAX_DEFERRAL_ATTEMPTS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_matches_owner_signed_sequence() {
        let ladder: Vec<u64> = (1..=9).map(backoff_secs).collect();
        assert_eq!(ladder, vec![30, 60, 120, 240, 480, 960, 1920, 3840, 7680]);
        assert_eq!(MAX_BACKOFF_SECONDS, 7680);
    }
}
