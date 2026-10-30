//! The provider executor and the rate table: what each answer becomes,
//! when a call is tried again, how long the next call waits, and that a
//! key in a URL never reaches an error or a log line.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_providers::coverart::{self, DownloadSize, EntityKind};
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
use droppedneedle_providers::{acoustid, audiodb, lastfm, listenbrainz};
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
    next: |_, next| Some(next.clone()),
    foreign: droppedneedle_providers::executor::never_foreign,
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
    assert!(executor.resting_after_failures());
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

/// A page never waits past its budget for a request. On a slow row
/// whose bucket is spent, a person's call that would wait longer than
/// 2.5 s, counting the bucket and the people queued ahead of it, is
/// refused at once with the wait and sends nothing; one whose turn is
/// close waits for it. Background work queued on the same row still waits
/// and is sent, and so does an image fetch, whose caller bounds the wait.
#[tokio::test(start_paused = true)]
async fn a_page_never_waits_past_its_budget_for_a_request() {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(Method::Get, &url("item"), answer(200, &[], "{}"));
    let row = |per_second| {
        Executor::new(
            &SPEC,
            Arc::new(RateLimiter::new(RatePolicy::new(per_second, 1))),
            wire.clone(),
            Arc::new(ServiceHealth::new()),
            Arc::new(SystemClock),
        )
    };
    let spawn = |executor: &Executor, priority| {
        let executor = executor.clone();
        tokio::spawn(async move { executor.send(get("item"), priority).await })
    };

    // One request every five seconds, the bucket spent, a sync queued.
    let slow = row(0.2);
    slow.send(get("item"), RequestPriority::BackgroundSync)
        .await
        .expect("the burst");
    let background = spawn(&slow, RequestPriority::BackgroundSync);
    tokio::task::yield_now().await;
    let started = Instant::now();
    let declined = slow
        .send(get("item"), RequestPriority::UserInitiated)
        .await
        .expect_err("a page does not wait five seconds");
    assert_eq!(started.elapsed(), Duration::ZERO, "refused at once");
    let ProviderError::RateLimited {
        retry_after: Some(wait),
        ..
    } = declined
    else {
        panic!("a rate-limit refusal with its wait: {declined:?}");
    };
    assert!(wait > Duration::from_secs(4) && wait <= Duration::from_secs(5));
    assert_eq!(wire.sent().len(), 1, "nothing sent for the page");
    background
        .await
        .unwrap()
        .expect("background work waits its turn");
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert_eq!(wire.sent().len(), 2);

    // An image fetch keeps fetching: its caller bounds the person's wait.
    let started = Instant::now();
    slow.send(get("item"), RequestPriority::ImageFetch)
        .await
        .expect("an image fetch waits its turn");
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert_eq!(wire.sent().len(), 3);

    // One a second: two people queued ahead put the third past the budget.
    let paced = row(1.0);
    paced
        .send(get("item"), RequestPriority::UserInitiated)
        .await
        .expect("the burst");
    let first = spawn(&paced, RequestPriority::UserInitiated);
    let second = spawn(&paced, RequestPriority::UserInitiated);
    tokio::task::yield_now().await;
    let third = paced
        .send(get("item"), RequestPriority::UserInitiated)
        .await
        .expect_err("third in line is three seconds away");
    assert!(matches!(third, ProviderError::RateLimited { .. }));
    first.await.unwrap().expect("one second away");
    second.await.unwrap().expect("two seconds away");
    assert_eq!(wire.sent().len(), 6);
}

