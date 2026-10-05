//! Playback-reporting briefs: session lifecycle, the scrobble threshold
//! rules, now-playing presence, remote attribution, and the MBID warmup
//! loops.
//!
//! Each test pins one behavior. The router runs against the slice fakes
//! with the test-principal layer standing in for the sibling session
//! middleware (auth resolution only). Loop briefs drive the manual
//! sleeper: no real waits anywhere.

use droppedneedle::playback;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::auth::users::memory::with_test_principal;
use droppedneedle::ids::IdGenerator;
use playback::PlaybackDeps;
use playback::error::PlaybackError;
use playback::fakes::{
    FakeCatalog, FakeHistory, FakeNames, FakePrefs, FakeRemotes, FakeSinks, FakeWarmup,
    ManualClock, fake_work,
};
use playback::handlers::AuthenticatedUser;
use playback::ports::{Clock, ScrobblePrefs};
use playback::services::{
    MixedDedup, PresenceRegistry, SESSION_TTL_SECS, ScrobbleDedup, SessionKey, SessionStore,
    meets_subsonic_threshold, presence_key, should_scrobble_native, submit_session_scrobble,
    subsonic_scrobble_threshold_ms,
};
use playback::warmup::{
    ManualSleeper, Sleeper, TokioSleeper, WarmupRegistry, WarmupScope, WarmupStats,
    run_warmup_loop, spawn_warmup_loops, warmup_once,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// Fixed id generator so error ids assert stably.
#[derive(Debug, Clone)]
struct FixedIds;

impl IdGenerator for FixedIds {
    fn new_id(&self) -> String {
        "123e4567-e89b-12d3-a456-426614174000".to_owned()
    }
}

const NOW: i64 = 1_790_000_000;

struct Rig {
    deps: PlaybackDeps,
    clock: ManualClock,
    sinks: Arc<FakeSinks>,
    remotes: Arc<FakeRemotes>,
    history: Arc<FakeHistory>,
    prefs: Arc<FakePrefs>,
    names: Arc<FakeNames>,
}

fn rig_with_sinks(sinks: FakeSinks) -> Rig {
    let clock = ManualClock::new(NOW);
    let sinks = Arc::new(sinks);
    let remotes = Arc::new(FakeRemotes::default());
    let history = Arc::new(FakeHistory::default());
    let prefs = Arc::new(FakePrefs::default());
    let names = Arc::new(FakeNames::default());
    let deps = PlaybackDeps {
        catalog: Arc::new(FakeCatalog::with_tracks(vec![
            FakeCatalog::sample_track(),
            FakeCatalog::short_track(),
        ])),
        sinks: sinks.clone(),
        remotes: remotes.clone(),
        history: history.clone(),
        prefs: prefs.clone(),
        names: names.clone(),
        sessions: SessionStore::new(),
        presence: PresenceRegistry::new(),
        dedup: ScrobbleDedup::new(),
        mixed: MixedDedup::new(),
        clock: Arc::new(clock.clone()),
        ids: Arc::new(FixedIds),
    };
    Rig {
        deps,
        clock,
        sinks,
        remotes,
        history,
        prefs,
        names,
    }
}

fn rig() -> Rig {
    rig_with_sinks(FakeSinks::accepting())
}

fn principal(user_id: &str) -> CurrentSession {
    CurrentSession {
        user_id: user_id.to_owned(),
        session_id: "sess-test".to_owned(),
        kind: SessionKind::Standard,
        transport: Transport::Bearer,
    }
}

fn app(rig: &Rig, user: Option<&str>) -> Router {
    let router = Router::new().nest("/api/v3", playback::playback_router(rig.deps.clone()));
    match user {
        Some(id) => with_test_principal(router, principal(id)),
        None => router,
    }
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, headers)
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("<no-code>")
}

async fn start(rig: &Rig, user: &str, track: &str, source: &str) -> (StatusCode, Value) {
    let (status, body, _) = call(
        app(rig, Some(user)),
        "POST",
        "/api/v3/playback/start",
        Some(json!({"track_id": track, "source": source, "device": "web"})),
    )
    .await;
    (status, body)
}

async fn progress(
    rig: &Rig,
    user: &str,
    track: &str,
    position_ms: Option<i64>,
    is_paused: bool,
) -> (StatusCode, Value) {
    let (status, body, _) = call(
        app(rig, Some(user)),
        "POST",
        "/api/v3/playback/progress",
        Some(json!({
            "track_id": track,
            "source": "local",
            "device": "web",
            "position_ms": position_ms,
            "is_paused": is_paused,
        })),
    )
    .await;
    (status, body)
}

