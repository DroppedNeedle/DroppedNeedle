//! Navidrome playlist-sync briefs: the route plus the background loop.
//!
//! The route takes no body (the target comes from saved settings, so no
//! caller can name an arbitrary directory) and reports the exporter's counts
//! verbatim. The loop polls on its cadence, skips quietly while sync is off,
//! and keeps cycling through exporter failures with backoff. Nothing here
//! writes a real file; the exporter is scripted.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use droppedneedle::jobs::playlist_sync::{
    self, PlaylistExporter, PlaylistSyncConfig, PlaylistSyncResult, PlaylistSyncSettings,
    PlaylistSyncState, SyncRoles,
};
use droppedneedle::jobs::registry::{BoxFuture, JobRegistry, MemoryRegistryStore};
use droppedneedle::jobs::schedule::Schedule;
use tower::ServiceExt as _;

type TestRegistry = JobRegistry<MemoryRegistryStore>;

fn registry() -> TestRegistry {
    JobRegistry::new(MemoryRegistryStore::new())
}

async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

/// Scripted settings: the config under test, flippable between calls.
#[derive(Clone, Default)]
struct FakeSettings {
    config: Arc<Mutex<Option<PlaylistSyncConfig>>>,
    reads: Arc<AtomicU64>,
}

impl FakeSettings {
    fn enabled() -> Self {
        Self {
            config: Arc::new(Mutex::new(Some(PlaylistSyncConfig {
                target_dir: "/music/playlists".to_owned(),
                scope: "public".to_owned(),
                remove_deleted: true,
            }))),
            reads: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl PlaylistSyncSettings for FakeSettings {
    fn sync_config(&self) -> BoxFuture<'_, Option<PlaylistSyncConfig>> {
        let config = Arc::clone(&self.config);
        let reads = Arc::clone(&self.reads);
        Box::pin(async move {
            reads.fetch_add(1, Ordering::SeqCst);
            config.lock().unwrap().clone()
        })
    }
}

/// Scripted exporter: answers from a rotating script, remembers configs.
#[derive(Clone)]
struct FakeExporter {
    script: Arc<Mutex<Vec<PlaylistSyncResult>>>,
    calls: Arc<AtomicU64>,
    seen: Arc<Mutex<Vec<PlaylistSyncConfig>>>,
}

impl FakeExporter {
    fn answering(first: PlaylistSyncResult) -> Self {
        Self {
            script: Arc::new(Mutex::new(vec![first])),
            calls: Arc::new(AtomicU64::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn ok() -> PlaylistSyncResult {
        PlaylistSyncResult {
            success: true,
            message: "3 written, 12 unchanged".to_owned(),
            written: 3,
            unchanged: 12,
            ..PlaylistSyncResult::default()
        }
    }
}

impl PlaylistExporter for FakeExporter {
    fn sync(&self, config: PlaylistSyncConfig) -> BoxFuture<'_, PlaylistSyncResult> {
        let script = Arc::clone(&self.script);
        let calls = Arc::clone(&self.calls);
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            seen.lock().unwrap().push(config);
            let guard = script.lock().unwrap();
            guard.first().cloned().unwrap_or_default()
        })
    }
}

/// Fixed role map for the admin gate: brenda administers, molly does not.
#[derive(Clone, Default)]
struct FakeRoles;

impl SyncRoles for FakeRoles {
    fn role_of(&self, user_id: &str) -> BoxFuture<'_, Option<Role>> {
        let role = match user_id {
            "user-brenda" => Some(Role::Admin),
            "user-molly" => Some(Role::User),
            _ => None,
        };
        Box::pin(async move { role })
    }
}

fn route_state(
    settings: FakeSettings,
    exporter: FakeExporter,
) -> PlaylistSyncState<FakeSettings, FakeExporter> {
    PlaylistSyncState {
        settings,
        exporter,
        roles: Arc::new(FakeRoles),
    }
}

fn post_sync() -> Request<Body> {
    post_sync_as(Some("user-brenda"))
}

/// One route call, optionally carrying a stashed session. The session gate
/// normally stashes it; here the test does.
fn post_sync_as(user_id: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/navidrome/playlist-sync")
        .body(Body::empty())
        .expect("request builds");
    if let Some(user_id) = user_id {
        request.extensions_mut().insert(CurrentSession {
            user_id: user_id.to_owned(),
            session_id: "sess-1".to_owned(),
            kind: SessionKind::Standard,
            transport: Transport::Bearer,
        });
    }
    request
}

async fn read_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body reads");
    serde_json::from_slice(&bytes).expect("body is JSON")
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_disabled_returns_the_hint_and_exports_nothing() {
    let settings = FakeSettings::default();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    let app = playlist_sync::router(route_state(settings, exporter.clone()));

    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["success"], false);
    assert_eq!(
        body["message"],
        "Playlist sync is turned off. Enable it and save first."
    );
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn route_enabled_returns_exporter_counts() {
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(PlaylistSyncResult {
        success: true,
        message: "2 written, 1 removed".to_owned(),
        written: 2,
        unchanged: 5,
        removed: 1,
        removal_failures: 0,
        skipped_empty: 1,
        skipped_not_ours: 4,
        tracks_missing_files: 2,
        tracks_unrepresentable: 0,
    });
    let app = playlist_sync::router(route_state(settings, exporter.clone()));

    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["written"], 2);
    assert_eq!(body["unchanged"], 5);
    assert_eq!(body["removed"], 1);
    assert_eq!(body["skipped_empty"], 1);
    assert_eq!(body["skipped_not_ours"], 4);
    assert_eq!(body["tracks_missing_files"], 2);
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 1);
    // The target came from settings, never from the (empty) body.
    assert_eq!(
        exporter.seen.lock().unwrap()[0].target_dir,
        "/music/playlists"
    );
}

