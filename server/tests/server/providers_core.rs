//! The provider executor and the rate table: what each answer becomes,
//! when a call is tried again, how long the next call waits, and that a
//! key in a URL never reaches an error or a log line.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_providers::executor::{
    Backoff, Executor, Method, RedirectRule, Redirects, Request, Scripted, ScriptedWire, Spec, Url,
    standard_classify,
};
use droppedneedle_providers::health::ServiceHealth;
use droppedneedle_providers::musicbrainz::{self, Criticality, MbSource};
use droppedneedle_providers::rates::{RATES, Rate};
use droppedneedle_providers::{
    LimiterSet, PayloadKind, ProviderEndpoints, ProviderError, Providers, RateLimiter, RatePolicy,
    RequestPriority, TransportKind,
};
use tokio::time::Instant;

const BASE: &str = "https://provider.test/api";

static SPEC: Spec = Spec {
    source: "test",
    health_key: "test",
    timeout: Duration::from_secs(1),
    max_body: 64,
    retry_after_default: Backoff::Fixed(Duration::from_secs(1)),
    retry_after_cap: Duration::from_secs(60),
    classify: standard_classify,
};

static HOPS: RedirectRule = RedirectRule {
    max_hops: 2,
    allow: |_, _| true,
};

fn url(path: &str) -> String {
    format!("{BASE}/{path}")
}

fn answer(status: u16, headers: &[(&str, &str)], body: &str) -> Scripted {
    Scripted::Answer {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: body.as_bytes().to_vec(),
    }
}

fn redirect(status: u16, to: &str) -> Scripted {
    answer(status, &[("location", &url(to))], "")
}

/// An executor over a scripted wire, on a bucket fast enough that only
/// cooldowns make anyone wait.
fn executor(wire: &Arc<ScriptedWire>) -> Executor {
    Executor::new(
        &SPEC,
        Arc::new(RateLimiter::new(RatePolicy::new(1000.0, 1000))),
        wire.clone(),
        Arc::new(ServiceHealth::new()),
        Arc::new(SystemClock),
    )
}

fn get(path: &str) -> Request {
    let base = Url::parse(BASE).unwrap();
    Request::get(&base, &[path], &[])
        .unwrap()
        .redirects(Redirects::Follow(&HOPS))
}

/// Every row of the table, its date and basis, and one limiter per row.
#[test]
fn the_rate_table_is_dated_and_builds_the_limiters() {
    for row in RATES {
        let date = row.verified_on.as_bytes();
        assert!(
            date.len() == 10
                && date[4] == b'-'
                && date[7] == b'-'
                && date
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit()),
            "{} is dated YYYY-MM-DD",
            row.source
        );
        assert!(!row.basis.trim().is_empty(), "{} has a basis", row.source);
        if row.per_second != Rate::Setting && !row.basis.starts_with("No documented limit") {
            assert!(
                row.url.starts_with("https://"),
                "{} says where it was read",
                row.source
            );
        }
    }
    let set = LimiterSet::new();
    let sources: Vec<&str> = RATES.iter().map(|row| row.source).collect();
    assert_eq!(
        set.sources(),
        sources,
        "one limiter per row, in table order"
    );
    let mut unique = sources.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), sources.len(), "no row twice");
    // MusicBrainz's documented rule is hard: one a second, no burst.
    let musicbrainz = set.limiter("musicbrainz").unwrap();
    assert_eq!(musicbrainz.policy(), RatePolicy::new(1.0, 1));
}

/// Expected outcome of one row: the error (or success) and how many
/// requests went out.
enum Want {
    Ok,
    Err(fn(&ProviderError) -> bool),
}

