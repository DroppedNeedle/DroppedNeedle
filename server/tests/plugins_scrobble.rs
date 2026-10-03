//! R9 ListenBrainz and scrobble-settings backend briefs.
//!
//! Each test pins one behavior: verify-then-store connects, username rules,
//! sealed tokens that never reach the wire, prefs round-trips with their
//! defaults, enum validation, and the personal-mix hook edge. No live
//! ListenBrainz traffic: connects run against a scripted verifier, and the
//! HTTP verifier runs against a loopback stub.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse as _;
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::auth::users::memory::with_test_principal;
use droppedneedle::auth::users::roles::Role;
use droppedneedle::db::{DbConfig, DbRuntime, Lane, open_runtime};
use droppedneedle::ids::IdGenerator;
use droppedneedle::plugins::fakes::{FakeRoles, FakeVerifier};
use droppedneedle::plugins::handlers::{ScrobbleHttpDeps, scrobble_router};
use droppedneedle::plugins::scrobble::{
    HttpListenBrainzVerifier, ListenBrainzLinkStore, MemoryListenBrainzLinkStore,
    MemoryScrobblePrefsStore, NoopConnectionChangedHook, NoopMixApprovalHook, ScrobbleDeps,
    ScrobblePrefsPatch, ScrobblePrefsStore, SqliteListenBrainzLinkStore, SqliteScrobblePrefsStore,
    StaticMixState, VerifyOutcome, connect_listenbrainz, disconnect_listenbrainz,
};
use droppedneedle::runtime_config::crypto::Crypto;
use tower::ServiceExt as _;

/// Fixed id generator so error ids assert stably.
#[derive(Debug, Clone)]
struct FixedIds;

impl IdGenerator for FixedIds {
    fn new_id(&self) -> String {
        "123e4567-e89b-12d3-a456-426614174000".to_owned()
    }
}

fn test_crypto() -> Arc<Crypto> {
    Arc::new(Crypto::from_key_bytes(&[11u8; 32]).unwrap())
}

fn user_session(user_id: &str) -> CurrentSession {
    CurrentSession {
        user_id: user_id.to_owned(),
        session_id: "session-1".to_owned(),
        kind: SessionKind::Standard,
        transport: Transport::Bearer,
    }
}

struct Rig {
    deps: ScrobbleDeps,
    http: ScrobbleHttpDeps,
    verifier: Arc<FakeVerifier>,
}

fn rig() -> Rig {
    let verifier = Arc::new(FakeVerifier::valid());
    let deps = ScrobbleDeps::new(
        Arc::new(MemoryScrobblePrefsStore::new()),
        Arc::new(MemoryListenBrainzLinkStore::new(test_crypto())),
        verifier.clone(),
        Arc::new(NoopMixApprovalHook),
        Arc::new(NoopConnectionChangedHook),
    );
    let roles = Arc::new(FakeRoles::new());
    roles.insert("user-1", Role::User);
    roles.insert("admin-1", Role::Admin);
    let http = ScrobbleHttpDeps {
        deps: ScrobbleDeps::new(
            deps.prefs.clone(),
            deps.links.clone(),
            deps.verifier.clone(),
            deps.mix_hook.clone(),
            deps.cache_hook.clone(),
        ),
        roles,
        mix_state: Arc::new(StaticMixState),
        ids: Arc::new(FixedIds),
    };
    Rig {
        deps,
        http,
        verifier,
    }
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn connect_verifies_then_stores() {
    let rig = rig();
    let link = connect_listenbrainz(&rig.deps, "user-1", "melody", "lb-token-1")
        .await
        .unwrap();
    assert_eq!(link.username, "melody");
    let status = rig.deps.links.status("user-1").await.unwrap();
    assert_eq!(status.username, "melody");
    assert_eq!(
        rig.deps.links.token_for("user-1").await.as_deref(),
        Some("lb-token-1")
    );
}

#[tokio::test]
async fn connect_requires_a_username() {
    let rig = rig();
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/me/connections/listenbrainz")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"user_token": "lb-token-1", "username": "  "}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        json_body(response)
            .await
            .to_string()
            .contains("username is required")
    );
    assert!(!rig.deps.links.has_link("user-1").await);
}