#[tokio::test]
async fn route_reads_settings_fresh_each_call() {
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    let app = playlist_sync::router(route_state(settings.clone(), exporter.clone()));

    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(read_json(response).await["success"], true);

    // Flip sync off between calls: the next call sees it at once.
    *settings.config.lock().unwrap() = None;
    let app = playlist_sync::router(route_state(settings, exporter.clone()));
    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(read_json(response).await["success"], false);
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn route_refuses_everyone_but_admins() {
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    let app = playlist_sync::router(route_state(settings, exporter.clone()));

    // No session at all: 401 with the Bearer challenge.
    let response = app
        .oneshot(post_sync_as(None))
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );

    // A signed-in non-admin: 403, and the exporter never runs.
    let settings = FakeSettings::enabled();
    let app = playlist_sync::router(route_state(settings, exporter.clone()));
    let response = app
        .oneshot(post_sync_as(Some("user-molly")))
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "FORBIDDEN");

    // A session whose account is gone: 401, like logged-out.
    let settings = FakeSettings::enabled();
    let app = playlist_sync::router(route_state(settings, exporter.clone()));
    let response = app
        .oneshot(post_sync_as(Some("user-ghost")))
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// Loop
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn loop_skips_quietly_while_disabled() {
    let registry = registry();
    let settings = FakeSettings::default();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    playlist_sync::spawn_on(
        &registry,
        settings,
        exporter.clone(),
        Schedule::new(Duration::from_secs(300)),
    )
    .await
    .expect("spawn wins");
    settle().await;

    // Several intervals pass; the exporter never hears about them.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(300)).await;
        settle().await;
    }
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 0);
    assert!(registry.is_running(playlist_sync::JOB_NAME));

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn loop_syncs_on_interval_once_enabled() {
    let registry = registry();
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    playlist_sync::spawn_on(
        &registry,
        settings,
        exporter.clone(),
        Schedule::new(Duration::from_secs(300)),
    )
    .await
    .expect("spawn wins");
    settle().await;

    // The first cycle waits out one interval, like v2's sleep-first loop.
    tokio::time::advance(Duration::from_secs(299)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(300)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 2);

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn loop_failure_backs_off_while_success_resets() {
    let registry = registry();
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(PlaylistSyncResult {
        success: false,
        message: "target vanished".to_owned(),
        ..PlaylistSyncResult::default()
    });
    playlist_sync::spawn_on(
        &registry,
        settings,
        exporter.clone(),
        Schedule::new(Duration::from_secs(100))
            .with_backoff(Duration::from_secs(100), Duration::from_secs(10_000)),
    )
    .await
    .expect("spawn wins");
    settle().await;

    // First sync fails at 100 s; backoff doubles the next waits.
    tokio::time::advance(Duration::from_secs(100)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(100)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(200)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 2);

    // The exporter recovers; the next cycle lands on the plain interval.
    *exporter.script.lock().unwrap() = vec![FakeExporter::ok()];
    tokio::time::advance(Duration::from_secs(500)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 3);
    tokio::time::advance(Duration::from_secs(100)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 4);

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn loop_jitter_spreads_the_sync_cadence() {
    let registry = registry();
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    playlist_sync::spawn_on(
        &registry,
        settings,
        exporter.clone(),
        Schedule::new(Duration::from_secs(300)).with_jitter(Duration::from_secs(30)),
    )
    .await
    .expect("spawn wins");
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 0);

    // The first sync lands somewhere in [300, 330].
    tokio::time::advance(Duration::from_secs(300)).await;
    settle().await;
    let early = exporter.calls.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), early + 1);
    assert!(early <= 1, "at most one jittered sync fired early");

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn loop_removal_failures_keep_cycling() {
    let registry = registry();
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(PlaylistSyncResult {
        success: true,
        message: "1 file would not delete; retrying next cycle".to_owned(),
        written: 1,
        removal_failures: 1,
        ..PlaylistSyncResult::default()
    });
    playlist_sync::spawn_on(
        &registry,
        settings,
        exporter.clone(),
        Schedule::new(Duration::from_secs(100)),
    )
    .await
    .expect("spawn wins");
    settle().await;

    // Removal failures log loud but count as a kept cycle, not a failure:
    // the cadence stays flat and the files retry.
    tokio::time::advance(Duration::from_secs(100)).await;
    settle().await;
    tokio::time::advance(Duration::from_secs(100)).await;
    settle().await;
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 2);

    registry.cancel_all(Duration::from_secs(5)).await;
}
