//! ListenBrainz linking and listening settings: connect verifies before it
//! stores, rejected credentials store nothing, sealed tokens and partial
//! preference saves survive a reopen of the database, the link lifecycle
//! over HTTP, a saved visibility reaching the live feed, and the
//! ListenBrainz client as the verifier against a scripted wire.

use crate::common::ScratchDir;
use crate::common::auth::{test_principal, with_test_principal};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_auth::Principal;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_kernel::db::{DbConfig, DbRuntime, Lane, open_runtime};
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_media::playback::listening::{ListenBrainzLinkStore, ListeningStore, PrefsPatch};
use droppedneedle_media::playback::listening_routes::{
    ListeningDeps, MixGrants, connect_listenbrainz, routes, update_prefs,
};
use droppedneedle_media::playback::presence::{PresenceRegistry, PresenceUpdate};
use droppedneedle_providers::listenbrainz::{ListenBrainzVerifier, VerifyOutcome};
use tower::ServiceExt as _;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn test_crypto() -> Arc<Crypto> {
    Arc::new(Crypto::from_key_bytes(&[11u8; 32]).unwrap())
}

fn store_over(runtime: &DbRuntime) -> ListeningStore {
    ListeningStore::new(
        runtime.db().clone(),
        test_crypto(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    )
}

fn user_session(user_id: &str) -> Principal {
    test_principal(user_id, Role::User, "session-1", SessionKind::Standard)
}

/// A ListenBrainz verifier that gives one answer to every pair.
struct Verifier(Mutex<VerifyOutcome>);

impl ListenBrainzVerifier for Verifier {
    fn verify<'a>(&'a self, _username: &'a str, _token: &'a str) -> BoxFuture<'a, VerifyOutcome> {
        let outcome = self.0.lock().unwrap().clone();
        Box::pin(async move { outcome })
    }
}

/// The grant answered by role alone: no requests side behind it.
struct RoleOnlyGrants;

impl MixGrants for RoleOnlyGrants {
    fn on_toggle<'a>(
        &'a self,
        _user_id: &'a str,
        _role: Role,
        _enabled: bool,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn state<'a>(
        &'a self,
        _user_id: &'a str,
        role: Role,
        toggle_on: bool,
    ) -> BoxFuture<'a, String> {
        let state = match (toggle_on, role) {
            (false, _) => "none",
            (true, Role::Admin) => "approved",
            (true, _) => "pending",
        };
        Box::pin(async move { state.to_owned() })
    }
}

struct Rig {
    deps: ListeningDeps,
    verifier: Arc<Verifier>,
    runtime: DbRuntime,
    _scratch: ScratchDir,
}

/// The settings over a scratch database that knows `user-1`.
async fn rig() -> Rig {
    let scratch = ScratchDir::new("listening-settings");
    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    mirror_user(&runtime, "user-1").await;
    let verifier = Arc::new(Verifier(Mutex::new(VerifyOutcome {
        valid: true,
        message: "Successfully connected".to_owned(),
        rate_limited: false,
    })));
    let deps = ListeningDeps {
        store: store_over(&runtime),
        verifier: verifier.clone(),
        mix: Arc::new(RoleOnlyGrants),
        presence: PresenceRegistry::new(EventHub::new()),
    };
    Rig {
        deps,
        verifier,
        runtime,
        _scratch: scratch,
    }
}