fn providers_over(wire: &Arc<ScriptedWire>) -> Providers {
    Providers::over_wire(
        wire.clone(),
        Arc::new(SystemClock),
        ProviderEndpoints {
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
            brainzmash: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
            coverart: coverart::COVER_ART_ARCHIVE_BASE.to_owned(),
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            lastfm: lastfm::DEFAULT_BASE_URL.to_owned(),
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            acoustid: acoustid::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
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

/// The MusicBrainz rules: the official 503 is its rate limit and a
/// headerless official 429 (the MetaBrainz gateway) rests a minute or more,
/// growing; a BrainzMash 429 that names no wait rests the mirror anyway,
/// and a page meeting a long rest is answered at once.
#[tokio::test(start_paused = true)]
async fn musicbrainz_hooks() {
    let wire = Arc::new(ScriptedWire::new());
    let official = format!("{}/release/{RELEASE}", musicbrainz::MB_API_BASE);
    wire.answer(&official, 503, &[], "");
    let providers = providers_over(&wire);
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
                retry_after: Some(wait),
                ..
            } if wait == Duration::from_secs(1)
        ),
        "official 503 is a rate limit of a second: {error:?}"
    );
    assert!(
        providers.limiter("musicbrainz").unwrap().cooling_for() > Duration::ZERO,
        "and rests the bucket"
    );

    // Three headerless 429s in a row: at least a minute, then two, then
    // four, each jittered upward by at most the same again.
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &official,
        429,
        &[("content-type", "text/html")],
        "<html>429</html>",
    );
    let providers = providers_over(&wire);
    let reads = providers
        .musicbrainz
        .fixed(MbSource::official(), RequestPriority::BackgroundSync);
    let mut rests = Vec::new();
    for _ in 0..3 {
        reads
            .lookup_release(RELEASE, &[], Criticality::IdentityCritical)
            .await
            .expect_err("429");
        rests.push(providers.limiter("musicbrainz").unwrap().cooling_for());
    }
    assert_eq!(wire.sent().len(), 3, "one request per rest");
    for (n, rest) in rests.iter().enumerate() {
        let full = Duration::from_secs(60 << n);
        assert!(
            (full..=full * 2).contains(rest),
            "headerless 429 number {}: rests {rest:?}, want {full:?} to twice that",
            n + 1
        );
    }

    let wire = Arc::new(ScriptedWire::new());
    let mirror = format!("{}/release/{RELEASE}", musicbrainz::BRAINZMASH_API_BASE);
    wire.answer(&mirror, 429, &[], "");
    let providers = providers_over(&wire);
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
    let providers = providers_over(&wire);
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
    let providers = providers_over(&wire);
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

/// The Cover Art Archive rules: a redirect to the archive hosts is
/// followed without another token, one to any other host is refused, an
/// image past the cap is unusable but not an outage (a run of them never
/// rests the archive), and a 404 at the end of the redirects is "no
/// cover".
#[tokio::test(start_paused = true)]
async fn cover_art_archive_hooks() {
    type Check = fn(&Result<Option<coverart::ArtworkBytes>, ProviderError>) -> bool;
    type Row<'a> = (&'a str, Vec<(&'a str, Scripted)>, Check, usize);
    let front = format!(
        "{}/release-group/{RELEASE}/front-500",
        coverart::COVER_ART_ARCHIVE_BASE
    );
    let cdn = "https://archive.org/download/mbid-x/front_500.png";
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 16]].concat();
    let to_cdn = answer(307, &[("location", cdn)], "");
    let rows: Vec<Row> = vec![
        (
            "307 to an archive host",
            vec![
                (front.as_str(), to_cdn.clone()),
                (
                    cdn,
                    Scripted::Answer {
                        status: 200,
                        headers: Vec::new(),
                        body: png.clone(),
                    },
                ),
            ],
            |result| matches!(result, Ok(Some(art)) if art.content_type == "image/png"),
            2,
        ),
        (
            "307 to another host",
            vec![(
                front.as_str(),
                answer(
                    307,
                    &[("location", "https://archive.org.evil.example/x.png")],
                    "",
                ),
            )],
            |result| matches!(result, Err(ProviderError::Redirect { status: 307, .. })),
            1,
        ),
        (
            "over-cap image",
            vec![
                (front.as_str(), to_cdn.clone()),
                (cdn, answer(200, &[], &"x".repeat(65))),
            ],
            |result| {
                matches!(
                    result,
                    Err(ProviderError::Payload {
                        kind: PayloadKind::TooLarge,
                        ..
                    })
                )
            },
            2,
        ),
        (
            "404 at the end of the redirects",
            vec![
                (front.as_str(), to_cdn.clone()),
                (cdn, answer(404, &[], "")),
            ],
            |result| matches!(result, Ok(None)),
            2,
        ),
    ];
    for (name, script, check, sends) in rows {
        let wire = Arc::new(ScriptedWire::new());
        for (url, outcome) in script {
            wire.on(Method::Get, url, outcome);
        }
        let providers = providers_over(&wire);
        for _ in 0..5 {
            let result = providers
                .coverart
                .fetch_front(
                    EntityKind::ReleaseGroup,
                    RELEASE,
                    DownloadSize::Size500,
                    64,
                    RequestPriority::ImageFetch,
                )
                .await;
            assert!(check(&result), "{name}: got {result:?}");
        }
        assert_eq!(wire.sent().len(), sends * 5, "{name}: requests sent");
        assert_eq!(
            providers.limiter("coverartarchive").unwrap().cooling_for(),
            Duration::ZERO,
            "{name}: the archive is not rested"
        );
    }
}

