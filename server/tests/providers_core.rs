//! Stage-5 provider-core briefs: policy table, pacing, retry, degradation.
//!
//! Scripted fakes and the in-module doubles only; no live network anywhere
//! in this target. The providers tree is wired into the crate
//! (`droppedneedle::providers`); in-module unit tests run under the lib
//! target instead of this one.

use droppedneedle::providers;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use providers::{
    InMemoryProviderCache, ManualClock, OperationKind, ProviderClient, Providers, RetryPolicy,
    USER_QUIET_WINDOW, cache_aside_bytes, check_client_contract, execute, invalidate_source,
    policy_for, scoped,
};
use providers::{IntegrationStatus, ProviderOutcome, RequestPriority, SlotManager, apply, role};

// --- Policy table: six verified rows, matching the committed table. ---

#[test]
fn policy_rows_match_the_verified_table() {
    let committed: Vec<&str> = droppedneedle::provider_policy::PROVIDER_POLICIES
        .iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(
        committed,
        [
            "musicbrainz",
            "listenbrainz",
            "audiodb",
            "acoustid",
            "coverartarchive",
            "lastfm"
        ]
    );
    for name in &committed {
        assert!(
            policy_for(name).is_some(),
            "core paces every verified row ({name})"
        );
    }
    // LAN services take no limiter and no row.
    assert_eq!(policy_for("slskd"), None);
    assert_eq!(policy_for("sabnzbd"), None);
}

#[test]
fn limiter_briefs_per_policy_row() {
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

// --- Retry/backoff briefs with a scripted clock. ---

fn transport(source: &'static str) -> providers::ProviderError {
    providers::ProviderError::Transport {
        provider: source,
        message: "reset by peer".to_owned(),
    }
}

#[tokio::test]
async fn retry_brief_idempotent_read_rides_out_a_blip() {
    let policy =
        RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(4)).without_jitter();
    let clock = ManualClock::new();
    let calls = AtomicUsize::new(0);
    let outcome = execute(&policy, &clock, true, || {
        let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            if call == 1 {
                Err(transport("acoustid"))
            } else {
                Ok("fingerprint match")
            }
        }
    })
    .await;
    assert_eq!(outcome, Ok("fingerprint match"));
    assert_eq!(clock.sleeps(), [Duration::from_secs(1)]);
}

#[tokio::test]
async fn retry_brief_budget_caps_a_long_outage() {
    let policy = RetryPolicy::new(10, Duration::from_secs(2), Duration::from_secs(2))
        .without_jitter()
        .with_budget(Duration::from_secs(5));
    let clock = ManualClock::new();
    let calls = AtomicUsize::new(0);
    let outcome = execute(&policy, &clock, true, || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { Err::<(), _>(transport("lastfm")) }
    })
    .await;
    assert!(outcome.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        clock.sleeps(),
        [Duration::from_secs(2), Duration::from_secs(2)]
    );
}

// --- Degradation-matrix briefs. ---

#[tokio::test]
async fn matrix_brief_dead_optional_source_still_serves_the_request() {
    let (answer, context) = scoped(async {
        // Enrichment from a dead Last.fm: recorded + None, request succeeds.
        apply::<String>(OperationKind::Enrich, "lastfm", Err(transport("lastfm")))
    })
    .await;
    assert_eq!(answer, Ok(None));
    let degraded = context.degraded_summary();
    assert_eq!(degraded.get("lastfm"), Some(&IntegrationStatus::Error));
    // The recording is the error signal: the caller below sees success.
    let request_succeeded = answer.is_ok();
    assert!(request_succeeded);
}

#[tokio::test]
async fn matrix_brief_dead_musicbrainz_fails_identity() {
    let failure = transport("musicbrainz");
    let (answer, context) =
        scoped(async { apply::<String>(OperationKind::Identify, "musicbrainz", Err(failure)) })
            .await;
    assert_eq!(answer, Err(transport("musicbrainz")));
    assert!(context.has_degradation());
    assert_eq!(
        role(OperationKind::Identify, "musicbrainz"),
        providers::SourceRole::IdentityCritical
    );
}