#[tokio::test]
async fn rejected_credentials_store_nothing() {
    let rig = rig();
    *rig.verifier.fallback.lock().unwrap() = VerifyOutcome {
        valid: false,
        message: "Token invalid or expired".to_owned(),
        rate_limited: false,
    };
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/me/connections/listenbrainz")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"user_token": "bad", "username": "melody"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!rig.deps.links.has_link("user-1").await);
}

#[tokio::test]
async fn rate_limited_verify_answers_429() {
    let rig = rig();
    *rig.verifier.fallback.lock().unwrap() = VerifyOutcome {
        valid: false,
        message: "rate-limiting".to_owned(),
        rate_limited: true,
    };
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/me/connections/listenbrainz")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"user_token": "t", "username": "melody"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().contains_key("retry-after"));
    assert!(!rig.deps.links.has_link("user-1").await);
}

#[tokio::test]
async fn link_lifecycle_over_http() {
    let rig = rig();
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/me/connections/listenbrainz")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"user_token": "lb-token-9", "username": "melody"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status = json_body(response).await;
    assert_eq!(status["service"], "listenbrainz");
    assert_eq!(status["enabled"], true);
    assert_eq!(status["username"], "melody");
    // The token never appears on the wire.
    assert!(!status.to_string().contains("lb-token-9"));

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!json_body(response).await.to_string().contains("lb-token-9"));

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_users_read_as_unlinked() {
    let store = MemoryListenBrainzLinkStore::new(test_crypto());
    store.save("user-1", "melody", "lb-token-1").await.unwrap();
    assert!(store.status("user-1").await.is_some());
    assert_eq!(store.status("ghost").await, None);
    assert_eq!(store.token_for("ghost").await, None);
    assert!(!store.has_link("ghost").await);
    assert!(!store.delete("ghost").await);
}

#[tokio::test]
async fn prefs_round_trip_from_defaults() {
    let rig = rig();
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/me/scrobble-preferences")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let prefs = json_body(response).await;
    assert_eq!(prefs["scrobble_to_lastfm"], false);
    assert_eq!(prefs["scrobble_to_listenbrainz"], false);
    assert_eq!(prefs["navidrome_handles_external_scrobbles"], true);
    assert_eq!(prefs["primary_music_source"], "listenbrainz");
    assert_eq!(prefs["now_playing_visibility"], "full");
    assert_eq!(prefs["auto_request_personal_mix"], false);
    assert_eq!(prefs["auto_request_state"], "none");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/me/scrobble-preferences")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "scrobble_to_listenbrainz": true,
                        "primary_music_source": "lastfm",
                        "now_playing_visibility": "offline",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let prefs = json_body(response).await;
    assert_eq!(prefs["scrobble_to_listenbrainz"], true);
    assert_eq!(prefs["scrobble_to_lastfm"], false);
    assert_eq!(prefs["primary_music_source"], "lastfm");
    assert_eq!(prefs["now_playing_visibility"], "offline");
    // Untouched fields keep their stored values.
    assert_eq!(prefs["navidrome_handles_external_scrobbles"], true);
}