async fn stop(
    rig: &Rig,
    user: &str,
    track: &str,
    source: &str,
    position_ms: Option<i64>,
) -> (StatusCode, Value) {
    let (status, body, _) = call(
        app(rig, Some(user)),
        "POST",
        "/api/v3/playback/stop",
        Some(json!({
            "track_id": track,
            "source": source,
            "device": "web",
            "position_ms": position_ms,
        })),
    )
    .await;
    (status, body)
}

async fn snapshot(rig: &Rig, user: &str) -> Value {
    let (_, body, _) = call(app(rig, Some(user)), "GET", "/api/v3/now-playing", None).await;
    body
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_start_progress_stop_counts_the_play() {
    let rig = rig();
    let (status, body) = start(&rig, "user-1", "track-1", "local").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["session"], "user-1:web:track-1");

    let (status, _) = progress(&rig, "user-1", "track-1", Some(120_000), false).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["scrobbled"], true);

    assert_eq!(rig.history.records().len(), 1);
    assert_eq!(rig.sinks.scrobble_calls().len(), 1);
    assert_eq!(rig.sinks.now_playing_calls().len(), 1);
    assert!(rig.remotes.calls().is_empty());
    assert_eq!(
        snapshot(&rig, "user-1").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(rig.deps.sessions.len(NOW), 1);
    rig.clock.advance(SESSION_TTL_SECS + 1);
    assert_eq!(rig.deps.sessions.len(rig.clock.now_unix()), 0);
}

#[tokio::test]
async fn progress_never_forwards_a_scrobble() {
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    progress(&rig, "user-1", "track-1", Some(60_000), false).await;
    progress(&rig, "user-1", "track-1", Some(120_000), true).await;

    assert!(rig.sinks.scrobble_calls().is_empty());
    assert!(rig.history.records().is_empty());
    let sessions = snapshot(&rig, "user-1").await["sessions"].clone();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["is_paused"], true);
    assert_eq!(sessions[0]["progress_ms"], 120_000);
    assert_eq!(sessions[0]["track_name"], "Roads");
}

