//! Navidrome playlist sync: the trigger route is admin-only and reads
//! settings fresh on every call.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::playlist_sync::{
    PlaylistExporter, PlaylistSyncConfig, PlaylistSyncResult, PlaylistSyncSettings,
};
use droppedneedle_media::remotes::playlist_sync::{self as playlist_route, PlaylistSyncState};
use tower::ServiceExt as _;

use crate::common::auth::{test_principal, with_test_principal};

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

fn route_state(
    settings: FakeSettings,
    exporter: FakeExporter,
) -> PlaylistSyncState<FakeSettings, FakeExporter> {
    PlaylistSyncState { settings, exporter }
}

/// The route signed in as `role`, standing in for the session gate, or
/// with no session at all.
fn app_as(role: Option<Role>, settings: FakeSettings, exporter: FakeExporter) -> axum::Router {
    let app = playlist_route::router(route_state(settings, exporter));
    match role {
        Some(role) => with_test_principal(
            app,
            test_principal("user-1", role, "sess-1", SessionKind::Standard),
        ),
        None => app,
    }
}

fn post_sync() -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/navidrome/playlist-sync")
        .body(Body::empty())
        .expect("request builds")
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
async fn route_reads_settings_fresh_each_call() {
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    let app = app_as(Some(Role::Admin), settings.clone(), exporter.clone());

    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(read_json(response).await["success"], true);

    // Flip sync off between calls: the next call sees it at once.
    *settings.config.lock().unwrap() = None;
    let app = app_as(Some(Role::Admin), settings, exporter.clone());
    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(read_json(response).await["success"], false);
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn route_refuses_everyone_but_admins() {
    let settings = FakeSettings::enabled();
    let exporter = FakeExporter::answering(FakeExporter::ok());
    let app = app_as(None, settings, exporter.clone());

    // No session at all: 401 with the Bearer challenge.
    let response = app.oneshot(post_sync()).await.expect("router answers");
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
    let app = app_as(Some(Role::Trusted), settings, exporter.clone());
    let response = app.oneshot(post_sync()).await.expect("router answers");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "FORBIDDEN");
    assert_eq!(body["error"]["message"], "Admin role required");
    assert_eq!(exporter.calls.load(Ordering::SeqCst), 0);
}
