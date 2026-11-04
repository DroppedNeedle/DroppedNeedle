//! ListenBrainz linking and listening settings: the link lifecycle over
//! HTTP, rejected credentials and refused saves storing nothing, sealed
//! tokens and partial preference saves surviving a reopen of the database,
//! and a saved visibility reaching the live feed.

use crate::common::Probe;
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
use droppedneedle_media::playback::listening::{ListeningStore, PrefsPatch};
use droppedneedle_media::playback::listening_routes::{
    ListeningDeps, MixGrants, routes, update_prefs,
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

/// The verifier's answer to a good pair.
fn connected() -> VerifyOutcome {
    VerifyOutcome {
        valid: true,
        message: "Successfully connected".to_owned(),
        rate_limited: false,
    }
}

/// The settings over a scratch database that knows `user-1`.
async fn rig() -> Rig {
    let scratch = ScratchDir::new("listening-settings");
    let runtime = crate::common::scratch_runtime(&scratch.join("app.db")).await;
    mirror_user(&runtime, "user-1").await;
    let verifier = Arc::new(Verifier(Mutex::new(connected())));
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
                 VALUES (?1, ?1, 'user', 0)",
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

/// A `PUT` of `body` as JSON.
fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// The routes over one rig: rejected credentials store nothing, a link
/// connects and disconnects without its token ever on the wire, and a
/// preferences save the database refuses answers an error, never the
/// stored values as if it had landed.
#[tokio::test]
async fn scrobble_routes_over_http() {
    let rig = rig().await;
    let router = with_test_principal(
        routes().with_state(rig.deps.clone()).into_router(),
        user_session("user-1"),
    );

    *rig.verifier.0.lock().unwrap() = VerifyOutcome {
        valid: false,
        message: "Token invalid or expired".to_owned(),
        rate_limited: false,
    };
    let response = router
        .clone()
        .oneshot(put(
            "/api/v3/me/connections/listenbrainz",
            serde_json::json!({"user_token": "bad", "username": "melody"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "rejected");
    assert!(!rig.deps.store.has_listenbrainz_link("user-1").await);

    *rig.verifier.0.lock().unwrap() = connected();
    let response = router
        .clone()
        .oneshot(put(
            "/api/v3/me/connections/listenbrainz",
            serde_json::json!({"user_token": "lb-token-9", "username": "melody"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "connect");
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
                .method("DELETE")
                .uri("/api/v3/me/connections/listenbrainz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "disconnect");
    assert!(rig.deps.store.listenbrainz_link("user-1").await.is_none());

    rig.runtime
        .db()
        .write(Lane::Foreground, "scrobble-test-refuse", |tx| {
            tx.execute_batch(
                "CREATE TRIGGER refuse_prefs BEFORE INSERT ON user_listening_prefs \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("trigger installs");
    let response = router
        .oneshot(put(
            "/api/v3/me/scrobble-preferences",
            serde_json::json!({"scrobble_to_listenbrainz": true}),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "refused preferences save"
    );
    rig.runtime.shutdown().await;
}

/// Partial preference saves and a sealed ListenBrainz link, written and
/// read back across one reopen of the database file.
#[tokio::test]
async fn scrobble_store_rows_survive_reopen() {
    let scratch = ScratchDir::new("scrobble-store");
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
        .await
        .expect("prefs save");
    let prefs = store.prefs("user-1").await;
    assert!(prefs.scrobble_to_listenbrainz);
    assert_eq!(prefs.primary_music_source, "lastfm");
    assert!(prefs.navidrome_handles_external_scrobbles);
    assert_eq!(prefs.now_playing_visibility, "full");

    assert!(!store.has_listenbrainz_link("user-1").await);
    store
        .save_listenbrainz("user-1", "melody", "lb-token-1")
        .await
        .expect("save wins");
    assert!(store.has_listenbrainz_link("user-1").await);
    assert_eq!(
        store
            .listenbrainz_link("user-1")
            .await
            .expect("link reads")
            .username,
        "melody"
    );
    assert_eq!(
        store.listenbrainz_token("user-1").await.as_deref(),
        Some("lb-token-1")
    );
    // The raw row holds ciphertext, never the token.
    let raw: String = runtime.db().one(
        "SELECT connection_data FROM user_connections WHERE user_id = 'user-1'",
        [],
    );
    assert!(!raw.contains("lb-token-1"), "{raw}");
    runtime.shutdown().await;

    // A second boot over the same file reads the merged row and the link
    // back.
    let runtime = open_runtime(&DbConfig::new(
        &db_path,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    ))
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
        .await
        .expect("prefs save");
    let prefs = store.prefs("user-1").await;
    assert_eq!(prefs.now_playing_visibility, "offline");
    assert!(prefs.scrobble_to_listenbrainz);

    assert_eq!(
        store
            .listenbrainz_link("user-1")
            .await
            .expect("link reads")
            .username,
        "melody"
    );
    assert!(store.delete_listenbrainz("user-1").await.unwrap());
    assert!(!store.has_listenbrainz_link("user-1").await);
    assert!(!store.delete_listenbrainz("user-1").await.unwrap());
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