/// The archive's redirects lead to the Internet Archive's storage nodes,
/// which fail one at a time. Five failures in a row there do not rest the
/// archive; five on a hop that stays on the archive's own host do; and a
/// wait a storage node names still cools the row.
#[tokio::test(start_paused = true)]
async fn only_storage_node_failures_are_kept_off_the_archive() {
    let front = format!(
        "{}/release-group/{RELEASE}/front-500",
        coverart::COVER_ART_ARCHIVE_BASE
    );
    let node = "https://ia600508.us.archive.org/9/items/mbid-x/front_500.jpg";
    let same_host = format!(
        "{}/release/{RELEASE}/front-500",
        coverart::COVER_ART_ARCHIVE_BASE
    );
    let fetch = |providers: &Providers| {
        let client = providers.coverart.clone();
        async move {
            client
                .fetch_front(
                    EntityKind::ReleaseGroup,
                    RELEASE,
                    DownloadSize::Size500,
                    1 << 20,
                    RequestPriority::BackgroundSync,
                )
                .await
        }
    };

    let wire = Arc::new(ScriptedWire::new());
    wire.on(Method::Get, &front, answer(307, &[("location", node)], ""));
    wire.on(
        Method::Get,
        node,
        Scripted::Transport(TransportKind::Connect),
    );
    let providers = providers_over(&wire);
    for _ in 0..5 {
        fetch(&providers).await.expect_err("the node is down");
    }
    assert!(
        !providers.coverart.executor().resting_after_failures(),
        "a failing storage node does not rest the archive"
    );

    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        &front,
        answer(307, &[("location", &same_host)], ""),
    );
    wire.on(Method::Get, &same_host, answer(500, &[], ""));
    let providers = providers_over(&wire);
    for _ in 0..5 {
        fetch(&providers).await.expect_err("the archive fails");
    }
    assert!(
        providers.coverart.executor().resting_after_failures(),
        "failures on the archive's own host rest it"
    );

    let wire = Arc::new(ScriptedWire::new());
    wire.on(Method::Get, &front, answer(307, &[("location", node)], ""));
    wire.on(Method::Get, node, answer(429, &[("retry-after", "20")], ""));
    let providers = providers_over(&wire);
    fetch(&providers).await.expect_err("the node is busy");
    assert!(
        providers.limiter("coverartarchive").unwrap().cooling_for() > Duration::from_secs(10),
        "the wait the node named cools the row"
    );
}

/// The ListenBrainz rules: a policy block (the popularity 500 "currently
/// disabled", the anti-scraper 401) is a deliberate switch-off that is not
/// retried and never rests the bucket; a 429 rests it for
/// `X-RateLimit-Reset-In`, else `Retry-After`, else from a minute (jittered
/// down by at most half), without a second try; and an answer that says
/// the window is spent rests it until the window resets.
#[tokio::test(start_paused = true)]
async fn listenbrainz_hooks() {
    type Check = fn(&Result<Vec<listenbrainz::stats::ArtistStat>, ProviderError>) -> bool;
    let artists = format!(
        "{}/1/stats/sitewide/artists",
        listenbrainz::DEFAULT_BASE_URL
    );
    let rows: Vec<(&str, Scripted, Check, usize, Duration)> = vec![
        (
            "popularity switched off",
            answer(
                500,
                &[],
                r#"{"code": 500, "error": "Popularity API currently disabled due to high load"}"#,
            ),
            |result| matches!(result, Err(ProviderError::Disabled { .. })),
            1,
            Duration::ZERO,
        ),
        (
            "anti-scraper 401",
            answer(
                401,
                &[],
                r#"{"code": 401, "error": "Due to AI scrapers, please provide an Auth token"}"#,
            ),
            |result| matches!(result, Err(ProviderError::Disabled { .. })),
            1,
            Duration::ZERO,
        ),
        (
            "plain 401",
            answer(401, &[], r#"{"error": "Invalid authorization token."}"#),
            |result| matches!(result, Err(ProviderError::Unauthorized { .. })),
            1,
            Duration::ZERO,
        ),
        (
            "429 with Retry-After",
            answer(429, &[("retry-after", "7")], ""),
            |result| matches!(result, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(7)),
            1,
            Duration::from_secs(7),
        ),
        (
            "429 with X-RateLimit-Reset-In",
            answer(
                429,
                &[("x-ratelimit-reset-in", "12"), ("retry-after", "5")],
                "",
            ),
            |result| matches!(result, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(12)),
            1,
            Duration::from_secs(12),
        ),
        (
            "429 naming no wait",
            answer(429, &[], ""),
            |result| {
                matches!(
                    result,
                    Err(ProviderError::RateLimited {
                        retry_after: None,
                        ..
                    })
                )
            },
            1,
            Duration::from_secs(60),
        ),
        (
            "window spent",
            answer(
                200,
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset-in", "9"),
                ],
                r#"{"payload": {"artists": []}}"#,
            ),
            |result| matches!(result, Ok(rows) if rows.is_empty()),
            1,
            Duration::from_secs(9),
        ),
    ];
    for (name, scripted, check, sends, rest) in rows {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(Method::Get, &artists, scripted);
        let providers = providers_over(&wire);
        let result = providers
            .listenbrainz
            .sitewide_top_artists("this_week", 10, 0, RequestPriority::UserInitiated)
            .await;
        assert!(check(&result), "{name}: got {result:?}");
        assert_eq!(wire.sent().len(), sends, "{name}: requests sent");
        let cooling = providers.limiter("listenbrainz").unwrap().cooling_for();
        let jitter = if name == "429 naming no wait" {
            rest / 2
        } else {
            Duration::from_millis(600)
        };
        assert!(
            cooling <= rest && cooling + jitter >= rest,
            "{name}: rests {cooling:?}, expected {rest:?}"
        );
    }
}

