//! The rate budget each verified provider row enforces. Retry, cache,
//! coalescing, slots and the degradation matrix are pinned by the unit
//! tests in `src/providers`.

use droppedneedle::providers::Providers;

use std::time::Duration;

#[test]
fn limiter_budgets_per_policy_row() {
    let deps = Providers::with_memory_cache();
    // MusicBrainz 1/s hard: one token, then dry with ~1s retry-after.
    let musicbrainz = deps.limiter("musicbrainz").expect("mb limiter");
    assert!(musicbrainz.try_acquire(1));
    assert!(!musicbrainz.try_acquire(1));
    assert_eq!(musicbrainz.retry_after(1), Duration::from_secs(1));
    // ListenBrainz 1/s, same shape.
    let listenbrainz = deps.limiter("listenbrainz").expect("lb limiter");
    assert!(listenbrainz.try_acquire(1));
    assert!(!listenbrainz.try_acquire(1));
    // AudioDB 30/min: a burst of 2 at 0.5/s.
    let audiodb = deps.limiter("audiodb").expect("audiodb limiter");
    assert!(audiodb.try_acquire(2));
    assert!(!audiodb.try_acquire(1));
    // AcoustID 3/s with a one-second burst.
    let acoustid = deps.limiter("acoustid").expect("acoustid limiter");
    assert!(acoustid.try_acquire(3));
    assert!(!acoustid.try_acquire(1));
    // Cover Art Archive ~1/s + backoff (backoff is the retry layer's job).
    let coverart = deps.limiter("coverartarchive").expect("caa limiter");
    assert!(coverart.try_acquire(2));
    assert!(!coverart.try_acquire(1));
    // Last.fm 5/s + backoff with a two-second burst.
    let lastfm = deps.limiter("lastfm").expect("lastfm limiter");
    assert!(lastfm.try_acquire(10));
    assert!(!lastfm.try_acquire(1));
}