#[tokio::test]
async fn matrix_brief_dead_musicbrainz_fails_search() {
    let failure = transport("musicbrainz");
    let (answer, context) =
        scoped(async { apply::<String>(OperationKind::Search, "musicbrainz", Err(failure)) }).await;
    assert_eq!(answer, Err(transport("musicbrainz")));
    assert!(context.has_degradation());
    assert_eq!(
        role(OperationKind::Search, "musicbrainz"),
        providers::SourceRole::Required
    );
}

#[tokio::test]
async fn matrix_brief_stale_cache_degrades_without_failing() {
    let outcome = ProviderOutcome::degraded(
        vec!["cached genres".to_owned()],
        "lastfm",
        "live fetch failed; serving cached",
    );
    assert!(outcome.is_degraded());
    let ((), context) = scoped(async {
        providers::record_outcome_current(&outcome);
    })
    .await;
    assert_eq!(outcome.data_or(vec![]), ["cached genres"]);
    assert_eq!(
        context.degraded_summary().get("lastfm"),
        Some(&IntegrationStatus::Degraded)
    );
}

// --- Coalescing brief. ---

#[tokio::test]
async fn coalescing_brief_one_flight_serves_the_burst() {
    let flights = providers::Singleflight::<String>::new();
    let runs = Arc::new(AtomicUsize::new(0));
    // The barrier lines the burst up so every task enters `run` together;
    // the short hold only covers join skew, not scheduling spread.
    let start = Arc::new(tokio::sync::Barrier::new(6));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let flights = flights.clone();
        let runs = Arc::clone(&runs);
        let start = Arc::clone(&start);
        handles.push(tokio::spawn(async move {
            let owned = runs;
            start.wait().await;
            flights
                .run("mb:rg:detail:shared", move || {
                    let runs = owned;
                    async move {
                        runs.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        Ok::<_, providers::ProviderError>("release group".to_owned())
                    }
                })
                .await
        }));
    }
    for handle in handles {
        let outcome = handle.await.expect("joins");
        assert_eq!(outcome.as_deref().map(String::as_str), Ok("release group"));
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

// --- Cache-aside + invalidation brief. ---

#[tokio::test]
async fn cache_brief_aside_and_source_sweep() {
    let deps = Providers::with_memory_cache();
    let key = "lfm_artist:cher";
    let miss = cache_aside_bytes(&*deps.cache, key, Duration::from_secs(60), || async {
        b"cached bio".to_vec()
    })
    .await;
    assert_eq!(miss, b"cached bio");
    // A source change sweeps the source; other sources survive.
    deps.cache
        .set_bytes("mb:rg:detail:x", b"mb".to_vec(), Duration::from_secs(60))
        .await;
    assert_eq!(invalidate_source(&*deps.cache, "lastfm").await, 1);
    assert!(deps.cache.get_bytes(key).await.is_none());
    assert_eq!(
        deps.cache.get_bytes("mb:rg:detail:x").await,
        Some(b"mb".to_vec())
    );
}

// --- Slot brief: background jobs name their lane. ---

#[tokio::test]
async fn slots_brief_background_names_its_lane_and_waits_for_quiet() {
    assert_eq!(USER_QUIET_WINDOW, Duration::from_secs(2));
    let slots = SlotManager::with_quiet_window(Duration::from_millis(60));
    slots.mark_user_activity();
    let started = tokio::time::Instant::now();
    let _permit = slots
        .acquire_slot(RequestPriority::BackgroundSync)
        .await
        .expect("background admits after quiet");
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert_eq!(slots.stats().background_slots_available, 4);
}

// --- Contract harness brief: slices call this against their clients. ---

struct ScriptedMusicBrainz;

impl ProviderClient for ScriptedMusicBrainz {
    fn source(&self) -> &'static str {
        "musicbrainz"
    }

    fn rate_policy(&self) -> providers::RatePolicy {
        policy_for("musicbrainz").expect("verified row")
    }

    fn cache_prefixes(&self) -> &'static [&'static str] {
        &["mb:rg:detail:", "mb:release:detail:"]
    }
}