/// Mirror a user row so FK-backed prefs and link writes accept it.
async fn mirror_user(runtime: &DbRuntime, user_id: &str) {
    let user_id = user_id.to_owned();
    runtime
        .db()
        .write(Lane::Foreground, "scrobble-test-seed", move |tx| {
            tx.execute(
                "INSERT INTO auth_users (id, display_name, role, created_at)
                 VALUES (?1, ?1, 'user', '2026-01-01T00:00:00Z')",
                rusqlite::params![user_id],
            )?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("user row seeds");
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn connect_verifies_then_stores() {
    let rig = rig().await;
    let link = connect_listenbrainz(&rig.deps, "user-1", "melody", "lb-token-1")
        .await
        .unwrap();
    assert_eq!(link.username, "melody");
    let status = rig.deps.store.status("user-1").await.unwrap();
    assert_eq!(status.username, "melody");
    assert_eq!(
        rig.deps.store.token_for("user-1").await.as_deref(),
        Some("lb-token-1")
    );
    rig.runtime.shutdown().await;
}

#[tokio::test]
async fn rejected_credentials_store_nothing() {
    let rig = rig().await;
    *rig.verifier.0.lock().unwrap() = VerifyOutcome {
        valid: false,
        message: "Token invalid or expired".to_owned(),
        rate_limited: false,
    };
    let router = with_test_principal(
        routes().with_state(rig.deps.clone()).into_router(),
        user_session("user-1"),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v3/me/connections/listenbrainz")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"user_token": "bad", "username": "melody"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!rig.deps.store.has_link("user-1").await);
    rig.runtime.shutdown().await;
}

#[tokio::test]
async fn link_lifecycle_over_http() {
    let rig = rig().await;
    let router = with_test_principal(
        routes().with_state(rig.deps.clone()).into_router(),
        user_session("user-1"),
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/v3/me/connections/listenbrainz")
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
                .uri("/api/v3/me/connections/listenbrainz")
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
                .uri("/api/v3/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v3/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    rig.runtime.shutdown().await;
}

#[tokio::test]
async fn scrobble_routes_need_a_session() {
    let rig = rig().await;
    let response = routes()
        .with_state(rig.deps.clone())
        .into_router()
        .oneshot(
            Request::builder()
                .uri("/api/v3/me/scrobble-preferences")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    rig.runtime.shutdown().await;
}

/// The link verifier is the one ListenBrainz client: a good token
/// connects as the account ListenBrainz names, a refused one is invalid, a
/// known username counts its listens, an unknown one is "not found", and a
/// rate limit asks the caller to back off.
#[tokio::test(start_paused = true)]
async fn the_listenbrainz_client_verifies_links() {
    use droppedneedle_providers::Providers;
    use droppedneedle_providers::executor::ScriptedWire;
    use droppedneedle_providers::listenbrainz::DEFAULT_BASE_URL;

    let wire = Arc::new(ScriptedWire::new());
    let token = format!("{DEFAULT_BASE_URL}/1/validate-token");
    wire.answer(
        &token,
        200,
        &[],
        r#"{"valid": true, "user_name": "melody"}"#,
    );
    wire.answer(&token, 401, &[], "");
    wire.answer(&token, 429, &[], "");
    wire.answer(
        &format!("{DEFAULT_BASE_URL}/1/user/melody/listen-count"),
        200,
        &[],
        r#"{"payload": {"count": 1234}}"#,
    );
    wire.answer(
        &format!("{DEFAULT_BASE_URL}/1/user/ghost/listen-count"),
        404,
        &[],
        "",
    );
    let providers = Providers::unpaced_at(
        wire.clone(),
        droppedneedle_providers::ProviderEndpoints {
            listenbrainz: DEFAULT_BASE_URL.to_owned(),
            ..droppedneedle_providers::ProviderEndpoints::closed()
        },
    );
    // Through the trait, as account linking holds it.
    let verifier: &dyn ListenBrainzVerifier = &providers.listenbrainz;

    let ok = verifier.verify("melody", "good-token").await;
    assert!(ok.valid);
    assert!(ok.message.contains("melody"));
    let sent = wire.sent();
    let authorization = sent[0]
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str());
    assert_eq!(authorization, Some("Token good-token"));

    let bad = verifier.verify("melody", "bad-token").await;
    assert!(!bad.valid);
    assert_eq!(bad.message, "Token invalid or expired");

    let named = verifier.verify("melody", "").await;
    assert!(named.valid);
    assert!(named.message.contains("1,234"));

    let ghost = verifier.verify("ghost", "").await;
    assert!(!ghost.valid);
    assert!(ghost.message.contains("ghost"));

    let limited = verifier.verify("melody", "limited").await;
    assert!(limited.rate_limited);

    let unsafe_token = verifier.verify("melody", "two words").await;
    assert!(!unsafe_token.valid);
    assert_eq!(
        wire.sent().len(),
        sent.len() + 5,
        "an unsafe token never reaches the wire"
    );
}

#[tokio::test]
async fn sqlite_prefs_partial_upsert_survives_reopen() {
    let scratch = ScratchDir::new("scrobble-prefs");
    let db_path = scratch.join("app.db");
    let runtime = crate::common::scratch_runtime(&db_path).await;
    mirror_user(&runtime, "user-1").await;
    let store = store_over(&runtime);

    // A user who saved nothing reads the defaults.
    let prefs = store.prefs("user-1").await;
    assert!(!prefs.scrobble_to_listenbrainz);
    assert!(prefs.navidrome_handles_external_scrobbles);
    assert_eq!(prefs.primary_music_source, "listenbrainz");

    // A partial patch lands; untouched fields keep their values.
    store
        .update_prefs(
            "user-1",
            &PrefsPatch {
                scrobble_to_listenbrainz: Some(true),
                primary_music_source: Some("lastfm".to_owned()),
                ..PrefsPatch::default()
            },
        )
        .await;
    let prefs = store.prefs("user-1").await;
    assert!(prefs.scrobble_to_listenbrainz);
    assert_eq!(prefs.primary_music_source, "lastfm");
    assert!(prefs.navidrome_handles_external_scrobbles);
    assert_eq!(prefs.now_playing_visibility, "full");
    runtime.shutdown().await;

    // A second boot over the same file reads the merged row back.
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime reopens");
    let store = store_over(&runtime);
    let prefs = store.prefs("user-1").await;
    assert!(prefs.scrobble_to_listenbrainz);
    assert_eq!(prefs.primary_music_source, "lastfm");

    // And a later patch merges over the persisted row, not the defaults.
    store
        .update_prefs(
            "user-1",
            &PrefsPatch {
                now_playing_visibility: Some("offline".to_owned()),
                ..PrefsPatch::default()
            },
        )
        .await;
    let prefs = store.prefs("user-1").await;
    assert_eq!(prefs.now_playing_visibility, "offline");
    assert!(prefs.scrobble_to_listenbrainz);
    runtime.shutdown().await;
}

#[tokio::test]
async fn sqlite_links_round_trip_sealed_and_survive_reopen() {
    let scratch = ScratchDir::new("scrobble-links");
    let db_path = scratch.join("app.db");
    let runtime = crate::common::scratch_runtime(&db_path).await;
    mirror_user(&runtime, "user-1").await;
    let store = store_over(&runtime);

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
            .fetch_one(runtime.db().sqlx())
            .await
            .expect("raw row reads");
    assert!(!raw.contains("lb-token-1"), "{raw}");
    runtime.shutdown().await;

    // A second boot over the same file reads the link back.
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime reopens");
    let store = store_over(&runtime);
    assert_eq!(
        store.status("user-1").await.expect("link reads").username,
        "melody"
    );
    assert!(store.delete("user-1").await);
    assert!(!store.has_link("user-1").await);
    assert!(!store.delete("user-1").await);
    runtime.shutdown().await;
}

/// Privacy: a user who sets their now-playing visibility to offline drops
/// out of the live feed at once, not after a restart.
#[tokio::test]
async fn saved_visibility_reaches_the_live_presence_feed() {
    let rig = rig().await;
    let registry = rig.deps.presence.clone();
    registry.update(
        PresenceUpdate {
            key: "user-1:web".to_owned(),
            user_id: Some("user-1".to_owned()),
            user_name: "Melody".to_owned(),
            source: "local".to_owned(),
            device_name: "Web".to_owned(),
            track_name: "Roads".to_owned(),
            artist_name: "Portishead".to_owned(),
            album_name: None,
            cover_url: String::new(),
            is_paused: false,
            progress_ms: None,
            duration_ms: None,
            track_file_id: None,
        },
        1_790_000_000,
        |_| Ok("full".to_owned()),
    );
    assert_eq!(registry.snapshot().len(), 1);
    update_prefs(
        &rig.deps,
        "user-1",
        Role::User,
        &PrefsPatch {
            now_playing_visibility: Some("offline".to_owned()),
            ..PrefsPatch::default()
        },
    )
    .await
    .unwrap();
    assert!(registry.snapshot().is_empty(), "offline hides the session");
    rig.runtime.shutdown().await;
}