/// The Last.fm rules: an error code inside an answer is read whatever the
/// status. Error 6 is "no such thing", 29 rests the bucket a second, 11
/// and 16 are a passing outage worth another try, and 10 is a refused
/// key, never tried again.
#[tokio::test(start_paused = true)]
async fn lastfm_hooks() {
    type Check = fn(&Result<Option<lastfm::ArtistInfo>, ProviderError>) -> bool;
    let envelope = |code: i64| format!(r#"{{"error": {code}, "message": "upstream says so"}}"#);
    let rows: Vec<(&str, Scripted, Check, usize, Duration)> = vec![
        (
            "error 6 in a 200",
            answer(200, &[], &envelope(6)),
            |result| matches!(result, Ok(None)),
            1,
            Duration::ZERO,
        ),
        (
            "error 29 in a 200",
            answer(200, &[], &envelope(29)),
            |result| matches!(result, Err(ProviderError::RateLimited { retry_after: Some(wait), .. }) if *wait == Duration::from_secs(1)),
            2,
            Duration::from_secs(1),
        ),
        (
            "error 11 in a 200",
            answer(200, &[], &envelope(11)),
            |result| matches!(result, Err(ProviderError::Unavailable { .. })),
            2,
            Duration::ZERO,
        ),
        (
            "error 16 in a 200",
            answer(200, &[], &envelope(16)),
            |result| matches!(result, Err(ProviderError::Unavailable { .. })),
            2,
            Duration::ZERO,
        ),
        (
            "error 10 in a 403",
            answer(403, &[], &envelope(10)),
            |result| matches!(result, Err(ProviderError::Rejected { code: 10, .. })),
            1,
            Duration::ZERO,
        ),
        (
            "an unlisted code in a 200",
            answer(200, &[], &envelope(8)),
            |result| matches!(result, Err(ProviderError::BadRequest { status: 200, .. })),
            1,
            Duration::ZERO,
        ),
    ];
    let creds = lastfm::LastFmCredentials {
        api_key: "key".to_owned(),
        ..lastfm::LastFmCredentials::default()
    };
    for (name, scripted, check, sends, rest) in rows {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(Method::Get, lastfm::DEFAULT_BASE_URL, scripted);
        let providers = providers_over(&wire);
        let result = providers
            .lastfm
            .artist_info(&creds, "Radiohead", None, RequestPriority::UserInitiated)
            .await;
        assert!(check(&result), "{name}: got {result:?}");
        assert_eq!(wire.sent().len(), sends, "{name}: requests sent");
        let cooling = providers.limiter("lastfm").unwrap().cooling_for();
        assert!(
            cooling <= rest && cooling + Duration::from_millis(600) >= rest,
            "{name}: rests {cooling:?}, expected {rest:?}"
        );
    }
}

/// TheAudioDB paces by the key saved: the free key, or none, at the free
/// row of 30 a minute; any other key at the premium row. A 429 that names
/// no wait rests the free row a minute.
#[tokio::test(start_paused = true)]
async fn audiodb_rows_follow_the_saved_key() {
    const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    let url = |key: &str| format!("{}/{key}/artist-mb.php", audiodb::DEFAULT_BASE_URL);
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(&url("123"), 200, &[], r#"{"artists": null}"#);
    wire.answer(&url("premium-key"), 200, &[], r#"{"artists": null}"#);
    let providers = providers_over(&wire);
    let free = providers.limiter("audiodb").unwrap();
    let premium = providers.limiter("audiodb-premium").unwrap();
    let user = RequestPriority::UserInitiated;

    let found = providers.audiodb.artist_by_mbid("", ARTIST, user).await;
    assert!(matches!(found, Ok(None)), "{found:?}");
    assert_eq!((free.remaining(), premium.remaining()), (0, 2));

    let found = providers
        .audiodb
        .artist_by_mbid("premium-key", ARTIST, user)
        .await;
    assert!(matches!(found, Ok(None)), "{found:?}");
    assert_eq!((free.remaining(), premium.remaining()), (0, 1));
    let paths: Vec<String> = wire
        .sent()
        .iter()
        .map(|sent| sent.url.path().to_owned())
        .collect();
    assert_eq!(
        paths,
        [
            "/api/v1/json/123/artist-mb.php",
            "/api/v1/json/premium-key/artist-mb.php"
        ]
    );

    let wire = Arc::new(ScriptedWire::new());
    wire.answer(&url("123"), 429, &[], "");
    let providers = providers_over(&wire);
    let found = providers
        .audiodb
        .artist_by_mbid("123", ARTIST, RequestPriority::BackgroundSync)
        .await;
    assert!(
        matches!(found, Err(ProviderError::RateLimited { .. })),
        "{found:?}"
    );
    let rest = providers.limiter("audiodb").unwrap().cooling_for();
    assert!(
        rest > Duration::from_secs(59) && rest <= Duration::from_secs(60),
        "rests a minute: {rest:?}"
    );
}

/// The AcoustID rules: a 4xx naming error code 4 is a refused key, a 4xx
/// naming another code waits on that code, and a 200 whose status is not
/// "ok" an outage.
#[tokio::test(start_paused = true)]
async fn acoustid_hooks() {
    let lookup = format!("{}/v2/lookup", acoustid::DEFAULT_BASE_URL);
    type Check = fn(&acoustid::BatchOutcome) -> bool;
    let rows: Vec<(&str, Scripted, Check)> = vec![
        (
            "code 4 in a 400",
            answer(
                400,
                &[],
                r#"{"status": "error", "error": {"code": 4, "message": "invalid API key"}}"#,
            ),
            |outcome| *outcome == acoustid::BatchOutcome::KeyRefused,
        ),
        (
            "another 400",
            answer(
                400,
                &[],
                r#"{"status": "error", "error": {"code": 2, "message": "missing parameter"}}"#,
            ),
            |outcome| {
                *outcome == acoustid::BatchOutcome::Unavailable(acoustid::Unanswered::Code(2))
            },
        ),
        (
            "a 200 that did not succeed",
            answer(200, &[], r#"{"status": "error"}"#),
            |outcome| matches!(outcome, acoustid::BatchOutcome::Unavailable { .. }),
        ),
    ];
    for (name, scripted, check) in rows {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(Method::Post, &lookup, scripted);
        let providers = providers_over(&wire);
        let outcome = providers
            .acoustid
            .lookup_batch_request(
                "key",
                &[acoustid::BatchQuery {
                    fingerprint: "print",
                    duration_secs: 200,
                }],
                RequestPriority::UserInitiated,
            )
            .await;
        assert!(check(&outcome), "{name}: got {outcome:?}");
    }

    // AcoustID's "too many requests" code rests the client as a 429 does.
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Post,
        &lookup,
        answer(
            400,
            &[],
            r#"{"status": "error", "error": {"code": 14, "message": "too many requests"}}"#,
        ),
    );
    let providers = providers_over(&wire);
    let outcome = providers
        .acoustid
        .lookup_batch_request(
            "key",
            &[acoustid::BatchQuery {
                fingerprint: "print",
                duration_secs: 200,
            }],
            RequestPriority::UserInitiated,
        )
        .await;
    assert_eq!(
        outcome,
        acoustid::BatchOutcome::Unavailable(acoustid::Unanswered::Paused)
    );
    assert!(
        !providers
            .limiter("acoustid")
            .expect("the AcoustID limiter")
            .cooling_for()
            .is_zero(),
        "code 14 rests AcoustID"
    );
}

/// GitHub: a 403 that says the allowance is spent rests until
/// `x-ratelimit-reset`, one that names a wait rests that long, one with no
/// rate-limit header at all rests a minute, and one that reports allowance
/// left is a refusal.
#[tokio::test(start_paused = true)]
async fn github_hooks() {
    use droppedneedle_kernel::clock::Clock as _;
    use droppedneedle_providers::github;

    let reset = (SystemClock.now_unix() + 120).to_string();
    type Check = fn(&ProviderError) -> bool;
    let rows: Vec<(&str, Scripted, Check, Duration)> = vec![
        (
            "403 with the allowance spent",
            answer(
                403,
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", &reset),
                ],
                "{}",
            ),
            |error| matches!(error, ProviderError::RateLimited { .. }),
            Duration::from_secs(120),
        ),
        (
            "403 naming a wait",
            answer(403, &[("retry-after", "30")], "{}"),
            |error| matches!(error, ProviderError::RateLimited { retry_after: Some(wait), .. } if *wait == Duration::from_secs(30)),
            Duration::from_secs(30),
        ),
        (
            "403 with no rate-limit headers (a secondary limit)",
            answer(403, &[], "{}"),
            |error| {
                matches!(
                    error,
                    ProviderError::RateLimited {
                        retry_after: None,
                        ..
                    }
                )
            },
            Duration::from_secs(60),
        ),
        (
            "plain 403",
            answer(403, &[("x-ratelimit-remaining", "12")], "{}"),
            |error| matches!(error, ProviderError::Forbidden { .. }),
            Duration::ZERO,
        ),
    ];
    for (name, scripted, check, rest) in rows {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(Method::Get, &url("releases"), scripted);
        let limiter = Arc::new(RateLimiter::new(RatePolicy::new(1000.0, 1000)));
        let executor = Executor::new(
            &github::SPEC,
            limiter.clone(),
            wire.clone(),
            Arc::new(ServiceHealth::new()),
            Arc::new(SystemClock),
        );
        let error = executor
            .send(get("releases"), RequestPriority::UserInitiated)
            .await
            .expect_err(name);
        assert!(check(&error), "{name}: got {error:?}");
        assert_eq!(wire.sent().len(), 1, "{name}: requests sent");
        let cooling = limiter.cooling_for();
        assert!(
            cooling <= rest && cooling + Duration::from_secs(2) >= rest,
            "{name}: rests {cooling:?}, expected {rest:?}"
        );
    }
}

/// Ticketmaster: a 429 naming the spent daily allowance rests until
/// `Rate-Limit-Reset` (unix milliseconds), or an hour when it names none,
/// and the key its body echoes reaches no error, answer or log line.
#[test]
fn ticketmaster_quota_hook() {
    use droppedneedle_kernel::clock::Clock as _;
    use droppedneedle_kernel::error::ApiError;

    let quota = r#"{"fault": {"faultstring": "Rate limit quota violation. Quota limit exceeded. Identifier : TM-SECRET", "detail": {"errorcode": "policies.ratelimit.QuotaViolation"}}}"#;
    let reset_ms = ((SystemClock.now_unix() + 120) * 1000).to_string();
    let rows: Vec<(&str, Scripted, Duration)> = vec![
        (
            "allowance spent, with its reset",
            answer(429, &[("rate-limit-reset", &reset_ms)], quota),
            Duration::from_secs(120),
        ),
        (
            "allowance spent, no reset named",
            answer(429, &[], quota),
            Duration::from_secs(3600),
        ),
    ];
    for (name, scripted, rest) in rows {
        let wire = Arc::new(ScriptedWire::new());
        wire.on(
            Method::Get,
            "http://127.0.0.1:9/ticketmaster/attractions.json",
            scripted,
        );
        let providers = providers_over(&wire);
        let (error, logs) = crate::common::logs::capture_logs(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(providers.ticketmaster.search_attractions(
                    "TM-SECRET",
                    "Radiohead",
                    RequestPriority::UserInitiated,
                ))
                .expect_err(name)
        });
        assert!(
            matches!(error, ProviderError::RateLimited { retry_after: Some(wait), .. } if wait <= rest && wait + Duration::from_secs(2) >= rest),
            "{name}: got {error:?}"
        );
        let answered = ApiError::from(error.clone());
        let seen = format!("{error} {error:?} {answered:?} {logs}");
        assert!(
            !seen.contains("TM-SECRET"),
            "{name}: the key leaked: {seen}"
        );
        assert_eq!(wire.sent().len(), 1, "{name}: requests sent");
        assert!(
            !providers.ticketmaster.resting_after_failures(),
            "{name}: a rest the provider asked for is not an outage"
        );
        let cooling = providers.ticketmaster.cooling_for();
        assert!(
            cooling <= rest && cooling + Duration::from_secs(2) >= rest,
            "{name}: rests {cooling:?}, expected {rest:?}"
        );
    }
}

/// A renamed repository's redirect is followed on the same host, so the
/// update check still finds its releases; a redirect to another host is
/// refused.
#[tokio::test]
async fn github_follows_a_renamed_repository() {
    let releases = "http://127.0.0.1:9/releases";
    let moved = "http://127.0.0.1:9/repositories/42/releases";
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        releases,
        answer(301, &[("location", moved)], ""),
    );
    wire.answer(
        moved,
        200,
        &[],
        r#"[{"tag_name": "v3.0.0", "draft": false, "prerelease": false}]"#,
    );
    let providers = Providers::unpaced(wire.clone());
    let found = providers
        .github
        .fetch_latest_release(RequestPriority::UserInitiated)
        .await
        .map(|release| release.tag_name);
    assert_eq!(found.as_deref(), Some("v3.0.0"));

    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        Method::Get,
        releases,
        answer(301, &[("location", "https://example.com/releases")], ""),
    );
    let providers = Providers::unpaced(wire.clone());
    assert!(
        providers
            .github
            .fetch_releases(RequestPriority::UserInitiated)
            .await
            .is_empty()
    );
    assert_eq!(wire.sent().len(), 1, "the other host is never asked");
}