#[test]
fn contract_brief_conforming_client_passes() {
    check_client_contract(&ScriptedMusicBrainz).expect("scripted client conforms");
}

#[test]
fn memory_cache_starts_empty() {
    let cache = InMemoryProviderCache::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        assert!(cache.is_empty().await);
    });
}

// --- Shared-surface briefs: every re-export earns its place. ---

#[test]
fn surface_brief_status_semantics_stay_distinct() {
    use providers::{MAX_RETRY_AFTER, ProviderError, classify_status, parse_retry_after};
    assert_eq!(classify_status("musicbrainz", 200, None), None);
    assert!(matches!(
        classify_status("lastfm", 401, None),
        Some(ProviderError::Unauthorized { .. })
    ));
    assert!(matches!(
        classify_status("lastfm", 403, None),
        Some(ProviderError::Forbidden { .. })
    ));
    assert!(matches!(
        classify_status("musicbrainz", 404, None),
        Some(ProviderError::NotFound { .. })
    ));
    // Only 429/503 honor Retry-After; a 500 header is not a wait signal.
    let limited = classify_status("audiodb", 429, Some("9")).expect("429 maps");
    assert_eq!(limited.retry_after(), Some(Duration::from_secs(9)));
    let broken = classify_status("audiodb", 500, Some("9")).expect("500 maps");
    assert_eq!(broken.retry_after(), None);
    assert_eq!(parse_retry_after(Some("9999")), Some(MAX_RETRY_AFTER));
}

#[tokio::test]
async fn surface_brief_over_capacity_names_its_burst() {
    use providers::{OverCapacity, RateLimiter};
    let limiter = RateLimiter::new(policy_for("musicbrainz").expect("row"));
    let error = limiter
        .acquire_with_priority(2, RequestPriority::UserInitiated)
        .await
        .expect_err("2 tokens exceed the burst of 1");
    assert_eq!(
        error,
        OverCapacity {
            tokens: 2,
            burst: 1
        }
    );
}

#[tokio::test]
async fn surface_brief_tokio_clock_runs_real_retries() {
    use providers::{Clock, TokioClock};
    let policy =
        RetryPolicy::new(2, Duration::from_millis(1), Duration::from_millis(1)).without_jitter();
    let clock = TokioClock;
    let started = clock.now();
    let outcome = execute(&policy, &clock, true, || async {
        Err::<(), _>(transport("coverartarchive"))
    })
    .await;
    assert!(outcome.is_err());
    assert!(clock.elapsed(started) >= Duration::from_millis(1));
}

#[test]
fn surface_brief_context_api_without_a_scope() {
    use providers::{
        DegradationContext, ProviderOutcome, aggregate_status, degraded_none, record_current,
        with_current,
    };
    let mut context = DegradationContext::new();
    context.record(&ProviderOutcome::ok("v", "acoustid"));
    assert!(!context.has_degradation());
    assert_eq!(
        aggregate_status(&[
            IntegrationStatus::Ok,
            IntegrationStatus::Degraded,
            IntegrationStatus::Error,
        ]),
        IntegrationStatus::Error
    );
    // Outside a scope, recording declines quietly and reads see nothing.
    assert!(!record_current("acoustid", IntegrationStatus::Error, false));
    assert_eq!(with_current(|_| ()), None);
    let missing: Option<String> = degraded_none("acoustid", IntegrationStatus::Error, false);
    assert_eq!(missing, None);
}

#[test]
fn surface_brief_outcome_from_shapes_aggregation() {
    use providers::{ProviderError, outcome_from};
    let ok = outcome_from("lastfm", Ok::<_, ProviderError>("genres".to_owned()));
    assert!(ok.expect("ok maps").is_ok());
    let failed = outcome_from("lastfm", Err::<String, _>(transport("lastfm")));
    assert!(failed.expect("error maps").is_error());
    // Absence contributes no outcome: nothing to aggregate.
    let absent = outcome_from(
        "musicbrainz",
        Err::<String, _>(ProviderError::NotFound {
            provider: "musicbrainz",
        }),
    );
    assert!(absent.is_none());
    let deterministic = outcome_from(
        "musicbrainz",
        Err::<String, _>(ProviderError::Payload {
            provider: "musicbrainz",
            message: "null MBID".to_owned(),
        }),
    );
    assert!(deterministic.expect("payload maps").deterministic);
}