#[tokio::test]
async fn stop_below_threshold_skips_but_clears_presence() {
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (status, body) = stop(&rig, "user-1", "track-1", "local", Some(100_000)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scrobbled"], false);

    assert!(rig.history.records().is_empty());
    assert!(rig.sinks.scrobble_calls().is_empty());
    assert_eq!(
        snapshot(&rig, "user-1").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn stop_without_position_counts_the_play() {
    // Omitted position counts (v2 "reference s6" quirk): a client that sends
    // no position is trusted to have finished.
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (_, body) = stop(&rig, "user-1", "track-1", "local", None).await;
    assert_eq!(body["scrobbled"], true);
    assert_eq!(rig.history.records().len(), 1);
}

#[tokio::test]
async fn stop_counts_once_per_session() {
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (_, first) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(first["scrobbled"], true);
    let (_, second) = stop(&rig, "user-1", "track-1", "local", Some(235_000)).await;
    assert_eq!(second["scrobbled"], false);
    assert_eq!(rig.history.records().len(), 1);
}

#[tokio::test]
async fn restart_after_rewind_opens_a_fresh_session() {
    // A stopped session that rewinds restarts fresh (v2 rewind branch), so
    // replaying the track counts again.
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (_, first) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(first["scrobbled"], true);
    rig.clock.advance(300);
    progress(&rig, "user-1", "track-1", Some(10_000), false).await;
    let (_, second) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(second["scrobbled"], true);
    assert_eq!(rig.history.records().len(), 2);
}

#[tokio::test]
async fn ignore_scrobble_skips_counting() {
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (status, body, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/playback/stop",
        Some(json!({
            "track_id": "track-1",
            "source": "local",
            "device": "web",
            "position_ms": 230_000,
            "ignore_scrobble": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scrobbled"], false);
    assert!(rig.history.records().is_empty());
}

// ---------------------------------------------------------------------------
// Threshold rules
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scrobble_threshold_rules() {
    // Native rule: the v2 Jellyfin stop rule, exact.
    assert!(should_scrobble_native(None, Some(200_000)));
    assert!(should_scrobble_native(None, None));
    assert!(should_scrobble_native(Some(181_000), Some(200_000)));
    assert!(!should_scrobble_native(Some(180_000), Some(200_000)));
    // Exactly 90% still counts inside the last second (the within-1s
    // clause firing where the percent clause does not).
    assert!(should_scrobble_native(Some(9_000), Some(10_000)));
    assert!(!should_scrobble_native(Some(8_999), Some(10_000)));
    assert!(!should_scrobble_native(Some(100_000), Some(200_000)));
    assert!(!should_scrobble_native(Some(100_000), None));
    assert!(!should_scrobble_native(Some(100_000), Some(0)));

    // Subsonic rule: half the track capped at four minutes, four minutes
    // flat for unknown durations.
    assert_eq!(subsonic_scrobble_threshold_ms(600_000), 240_000);
    assert_eq!(subsonic_scrobble_threshold_ms(200_000), 100_000);
    assert_eq!(subsonic_scrobble_threshold_ms(0), 240_000);
    assert!(meets_subsonic_threshold(100_000, 200_000));
    assert!(!meets_subsonic_threshold(99_999, 200_000));
}

// ---------------------------------------------------------------------------
// Native submit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn short_tracks_record_without_forwarding() {
    let rig = rig();
    start(&rig, "user-1", "track-short", "local").await;
    let (_, body) = stop(&rig, "user-1", "track-short", "local", Some(19_000)).await;
    assert_eq!(body["scrobbled"], true);
    assert_eq!(rig.history.records().len(), 1);
    assert!(rig.sinks.scrobble_calls().is_empty());
}

#[tokio::test]
async fn duplicate_submits_dedup() {
    let rig = rig();
    let body = json!({
        "track_name": "Roads",
        "artist_name": "Portishead",
        "timestamp": NOW,
        "album_name": "Dummy",
        "duration_ms": 240_000,
    });
    for _ in 0..2 {
        let (status, answer, _) = call(
            app(&rig, Some("user-1")),
            "POST",
            "/api/v3/scrobble/submit",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["accepted"], true);
    }
    assert_eq!(rig.history.records().len(), 1);
    assert_eq!(rig.sinks.scrobble_calls().len(), 1);
}

#[tokio::test]
async fn submit_validation_rejects_bad_input() {
    let rig = rig();
    for body in [
        json!({"track_name": "T", "artist_name": "A", "timestamp": NOW + 61}),
        json!({"track_name": "T", "artist_name": "A", "timestamp": NOW - 14 * 86400 - 1}),
        json!({"track_name": "T", "artist_name": "A", "timestamp": NOW, "duration_ms": -1}),
    ] {
        let (status, answer, _) = call(
            app(&rig, Some("user-1")),
            "POST",
            "/api/v3/scrobble/submit",
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error_code(&answer), "INVALID_INPUT");
    }
    assert!(rig.history.records().is_empty());
}

#[tokio::test]
async fn navidrome_delegation_skips_forwarding_but_records() {
    let rig = rig();
    rig.prefs.set_scrobble(
        "user-1",
        ScrobblePrefs {
            scrobble_to_lastfm: true,
            scrobble_to_listenbrainz: true,
            navidrome_handles_external: true,
        },
    );
    let (status, answer, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/scrobble/submit",
        Some(json!({
            "track_name": "Roads",
            "artist_name": "Portishead",
            "timestamp": NOW,
            "duration_ms": 240_000,
            "source": "Navidrome",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["accepted"], true);
    assert_eq!(rig.history.records().len(), 1);
    assert!(rig.sinks.scrobble_calls().is_empty());
}

#[tokio::test]
async fn now_playing_forward_accepted_quirk() {
    // No linked account: accepted is false (the v2 submit/forward split:
    // submits count locally and accept, forwards have nowhere to go).
    let unlinked = rig_with_sinks(FakeSinks::unlinked());
    let (status, answer, _) = call(
        app(&unlinked, Some("user-1")),
        "POST",
        "/api/v3/scrobble/now-playing",
        Some(json!({"track_name": "Roads", "artist_name": "Portishead"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["accepted"], false);

    let linked = rig();
    let (status, answer, _) = call(
        app(&linked, Some("user-1")),
        "POST",
        "/api/v3/scrobble/now-playing",
        Some(json!({"track_name": "Roads", "artist_name": "Portishead"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["accepted"], true);
    assert_eq!(answer["services"]["lastfm"]["success"], true);
}

#[tokio::test]
async fn mixed_report_dedup_counts_live_once() {
    let rig = rig();
    let track = FakeCatalog::sample_track();
    let key = SessionKey {
        user_id: "user-1".to_owned(),
        client: "web".to_owned(),
        track_id: track.track_id.clone(),
    };
    let presence = presence_key("user-1", "web");
    let first = submit_session_scrobble(
        &rig.deps, "user-1", &key, &presence, &track, None, None, NOW,
    );
    assert!(first.accepted);
    let second = submit_session_scrobble(
        &rig.deps, "user-1", &key, &presence, &track, None, None, NOW,
    );
    assert!(second.accepted);
    assert!(second.services.is_empty());
    assert_eq!(rig.history.records().len(), 1);

    // Backdated plays bypass the mixed cache (v2 exact); a distinct play
    // time counts again.
    let third = submit_session_scrobble(
        &rig.deps,
        "user-1",
        &key,
        &presence,
        &track,
        None,
        Some(NOW - 100),
        NOW,
    );
    assert!(third.accepted);
    assert_eq!(rig.history.records().len(), 2);
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn presence_heartbeat_snapshot_and_clear() {
    let rig = rig();
    rig.names.set("user-1", "Molly");
    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/now-playing",
        Some(json!({
            "track_name": "Roads",
            "artist_name": "Portishead",
            "album_name": "Dummy",
            "progress_ms": 61_000,
            "duration_ms": 295_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let body = snapshot(&rig, "user-1").await;
    assert_eq!(body["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(body["sessions"][0]["id"], "user-1:web");
    assert_eq!(body["sessions"][0]["user_name"], "Molly");
    assert_eq!(body["sessions"][0]["track_name"], "Roads");
    assert_eq!(body["sessions"][0]["device_name"], "Web");
    assert_eq!(body["sessions"][0]["redacted"], false);

    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "DELETE",
        "/api/v3/now-playing?device=web",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        snapshot(&rig, "user-1").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn presence_visibility_redacts_and_omits() {
    let rig = rig();
    rig.prefs.set_visibility("user-hidden", "track_hidden");
    rig.prefs.set_visibility("user-offline", "offline");
    rig.prefs.fail_visibility("user-broken");
    for user in ["user-hidden", "user-offline", "user-broken"] {
        let (status, _, _) = call(
            app(&rig, Some(user)),
            "POST",
            "/api/v3/now-playing",
            Some(json!({
                "track_name": "Roads",
                "artist_name": "Portishead",
                "progress_ms": 10_000,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let body = snapshot(&rig, "user-1").await;
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    for entry in sessions {
        assert_eq!(entry["track_name"], "");
        assert_eq!(entry["artist_name"], "");
        assert_eq!(entry["redacted"], true);
        assert_eq!(entry["progress_ms"], 10_000);
    }
    assert!(
        sessions
            .iter()
            .all(|entry| entry["id"] != "user-offline:web")
    );
}

#[tokio::test]
async fn presence_sweep_drops_stale_and_idle_reconcile_stays_silent() {
    let rig = rig();
    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/now-playing",
        Some(json!({"track_name": "Roads", "artist_name": "Portishead"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let published = rig.deps.presence.generation();

    rig.deps.presence.reconcile_source("plex", vec![], NOW);
    assert_eq!(rig.deps.presence.generation(), published);

    rig.clock.advance(46);
    let removed = rig.deps.presence.sweep(rig.clock.now_unix());
    assert_eq!(removed, 1);
    assert_eq!(rig.deps.presence.generation(), published + 1);
    assert_eq!(
        snapshot(&rig, "user-1").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn compat_now_playing_serves_full_sessions_only() {
    let rig = rig();
    rig.prefs.set_visibility("user-hidden", "track_hidden");
    start(&rig, "user-1", "track-1", "local").await;
    start(&rig, "user-hidden", "track-1", "local").await;

    let served = rig.deps.presence.compat_now_playing();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].1, "track-1");
    assert_eq!(served[0].0.track_name, "Roads");
}

// ---------------------------------------------------------------------------
// Remote attribution, errors, auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remote_attribution_per_source_and_failures_swallowed() {
    let jelly = rig();
    start(&jelly, "user-1", "track-1", "jellyfin").await;
    let (status, _, _) = call(
        app(&jelly, Some("user-1")),
        "POST",
        "/api/v3/playback/progress",
        Some(json!({
            "track_id": "track-1",
            "source": "jellyfin",
            "device": "web",
            "position_ms": 120_000,
            "is_paused": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = stop(&jelly, "user-1", "track-1", "jellyfin", Some(230_000)).await;
    assert_eq!(body["scrobbled"], true);

    let ops: Vec<(String, String)> = jelly
        .remotes
        .calls()
        .iter()
        .map(|call| (call.op.clone(), call.source.clone()))
        .collect();
    assert_eq!(
        ops,
        vec![
            ("start".to_owned(), "jellyfin".to_owned()),
            ("progress".to_owned(), "jellyfin".to_owned()),
            ("stop".to_owned(), "jellyfin".to_owned()),
            ("scrobble".to_owned(), "jellyfin".to_owned()),
        ]
    );

    // A failing remote never fails the report.
    let failing = rig();
    failing.remotes.fail("plex", "stop", "plex is down");
    start(&failing, "user-1", "track-1", "plex").await;
    let (status, body) = stop(&failing, "user-1", "track-1", "plex", Some(230_000)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scrobbled"], true);
}

#[tokio::test]
async fn unknown_track_404s_in_envelope() {
    let rig = rig();
    let (status, body) = start(&rig, "user-1", "track-missing", "local").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "NOT_FOUND");
    assert_eq!(body["error"]["message"], "Not found");
}

#[tokio::test]
async fn reporting_requires_session() {
    let rig = rig();
    let (status, body, headers) = call(app(&rig, None), "GET", "/api/v3/now-playing", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
    assert_eq!(
        headers
            .get(axum::http::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    let (status, body, _) = call(
        app(&rig, None),
        "POST",
        "/api/v3/playback/start",
        Some(json!({"track_id": "track-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
}

// ---------------------------------------------------------------------------
// Warmup loops (manual sleeper: no real sleeps)
// ---------------------------------------------------------------------------

async fn wait_for_parks(sleeper: &ManualSleeper, count: usize) {
    for _ in 0..200 {
        if sleeper.waits() == count {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(sleeper.waits(), count);
}

async fn wait_for_passes(fake: &FakeWarmup, count: usize) {
    for _ in 0..200 {
        if fake.passes().len() >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(fake.passes().len(), count);
}

#[tokio::test]
async fn warmup_loops_use_honest_cadences() {
    for (scope, delay) in [
        (WarmupScope::Jellyfin, Duration::from_secs(8)),
        (WarmupScope::Navidrome, Duration::from_secs(12)),
        (WarmupScope::Plex, Duration::from_secs(15)),
    ] {
        let registry = Arc::new(WarmupRegistry::new());
        let sleeper = ManualSleeper::new();
        let fake = Arc::new(FakeWarmup::default());
        let task_registry = registry.clone();
        let task_sleeper = sleeper.clone();
        let task_fake = fake.clone();
        let task = tokio::spawn(async move {
            run_warmup_loop(task_registry, task_sleeper, scope, move || {
                let task_fake = task_fake.clone();
                async move { task_fake.run(scope) }
            })
            .await;
        });

        wait_for_parks(&sleeper, 1).await;
        assert_eq!(sleeper.requested(), vec![delay]);
        sleeper.wake();
        wait_for_passes(&fake, 1).await;
        assert_eq!(fake.passes(), vec![scope.key().to_owned()]);

        if scope == WarmupScope::Jellyfin {
            // One-shot: the loop returns after its first pass.
            task.await.unwrap();
            assert_eq!(sleeper.requested(), vec![delay]);
        } else {
            // Periodic: the loop parks on the four-hour interval next.
            wait_for_parks(&sleeper, 1).await;
            assert_eq!(
                sleeper.requested(),
                vec![delay, Duration::from_secs(14_400)]
            );
            sleeper.wake();
            wait_for_passes(&fake, 2).await;
            sleeper.shut_down();
            task.await.unwrap();
        }
        assert!(!registry.is_live(scope));
    }
}

#[tokio::test]
async fn warmup_failures_log_and_continue() {
    let registry = Arc::new(WarmupRegistry::new());
    let sleeper = ManualSleeper::new();
    let fake = Arc::new(FakeWarmup::default());
    fake.fail(WarmupScope::Navidrome, "navidrome is down");
    let task_registry = registry.clone();
    let task_sleeper = sleeper.clone();
    let task_fake = fake.clone();
    let task = tokio::spawn(async move {
        run_warmup_loop(
            task_registry,
            task_sleeper,
            WarmupScope::Navidrome,
            move || {
                let task_fake = task_fake.clone();
                async move { task_fake.run(WarmupScope::Navidrome) }
            },
        )
        .await;
    });

    wait_for_parks(&sleeper, 1).await;
    sleeper.wake();
    wait_for_passes(&fake, 1).await;
    wait_for_parks(&sleeper, 1).await;
    sleeper.wake();
    wait_for_passes(&fake, 2).await;
    assert!(!registry.is_live(WarmupScope::Navidrome));
    sleeper.shut_down();
    task.await.unwrap();
}

#[tokio::test]
async fn spawn_starts_three_loops_behind_one_registry() {
    let sleeper = ManualSleeper::new();
    let fake = Arc::new(FakeWarmup::default());
    let handles = spawn_warmup_loops(
        sleeper.clone(),
        fake_work(fake.clone(), WarmupScope::Jellyfin),
        fake_work(fake.clone(), WarmupScope::Navidrome),
        fake_work(fake.clone(), WarmupScope::Plex),
    );
    assert_eq!(handles.tasks.len(), 3);

    wait_for_parks(&sleeper, 3).await;
    let mut requested = sleeper.requested();
    requested.sort();
    assert_eq!(
        requested,
        vec![
            Duration::from_secs(8),
            Duration::from_secs(12),
            Duration::from_secs(15),
        ]
    );

    sleeper.shut_down();
    for task in handles.tasks {
        task.await.unwrap();
    }
    assert!(fake.passes().is_empty());
    assert!(!handles.registry.is_live(WarmupScope::Jellyfin));
    assert!(!handles.registry.is_live(WarmupScope::Navidrome));
    assert!(!handles.registry.is_live(WarmupScope::Plex));
}

#[tokio::test]
async fn set_visibility_normalizes_unknown_values() {
    let rig = rig();
    let (status, _, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/now-playing",
        Some(json!({"track_name": "Roads", "artist_name": "Portishead"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    rig.deps.presence.set_visibility("user-1", "bogus-value");
    let body = snapshot(&rig, "user-1").await;
    assert_eq!(body["sessions"][0]["track_name"], "Roads");
    assert_eq!(body["sessions"][0]["redacted"], false);
}

#[tokio::test]
async fn server_faults_hide_causes() {
    use axum::response::IntoResponse as _;

    let ids = FixedIds;
    for error in [
        PlaybackError::internal(&"db file is gone", &ids),
        PlaybackError::upstream(&"plex://10.0.0.9 blew up", &ids),
    ] {
        let response = error.into_response();
        assert!(response.status().is_server_error());
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!body["error"]["message"].as_str().unwrap().contains('/'));
        assert_eq!(
            body["error"]["details"]["error_id"],
            "123e4567-e89b-12d3-a456-426614174000"
        );
    }
}

#[tokio::test]
async fn authenticated_user_carries_session_identity() {
    let request = Request::builder().uri("/").body(()).unwrap();
    let (mut parts, _) = request.into_parts();
    parts.extensions.insert(principal("user-1"));
    let user = AuthenticatedUser::require(&parts).unwrap();
    assert_eq!(user.user_id, "user-1");
    assert_eq!(user.session_id, "sess-test");
}

#[tokio::test]
async fn production_clock_and_sleeper_cover_shutdown() {
    let clock = playback::ports::SystemClock;
    assert!(clock.now_unix() > NOW);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let sleeper = TokioSleeper::new(shutdown_rx);
    shutdown_tx.send(true).unwrap();
    assert!(!sleeper.sleep(Duration::from_secs(14_400)).await);
}

#[tokio::test]
async fn warmup_single_flight_skips() {
    let registry = WarmupRegistry::new();
    assert!(registry.begin(WarmupScope::Plex));
    let skipped = warmup_once(&registry, WarmupScope::Plex, || async {
        Ok(WarmupStats {
            scope: "plex",
            warmed: 1,
            pruned: 0,
        })
    })
    .await;
    assert!(!skipped);
    registry.finish(WarmupScope::Plex);
    let ran = warmup_once(&registry, WarmupScope::Plex, || async {
        Ok(WarmupStats {
            scope: "plex",
            warmed: 1,
            pruned: 0,
        })
    })
    .await;
    assert!(ran);
}