/// Spotify's 429 rests the `spotify` row for the wait it names, so an
/// import pauses for Spotify instead of calling into its limit again: the
/// next call a person makes is turned down at once without a request.
#[tokio::test]
async fn a_spotify_rate_limit_rests_its_row() {
    use droppedneedle_providers::spotify;

    let wire = Arc::new(ScriptedWire::new());
    let me = format!("{}/me", spotify::API_BASE);
    wire.on(
        Method::Get,
        &me,
        answer(429, &[("retry-after", "30")], "{}"),
    );
    let providers = Providers::over_wire(
        wire.clone(),
        Arc::new(SystemClock),
        ProviderEndpoints {
            spotify_api: spotify::API_BASE.to_owned(),
            ..ProviderEndpoints::closed()
        },
    );

    let error = providers
        .spotify
        .me("token", RequestPriority::UserInitiated)
        .await
        .expect_err("rate limited");
    assert!(
        matches!(error, ProviderError::RateLimited { retry_after: Some(wait), .. } if wait == Duration::from_secs(30)),
        "{error:?}"
    );
    let cooling = providers.limiter(spotify::SOURCE).unwrap().cooling_for();
    assert!(
        cooling <= Duration::from_secs(30)
            && cooling + Duration::from_secs(2) >= Duration::from_secs(30),
        "rests {cooling:?}"
    );

    let again = providers
        .spotify
        .me("token", RequestPriority::UserInitiated)
        .await
        .expect_err("still resting");
    assert!(
        matches!(again, ProviderError::RateLimited { .. }),
        "{again:?}"
    );
    assert_eq!(wire.sent().len(), 1, "the rest sends nothing");
}