#[tokio::test]
async fn surface_brief_json_aside_and_key_builders() {
    use providers::{cache_aside_json, digest_key, namespaced_key, prefixes_for};
    assert!(!prefixes_for("musicbrainz").is_empty());
    assert!(prefixes_for("slskd").is_empty());
    assert_eq!(
        namespaced_key("mb:rg:detail:", "  AB12 "),
        "mb:rg:detail:ab12"
    );
    let key = digest_key("lfm_management:album:", &["Cher", " Believe "], "v1");
    assert!(key.starts_with("lfm_management:album:"));
    let deps = Providers::with_memory_cache();
    let value = cache_aside_json::<Vec<String>, String, _, _>(
        &*deps.cache,
        &key,
        Duration::from_secs(60),
        || async { Ok(vec!["pop".to_owned()]) },
    )
    .await
    .expect("fetch succeeds");
    assert_eq!(value, ["pop"]);
}

#[test]
fn surface_brief_contract_violation_reports_every_breach() {
    use providers::{ContractViolation, RatePolicy};
    struct BadClient;
    impl ProviderClient for BadClient {
        fn source(&self) -> &'static str {
            "NOT-LOWER"
        }
        fn rate_policy(&self) -> RatePolicy {
            RatePolicy::new(1.0, 1)
        }
        fn cache_prefixes(&self) -> &'static [&'static str] {
            &[]
        }
    }
    let violation: ContractViolation =
        check_client_contract(&BadClient).expect_err("bad client fails");
    assert!(violation.failures.len() >= 3, "{violation}");
}

#[tokio::test]
async fn surface_brief_every_helper_earns_its_keep() {
    use providers::{ProviderCache as _, ProviderError, Singleflight};
    // The deps struct's own lanes serve slots too.
    let deps = Providers::with_memory_cache();
    let _permit = deps
        .slots
        .acquire_slot(RequestPriority::PrefetchVisible)
        .await
        .expect("prefetch takes the background lane when idle");
    // The limiter reports the row it enforces.
    let musicbrainz = deps.limiter("musicbrainz").expect("mb limiter");
    assert_eq!(
        musicbrainz.policy(),
        policy_for("musicbrainz").expect("row")
    );
    // Cache entries delete individually and sweep when expired.
    deps.cache
        .set_bytes("lfm_temp", b"x".to_vec(), Duration::from_secs(60))
        .await;
    deps.cache.delete("lfm_temp").await;
    assert!(deps.cache.get_bytes("lfm_temp").await.is_none());
    let memory = InMemoryProviderCache::new();
    memory
        .set_bytes("lfm_stale", b"x".to_vec(), Duration::ZERO)
        .await;
    assert_eq!(memory.cleanup_expired().await, 1);
    // The scripted clock burns operation time as well as sleeps.
    let clock = ManualClock::new();
    clock.advance(Duration::from_secs(3));
    assert_eq!(
        clock.sleeps(),
        Vec::<Duration>::new(),
        "advancing is not sleeping"
    );
    // Clearing the flight table only affects future callers.
    let flights = Singleflight::<String>::new();
    flights.clear().await;
    assert_eq!(flights.pending_count().await, 0);
    // Missing configuration is deterministic, never retried.
    let missing = ProviderError::NotConfigured {
        provider: "lastfm",
        message: "no API key".to_owned(),
    };
    assert!(!missing.is_retriable());
    assert!(!missing.trips_breaker());
}

#[tokio::test]
async fn surface_brief_slot_stats_and_error_shape() {
    use providers::{SlotError, SlotStats};
    let slots = SlotManager::new();
    let stats: SlotStats = slots.stats();
    assert_eq!(stats.user_slots_available, 20);
    let permit = slots
        .acquire_slot(RequestPriority::ImageFetch)
        .await
        .map_err(|_: SlotError| "shutdown".to_owned());
    assert!(permit.is_ok());
}
