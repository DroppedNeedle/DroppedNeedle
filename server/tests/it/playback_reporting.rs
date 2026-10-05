//! Playback reporting over the port fakes: the start, progress and stop
//! lifecycle, the scrobble threshold rules, dedup across duplicate and
//! mixed reports, presence visibility, the session gate, hidden faults,
//! and the MBID warmup loop cadence.

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
    FakeCatalog, FakeHistory, FakeNames, FakePrefs, FakeRemotes, FakeSinks, FakeWarmup, ManualClock,
};
use playback::ports::{Clock, ScrobblePrefs};
use playback::services::{
    MixedDedup, PresenceRegistry, SESSION_TTL_SECS, ScrobbleDedup, SessionKey, SessionStore,
    meets_subsonic_threshold, presence_key, should_scrobble_native, submit_session_scrobble,
    subsonic_scrobble_threshold_ms,
};
use playback::warmup::{ManualSleeper, WarmupRegistry, WarmupScope, run_warmup_loop};
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
}

fn rig_with_sinks(sinks: FakeSinks) -> Rig {
    let clock = ManualClock::new(NOW);
    let sinks = Arc::new(sinks);
    let remotes = Arc::new(FakeRemotes::default());
    let history = Arc::new(FakeHistory::default());
    let prefs = Arc::new(FakePrefs::default());
    let deps = PlaybackDeps {
        catalog: Arc::new(FakeCatalog::with_tracks(vec![
            FakeCatalog::sample_track(),
            FakeCatalog::short_track(),
        ])),
        sinks: sinks.clone(),
        remotes: remotes.clone(),
        history: history.clone(),
        prefs: prefs.clone(),
        names: Arc::new(FakeNames::default()),
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

// ---------------------------------------------------------------------------
// Remote attribution, errors, auth
// ---------------------------------------------------------------------------

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

/// `track_hidden` users stay out of the Subsonic now-playing list.
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