#[tokio::test]
async fn prefs_reject_unknown_enum_values() {
    let rig = rig();
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    for body in [
        serde_json::json!({"primary_music_source": "deezer"}),
        serde_json::json!({"now_playing_visibility": "invisible"}),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/me/scrobble-preferences")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let stored = rig.deps.prefs.get("user-1").await;
    assert_eq!(stored.primary_music_source, "listenbrainz");
    assert_eq!(stored.now_playing_visibility, "full");
}

/// Counting personal-mix hook for the toggle edge.
struct CountingHook {
    calls: Mutex<Vec<(String, String, bool)>>,
}

impl droppedneedle::plugins::scrobble::MixApprovalHook for CountingHook {
    fn on_auto_request_toggled(&self, user_id: &str, role: &str, enabled: bool) {
        if let Ok(mut guard) = self.calls.lock() {
            guard.push((user_id.to_owned(), role.to_owned(), enabled));
        }
    }
}

#[tokio::test]
async fn mix_hook_fires_only_on_real_toggle_changes() {
    let hook = Arc::new(CountingHook {
        calls: Mutex::new(Vec::new()),
    });
    let deps = ScrobbleDeps::new(
        Arc::new(MemoryScrobblePrefsStore::new()),
        Arc::new(MemoryListenBrainzLinkStore::new(test_crypto())),
        Arc::new(FakeVerifier::valid()),
        hook.clone(),
        Arc::new(NoopConnectionChangedHook),
    );
    let on = droppedneedle::plugins::scrobble::ScrobblePrefsPatch {
        auto_request_personal_mix: Some(true),
        ..droppedneedle::plugins::scrobble::ScrobblePrefsPatch::default()
    };
    droppedneedle::plugins::scrobble::update_prefs(&deps, "user-1", "user", &on)
        .await
        .unwrap();
    // Resending the unchanged value must not re-queue the grant.
    droppedneedle::plugins::scrobble::update_prefs(&deps, "user-1", "user", &on)
        .await
        .unwrap();
    let off = droppedneedle::plugins::scrobble::ScrobblePrefsPatch {
        auto_request_personal_mix: Some(false),
        ..droppedneedle::plugins::scrobble::ScrobblePrefsPatch::default()
    };
    droppedneedle::plugins::scrobble::update_prefs(&deps, "user-1", "user", &off)
        .await
        .unwrap();
    assert_eq!(hook.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn disconnect_without_a_link_is_not_found() {
    let rig = rig();
    assert!(!disconnect_listenbrainz(&rig.deps, "user-1").await);
    let router = with_test_principal(scrobble_router(rig.http), user_session("user-1"));
    let response = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scrobble_routes_need_a_session() {
    let rig = rig();
    let response = scrobble_router(rig.http)
        .oneshot(
            Request::builder()
                .uri("/me/scrobble-preferences")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Loopback ListenBrainz stub: scripted statuses per endpoint.
async fn stub_server() -> (String, tokio::task::JoinHandle<()>) {
    use axum::routing::get;
    let app = axum::Router::new()
        .route(
            "/1/validate-token",
            get(|headers: axum::http::HeaderMap| async move {
                let token = headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("");
                match token {
                    "Token good-token" => (
                        StatusCode::OK,
                        JsonStub(serde_json::json!({"valid": true, "user_name": "melody"})),
                    )
                        .into_response(),
                    "Token limited" => StatusCode::TOO_MANY_REQUESTS.into_response(),
                    _ => StatusCode::UNAUTHORIZED.into_response(),
                }
            }),
        )
        .route(
            "/1/user/{name}/listen-count",
            get(
                |axum::extract::Path(name): axum::extract::Path<String>| async move {
                    if name == "melody" {
                        (
                            StatusCode::OK,
                            JsonStub(serde_json::json!({"payload": {"count": 1234}})),
                        )
                            .into_response()
                    } else {
                        StatusCode::NOT_FOUND.into_response()
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, handle)
}

struct JsonStub(serde_json::Value);

impl axum::response::IntoResponse for JsonStub {
    fn into_response(self) -> axum::response::Response {
        axum::Json(self.0).into_response()
    }
}

#[tokio::test]
async fn http_verifier_maps_stub_statuses() {
    use droppedneedle::plugins::scrobble::ListenBrainzVerifier;
    let (base, server) = stub_server().await;
    let verifier = HttpListenBrainzVerifier::new(reqwest::Client::new(), &base);

    let ok = verifier.verify("melody", "good-token").await;
    assert!(ok.valid);
    assert!(ok.message.contains("melody"));

    let bad = verifier.verify("melody", "bad-token").await;
    assert!(!bad.valid);
    assert_eq!(bad.message, "Token invalid or expired");

    let limited = verifier.verify("melody", "limited").await;
    assert!(limited.rate_limited);

    let named = verifier.verify("melody", "").await;
    assert!(named.valid);
    assert!(named.message.contains("1,234"));

    let ghost = verifier.verify("ghost", "").await;
    assert!(!ghost.valid);
    assert!(ghost.message.contains("ghost"));

    server.abort();
}

// ---------------------------------------------------------------------------
// SQLite stores
// ---------------------------------------------------------------------------

/// Scratch-dir sequence so parallel tests never share a database.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_db(tag: &str) -> std::path::PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "droppedneedle-scrobble-{tag}-{seq}-{}",
        std::process::id()
    ))
}

/// Mirror a user row so FK-backed prefs and link writes accept it.
async fn mirror_user(runtime: &DbRuntime, user_id: &str) {
    let user_id = user_id.to_owned();
    runtime
        .lane()
        .write(Lane::Foreground, "scrobble-test-seed", move |tx| {
            tx.execute(
                "INSERT INTO auth_users (id, display_name, role, created_at)
                 VALUES (?1, ?1, 'user', '2026-01-01T00:00:00Z')",
                rusqlite::params![user_id],
            )?;
            Ok(())
        })
        .await
        .expect("user row seeds");
}

#[tokio::test]
async fn sqlite_prefs_partial_upsert_survives_reopen() {
    let db_path = scratch_db("prefs").join("app.db");
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime opens");
    mirror_user(&runtime, "user-1").await;
    let store = SqliteScrobblePrefsStore::new(runtime.pool().clone(), runtime.lane().clone());

    // Unknown users read the defaults, like the memory store.
    let prefs = store.get("user-1").await;
    assert!(!prefs.scrobble_to_listenbrainz);
    assert!(prefs.navidrome_handles_external_scrobbles);
    assert_eq!(prefs.primary_music_source, "listenbrainz");

    // A partial patch lands; untouched fields keep their values.
    store
        .upsert(
            "user-1",
            &ScrobblePrefsPatch {
                scrobble_to_listenbrainz: Some(true),
                primary_music_source: Some("lastfm".to_owned()),
                ..ScrobblePrefsPatch::default()
            },
        )
        .await;
    let prefs = store.get("user-1").await;
    assert!(prefs.scrobble_to_listenbrainz);
    assert_eq!(prefs.primary_music_source, "lastfm");
    assert!(prefs.navidrome_handles_external_scrobbles);
    assert_eq!(prefs.now_playing_visibility, "full");
    runtime.shutdown().await;

    // A second boot over the same file reads the merged row back.
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime reopens");
    let store = SqliteScrobblePrefsStore::new(runtime.pool().clone(), runtime.lane().clone());
    let prefs = store.get("user-1").await;
    assert!(prefs.scrobble_to_listenbrainz);
    assert_eq!(prefs.primary_music_source, "lastfm");

    // And a later patch merges over the persisted row, not the defaults.
    store
        .upsert(
            "user-1",
            &ScrobblePrefsPatch {
                now_playing_visibility: Some("offline".to_owned()),
                ..ScrobblePrefsPatch::default()
            },
        )
        .await;
    let prefs = store.get("user-1").await;
    assert_eq!(prefs.now_playing_visibility, "offline");
    assert!(prefs.scrobble_to_listenbrainz);
    runtime.shutdown().await;
}

#[tokio::test]
async fn sqlite_links_round_trip_sealed_and_survive_reopen() {
    let db_path = scratch_db("links").join("app.db");
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime opens");
    mirror_user(&runtime, "user-1").await;
    let store = SqliteListenBrainzLinkStore::new(
        runtime.pool().clone(),
        runtime.lane().clone(),
        test_crypto(),
    );

    assert!(!store.has_link("user-1").await);
    store
        .save("user-1", "melody", "lb-token-1")
        .await
        .expect("save wins");
    assert!(store.has_link("user-1").await);
    assert_eq!(
        store.status("user-1").await.expect("link reads").username,
        "melody"
    );
    assert_eq!(
        store.token_for("user-1").await.as_deref(),
        Some("lb-token-1")
    );
    // The raw row holds ciphertext, never the token.
    let raw: String =
        sqlx::query_scalar("SELECT connection_data FROM user_connections WHERE user_id = 'user-1'")
            .fetch_one(runtime.pool())
            .await
            .expect("raw row reads");
    assert!(!raw.contains("lb-token-1"), "{raw}");
    runtime.shutdown().await;

    // A second boot over the same file reads the link back.
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime reopens");
    let store = SqliteListenBrainzLinkStore::new(
        runtime.pool().clone(),
        runtime.lane().clone(),
        test_crypto(),
    );
    assert_eq!(
        store.status("user-1").await.expect("link reads").username,
        "melody"
    );
    assert!(store.delete("user-1").await);
    assert!(!store.has_link("user-1").await);
    assert!(!store.delete("user-1").await);
    runtime.shutdown().await;
}