/// YouTube's refusals answer honestly. A key it does not accept (the
/// recorded 400) answers the refused-key 409 on search. A spent daily quota
/// is a 503 naming YouTube with the wait until the quota day turns at 08:00
/// UTC, and the rest of that day sends nothing.
#[tokio::test]
async fn youtube_refusals_answer_honestly() {
    use droppedneedle_kernel::clock::ManualClock;
    use droppedneedle_kernel::error::ApiError;
    use droppedneedle_providers::youtube::{YouTubeClient, YouTubeSettings, YoutubeError};

    const SEARCH: &str = "http://127.0.0.1:9/youtube/youtube/v3/search";
    let invalid_key = include_str!("../fixtures/youtube/search_invalid_key.json");
    let quota_spent = r#"{"error": {"code": 403, "message": "quota", "errors": [{"reason": "quotaExceeded", "domain": "youtube.quota"}]}}"#;
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(SEARCH, 400, &[], invalid_key);
    wire.answer(SEARCH, 403, &[], quota_spent);
    let providers = Providers::unpaced(wire.clone());
    let scratch = crate::common::ScratchDir::new("youtube-refusals");
    // 18:00 UTC: the quota day turns at 08:00 UTC, in fourteen hours.
    let clock = Arc::new(ManualClock::at((1_777_939_200 + 18 * 3600) * 1000));
    let client = YouTubeClient::new(
        providers.youtube.clone(),
        scratch.join("youtube_quota.json"),
        YouTubeSettings {
            api_key: "not-a-key".to_owned(),
            enabled: true,
            api_enabled: true,
            daily_quota_limit: 80,
        },
        clock,
        providers.cache.clone(),
    )
    .unwrap();
    let refused = |result: Result<Option<String>, YoutubeError>| match result {
        Err(YoutubeError::Provider(error)) => ApiError::from(error),
        other => panic!("not a provider failure: {other:?}"),
    };

    let answered = refused(
        client
            .search_video("Portishead", "Dummy", RequestPriority::UserInitiated)
            .await,
    );
    assert_eq!(answered.status().as_u16(), 409, "{answered:?}");
    assert_eq!(answered.code(), "PROVIDER_NOT_CONFIGURED");

    let spent = client
        .search_video("Portishead", "Third", RequestPriority::UserInitiated)
        .await;
    assert!(
        matches!(
            &spent,
            Err(YoutubeError::Provider(ProviderError::Unavailable {
                provider: "youtube",
                retry_after: Some(wait),
            })) if *wait == Duration::from_secs(14 * 3600)
        ),
        "{spent:?}"
    );
    let answered = refused(spent);
    assert_eq!(answered.status().as_u16(), 503, "{answered:?}");
    assert_eq!(answered.code(), "PROVIDER_UNAVAILABLE");

    assert!(matches!(
        client
            .search_video("Portishead", "Roseland", RequestPriority::UserInitiated)
            .await,
        Err(YoutubeError::QuotaExhausted)
    ));
    assert_eq!(wire.sent().len(), 2, "nothing more is sent today");
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

/// A Newznab indexer at its daily cap is not called again until the cap
/// frees up: a day after the oldest call it counts, as its answer says.
#[tokio::test]
async fn a_newznab_indexer_at_its_daily_cap_waits_for_the_reset() {
    use droppedneedle_kernel::clock::ManualClock;
    use droppedneedle_providers::newznab::NewznabError;

    const NOW: i64 = 1_800_000_000;
    // An hour before NOW.
    const OLDEST: &str = "Fri, 15 Jan 2027 07:00:00 GMT";
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        "https://indexer.test/api",
        200,
        &[],
        format!(
            r#"<rss xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel>
<newznab:apilimits apiCurrent="100" apiMax="100" grabCurrent="3" grabMax="50" apiOldestTime="{OLDEST}"/>
</channel></rss>"#
        ),
    );
    let clock = Arc::new(ManualClock::at(NOW * 1000));
    let providers = Providers::over_wire(wire.clone(), clock.clone(), ProviderEndpoints::closed());
    let client = providers
        .newznab
        .client("idx", "Indexer", "https://indexer.test/api", "KEY");
    let search = || client.search("Radiohead", &[], 0, 100, RequestPriority::UserInitiated);

    let (releases, _) = search().await.expect("the answer that reports the cap");
    assert!(releases.is_empty());
    assert_eq!(wire.sent().len(), 1);

    let refusals = [
        search().await.map(|_| ()),
        client
            .caps(RequestPriority::BackgroundSync)
            .await
            .map(|_| ()),
    ];
    for refused in refusals {
        match refused {
            Err(NewznabError::Provider(ProviderError::RateLimited {
                retry_after: Some(wait),
                ..
            })) => assert_eq!(wait, Duration::from_secs(23 * 3600)),
            other => panic!("not held to the cap: {other:?}"),
        }
    }
    assert_eq!(wire.sent().len(), 1, "nothing sent while the cap holds");

    clock.advance(23 * 3600 * 1000);
    search().await.expect("the cap freed up");
    assert_eq!(wire.sent().len(), 2);
}