/// One executor per row, driven through every status the classifier
/// knows. A call is tried again only for an answer that may change; 429
/// and 503 with a wait hold the next call that long.
#[tokio::test(start_paused = true)]
async fn the_executor_classifies_every_status_and_retries_only_what_may_change() {
    use ProviderError as E;
    let rows: Vec<(&str, Vec<Scripted>, Want, usize)> = vec![
        ("200", vec![answer(200, &[], "{}")], Want::Ok, 1),
        ("204", vec![answer(204, &[], "")], Want::Ok, 1),
        (
            "301 within the hop limit",
            vec![redirect(301, "next")],
            Want::Ok,
            2,
        ),
        (
            "302 within the hop limit",
            vec![redirect(302, "next")],
            Want::Ok,
            2,
        ),
        (
            "307 within the hop limit",
            vec![redirect(307, "next")],
            Want::Ok,
            2,
        ),
        (
            "307 past the hop limit",
            vec![redirect(307, "loop")],
            Want::Err(|e| {
                matches!(
                    e,
                    E::Payload {
                        kind: PayloadKind::Contract,
                        ..
                    }
                )
            }),
            3,
        ),
        (
            "400",
            vec![answer(400, &[], "")],
            Want::Err(|e| matches!(e, E::BadRequest { status: 400, .. })),
            1,
        ),
        (
            "401",
            vec![answer(401, &[], "")],
            Want::Err(|e| matches!(e, E::Unauthorized { .. })),
            1,
        ),
        (
            "403",
            vec![answer(403, &[], "")],
            Want::Err(|e| matches!(e, E::Forbidden { .. })),
            1,
        ),
        (
            "404",
            vec![answer(404, &[], "")],
            Want::Err(|e| matches!(e, E::NotFound { .. })),
            1,
        ),
        (
            "408",
            vec![answer(408, &[], "")],
            Want::Err(|e| matches!(e, E::BadRequest { status: 408, .. })),
            1,
        ),
        (
            "429 with Retry-After",
            vec![answer(429, &[("retry-after", "2")], "")],
            Want::Err(
                |e| matches!(e, E::RateLimited { retry_after: Some(wait), .. } if *wait == Duration::from_secs(2)),
            ),
            2,
        ),
        (
            "429 without Retry-After",
            vec![answer(429, &[], "")],
            Want::Err(|e| {
                matches!(
                    e,
                    E::RateLimited {
                        retry_after: None,
                        ..
                    }
                )
            }),
            2,
        ),
        (
            "500",
            vec![answer(500, &[], "")],
            Want::Err(|e| matches!(e, E::Server { status: 500, .. })),
            2,
        ),
        (
            "502",
            vec![answer(502, &[], "")],
            Want::Err(|e| matches!(e, E::Server { status: 502, .. })),
            2,
        ),
        (
            "503 with Retry-After",
            vec![answer(503, &[("retry-after", "2")], "")],
            Want::Err(
                |e| matches!(e, E::Unavailable { retry_after: Some(wait), .. } if *wait == Duration::from_secs(2)),
            ),
            2,
        ),
        (
            "503 without Retry-After",
            vec![answer(503, &[], "")],
            Want::Err(|e| {
                matches!(
                    e,
                    E::Unavailable {
                        retry_after: None,
                        ..
                    }
                )
            }),
            2,
        ),
        (
            "transport failure",
            vec![Scripted::Transport(TransportKind::Connect)],
            Want::Err(|e| {
                matches!(
                    e,
                    E::Transport {
                        kind: TransportKind::Connect,
                        ..
                    }
                )
            }),
            2,
        ),
        (
            "timeout",
            vec![Scripted::Hang],
            Want::Err(|e| {
                matches!(
                    e,
                    E::Transport {
                        kind: TransportKind::Timeout,
                        ..
                    }
                )
            }),
            2,
        ),
        (
            "over-cap body",
            vec![answer(200, &[], &"x".repeat(65))],
            Want::Err(|e| {
                matches!(
                    e,
                    E::Payload {
                        kind: PayloadKind::TooLarge,
                        ..
                    }
                )
            }),
            1,
        ),
    ];
    for (name, script, want, sends) in rows {
        let wire = Arc::new(ScriptedWire::new());
        for outcome in script {
            wire.on(Method::Get, &url("start"), outcome);
        }
        wire.on(Method::Get, &url("next"), answer(200, &[], "{}"));
        wire.on(Method::Get, &url("loop"), redirect(307, "loop"));
        let executor = executor(&wire);
        let result = executor
            .send(get("start"), RequestPriority::UserInitiated)
            .await;
        match (&want, &result) {
            (Want::Ok, Ok(_)) => {}
            (Want::Err(check), Err(error)) if check(error) => {}
            _ => panic!("{name}: got {result:?}"),
        }
        assert_eq!(wire.sent().len(), sends, "{name}: requests sent");
        if let Err(error) = &result
            && let Some(wait) = error.retry_after()
        {
            let started = Instant::now();
            executor
                .send(get("next"), RequestPriority::UserInitiated)
                .await
                .expect("the next call is answered");
            assert!(
                started.elapsed() >= wait,
                "{name}: the next call waits out {wait:?}, waited {:?}",
                started.elapsed()
            );
        }
    }
}

/// Five failed calls in a row rest the provider for 30 seconds: a page
/// is answered at once without a request, background work waits it out.
#[tokio::test(start_paused = true)]
async fn a_run_of_failures_rests_the_provider() {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(Method::Get, &url("start"), answer(500, &[], ""));
    wire.on(Method::Get, &url("next"), answer(200, &[], "{}"));
    let executor = executor(&wire);
    for _ in 0..5 {
        executor
            .send(get("start"), RequestPriority::UserInitiated)
            .await
            .expect_err("500");
    }
    let sent = wire.sent().len();
    let declined = executor
        .send(get("next"), RequestPriority::UserInitiated)
        .await
        .expect_err("a page does not wait for a resting provider");
    assert!(matches!(declined, ProviderError::RateLimited { .. }));
    assert_eq!(wire.sent().len(), sent, "nothing sent");
    let started = Instant::now();
    executor
        .send(get("next"), RequestPriority::BackgroundSync)
        .await
        .expect("background work is answered after the rest");
    assert!(started.elapsed() >= Duration::from_secs(29));
}

fn musicbrainz_over(wire: &Arc<ScriptedWire>) -> Providers {
    Providers::over_wire(
        wire.clone(),
        Arc::new(SystemClock),
        ProviderEndpoints {
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
            brainzmash: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
        },
    )
}

const RELEASE: &str = "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b";

fn brainzmash() -> MbSource {
    MbSource::BrainzMash {
        base_url: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
        binding_valid: true,
    }
}

/// The MusicBrainz rules: the official 503 is its rate limit; a BrainzMash
/// 429 that names no wait rests the mirror anyway, and a page meeting a
/// long rest is answered at once.
#[tokio::test(start_paused = true)]
async fn musicbrainz_hooks() {
    let wire = Arc::new(ScriptedWire::new());
    let official = format!("{}/release/{RELEASE}", musicbrainz::MB_API_BASE);
    wire.answer(&official, 503, &[], "");
    let providers = musicbrainz_over(&wire);
    let error = providers
        .musicbrainz
        .fixed(MbSource::official(), RequestPriority::UserInitiated)
        .lookup_release(RELEASE, &[], Criticality::IdentityCritical)
        .await
        .expect_err("503");
    assert!(
        matches!(
            error,
            ProviderError::RateLimited {
                retry_after: None,
                ..
            }
        ),
        "official 503 is a rate limit: {error:?}"
    );
    assert!(
        providers.limiter("musicbrainz").unwrap().cooling_for() > Duration::ZERO,
        "and rests the bucket"
    );

    let wire = Arc::new(ScriptedWire::new());
    let mirror = format!("{}/release/{RELEASE}", musicbrainz::BRAINZMASH_API_BASE);
    wire.answer(&mirror, 429, &[], "");
    let providers = musicbrainz_over(&wire);
    let reads = providers
        .musicbrainz
        .fixed(brainzmash(), RequestPriority::BackgroundSync);
    reads
        .lookup_release(RELEASE, &[], Criticality::IdentityCritical)
        .await
        .expect_err("429");
    // Three headerless 429s in a row: one second doubled twice, jittered
    // down by at most half.
    let rest = providers.limiter("brainzmash").unwrap().cooling_for();
    assert!(
        (Duration::from_secs(2)..=Duration::from_secs(4)).contains(&rest),
        "a headerless 429 rests the mirror, longer each time: {rest:?}"
    );

    let wire = Arc::new(ScriptedWire::new());
    wire.answer(&mirror, 429, &[("retry-after", "9999")], "");
    let providers = musicbrainz_over(&wire);
    let error = providers
        .musicbrainz
        .fixed(brainzmash(), RequestPriority::UserInitiated)
        .lookup_release(RELEASE, &[], Criticality::IdentityCritical)
        .await
        .expect_err("429");
    assert_eq!(
        error.retry_after(),
        Some(Duration::from_secs(60)),
        "the mirror's wait is capped at a minute"
    );

    let wire = Arc::new(ScriptedWire::new());
    wire.answer(&mirror, 200, &[], format!(r#"{{"id": "{RELEASE}"}}"#));
    let providers = musicbrainz_over(&wire);
    providers
        .limiter("brainzmash")
        .unwrap()
        .cool_down(Duration::from_secs(30));
    let error = providers
        .musicbrainz
        .fixed(brainzmash(), RequestPriority::UserInitiated)
        .lookup_release(RELEASE, &[], Criticality::IdentityCritical)
        .await
        .expect_err("a page does not wait 30 s");
    assert!(
        matches!(error, ProviderError::RateLimited { retry_after: Some(wait), .. } if wait > Duration::from_millis(2500)),
        "{error:?}"
    );
    assert!(wire.sent().is_empty(), "nothing sent");
}

/// A key in the query or in the path never shows in the error or in any
/// log line when the request fails in transport.
#[test]
fn a_key_in_the_url_never_reaches_an_error_or_a_log() {
    let ((query_error, path_error), logs) = crate::common::logs::capture_logs(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
                let executor = Executor::new(
                    &SPEC,
                    Arc::new(RateLimiter::new(RatePolicy::new(1000.0, 1000))),
                    Arc::new(droppedneedle_providers::ReqwestWire::new(&http)),
                    Arc::new(ServiceHealth::new()),
                    Arc::new(SystemClock),
                );
                // Nothing listens on the discard port.
                let base = Url::parse("http://127.0.0.1:9/api").unwrap();
                let query = executor
                    .send(
                        Request::get(&base, &["search"], &[("api_key", "SECRET")]).unwrap(),
                        RequestPriority::UserInitiated,
                    )
                    .await
                    .expect_err("nothing listens");
                let path = executor
                    .send(
                        Request::get(&base, &["SECRET", "search.php"], &[]).unwrap(),
                        RequestPriority::UserInitiated,
                    )
                    .await
                    .expect_err("nothing listens");
                (query, path)
            })
    });
    for error in [&query_error, &path_error] {
        assert!(
            matches!(error, ProviderError::Transport { .. }),
            "{error:?}"
        );
        let text = format!("{error} {error:?}");
        assert!(
            !text.contains("SECRET") && !text.contains("127.0.0.1"),
            "{text}"
        );
    }
    assert!(logs.contains("provider"), "the failure is logged: {logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
    assert!(!logs.contains("/api/"), "{logs}");
}
