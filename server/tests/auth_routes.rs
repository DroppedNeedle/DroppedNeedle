//! Auth-routes briefs: native login/logout/setup plus the federated flows.
//!
//! Thin-HTTP proofs over the slice services with fakes only (no live IdP,
//! no network): transport shapes, logout revocation, once-only setup, the
//! OIDC/Jellyfin/Plex journeys, and allowlist reachability through the real
//! session middleware.

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    middleware,
    routing::get,
};
use droppedneedle::auth::federated::fakes::{
    FakeJellyfinIdp, FakeJellyfinLink, FakeOidcExchanges, FakeOidcIdp, FakeOidcStates,
    FakePlexLink, FakePlexPinClient, FakeSessionIssuer, FakeUserStore,
};
use droppedneedle::auth::federated::jellyfin_login::JellyfinProfile;
use droppedneedle::auth::federated::oidc::{
    DiscoveryDoc, OidcConfig, OidcLogin, OidcTokens, RawClaims,
};
use droppedneedle::auth::federated::plex::PlexAccount;
use droppedneedle::auth::routes::federated::{
    JellyfinRouteState, OidcRouteState, PlexRouteState, StaticOidcConfig, jellyfin_router,
    oidc_router, plex_router,
};
use droppedneedle::auth::routes::native::{NativeAuthState, native_auth_router};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::login::{FakePasswordVerifier, FakeUserTable};
use droppedneedle::auth::session::middleware::{CurrentSession, SessionAuth, require_session};
use droppedneedle::auth::session::store::{
    MemorySessionStore, SessionKind, SessionRecord, SessionStore, SessionStoreError, now_unix,
};
use droppedneedle::auth::session::{cookies::COOKIE_NAME, tokens};
use droppedneedle::auth::users::UsersDeps;
use droppedneedle::auth::users::memory::{MemorySessionManager, TestRig};
use droppedneedle::auth::users::models::{ManagedSession, SessionOwner, UserRecord};
use droppedneedle::auth::users::roles::Role;
use droppedneedle::auth::users::stores::{BoxFuture, SessionManager, StoreError, UserStore};
use serde_json::{Value, json};
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One logical `auth_tokens` table behind both committed traits, mirroring
/// the production wiring (the SQLite adapter implements `SessionStore` and
/// `SessionManager` over the same rows). Inserts land in both halves; the
/// management half is the revoke target.
#[derive(Clone)]
struct TwinSessions {
    mem: MemorySessionStore,
    mgr: Arc<MemorySessionManager>,
}

impl SessionStore for TwinSessions {
    async fn insert(&self, record: SessionRecord) -> Result<(), SessionStoreError> {
        self.mgr
            .seed_standard(
                &record.id,
                &record.user_id,
                &record.token_hash,
                record.user_agent.as_deref().unwrap_or(""),
                record.issued_at,
                record.expires_at,
            )
            .await;
        self.mem.insert(record).await
    }

    async fn lookup_valid(
        &self,
        token_hash: &str,
        now_unix: i64,
    ) -> Result<Option<SessionRecord>, SessionStoreError> {
        self.mem.lookup_valid(token_hash, now_unix).await
    }
}

impl SessionManager for TwinSessions {
    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ManagedSession>, StoreError>> {
        self.mgr.list_for_user(user_id)
    }

    fn revoke_scoped<'a>(
        &'a self,
        user_id: &'a str,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        self.mgr.revoke_scoped(user_id, session_id)
    }

    fn revoke_all_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        self.mgr.revoke_all_for_user(user_id)
    }

    fn replace_companion<'a>(
        &'a self,
        id: &'a str,
        user_id: &'a str,
        token_hash: &'a str,
        label: &'a str,
        issued_at: i64,
        expires_at: i64,
    ) -> BoxFuture<'a, Result<ManagedSession, StoreError>> {
        self.mgr
            .replace_companion(id, user_id, token_hash, label, issued_at, expires_at)
    }

    fn owner_by_hash<'a>(
        &'a self,
        token_hash: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionOwner>, StoreError>> {
        self.mgr.owner_by_hash(token_hash)
    }
}

struct NativeRig {
    router: Router,
    sessions: TwinSessions,
    verifier: FakePasswordVerifier,
    rig: TestRig,
}

fn native_rig() -> NativeRig {
    let rig = TestRig::new().expect("test crypto key");
    let sessions = TwinSessions {
        mem: MemorySessionStore::new(),
        mgr: rig.sessions.clone(),
    };
    let verifier = FakePasswordVerifier::new();
    let users = FakeUserTable::with_user("ada", "correct-horse-99", "user-1", "Ada");
    let deps = UsersDeps {
        sessions: Arc::new(sessions.clone()),
        ..rig.deps.clone()
    };
    let state = NativeAuthState::new(sessions.clone(), verifier.clone(), users, deps, "");
    let router = Router::new().nest("/api/v3", native_auth_router(state));
    NativeRig {
        router,
        sessions,
        verifier,
        rig,
    }
}

/// Seed the users-store row the login response enriches from.
async fn seed_login_user(rig: &TestRig) {
    rig.users
        .insert(UserRecord {
            id: "user-1".to_owned(),
            username: Some("ada".to_owned()),
            username_display: Some("Ada".to_owned()),
            display_name: "Ada".to_owned(),
            email: None,
            avatar_url: None,
            role: Role::User,
            created_at: 1_700_000_000,
            last_login_at: None,
        })
        .await
        .expect("seed user");
}

async fn post_json(app: Router, uri: &str, value: Value) -> (StatusCode, HeaderMap, Value) {
    post_json_headers(app, uri, value, &[]).await
}

async fn post_json_headers(
    app: Router,
    uri: &str,
    value: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method("POST").uri(uri);
    for (name, val) in headers {
        builder = builder.header(*name, *val);
    }
    let request = builder
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, response_headers, body)
}

/// POST JSON with a stashed session, for the session-gated Plex polls.
/// The extractor reads what the middleware would have stashed.
async fn post_json_authed(app: Router, uri: &str, value: Value) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .expect("request builds");
    let (mut parts, body) = request.into_parts();
    parts.extensions.insert(CurrentSession {
        user_id: "user-1".to_owned(),
        session_id: "sess-1".to_owned(),
        kind: SessionKind::Standard,
        transport: Transport::Bearer,
    });
    let response = app
        .oneshot(Request::from_parts(parts, body))
        .await
        .expect("router responds");
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, response_headers, body)
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, headers, body)
}

/// Raw token carried by the response `Set-Cookie` (`Some("")` when cleared).
fn set_cookie_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::SET_COOKIE)?.to_str().ok()?;
    let prefix = format!("{COOKIE_NAME}=");
    let rest = value.split(';').next()?.trim();
    rest.strip_prefix(&prefix).map(str::to_owned)
}

fn no_store(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        == Some("no-store")
}

// ---------------------------------------------------------------------------
// Native login / logout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_cookie_sets_cookie_without_token_in_body() {
    let rig = native_rig();
    seed_login_user(&rig.rig).await;
    let (status, headers, body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/login",
        json!({"username": "ada", "password": "correct-horse-99"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(no_store(&headers));
    let set_cookie = headers
        .get(axum::http::header::SET_COOKIE)
        .expect("cookie set")
        .to_str()
        .expect("ascii cookie");
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
    assert!(set_cookie.contains("Path=/api/v3"));
    assert!(set_cookie.contains("Max-Age=2592000"));
    assert_eq!(body["user"]["id"], json!("user-1"));
    assert_eq!(body["user"]["role"], json!("user"));
    assert!(body.get("token").is_none(), "cookie body leaks no token");
    let token = set_cookie_token(&headers).expect("cookie token");
    let found = rig
        .sessions
        .lookup_valid(&tokens::hash_token(&token), now_unix())
        .await
        .expect("store reads");
    assert_eq!(found.map(|r| r.user_id).as_deref(), Some("user-1"));
}

#[tokio::test]
async fn login_bearer_returns_token_without_cookie() {
    let rig = native_rig();
    seed_login_user(&rig.rig).await;
    let (status, headers, body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/login",
        json!({"username": "ADA", "password": "correct-horse-99", "transport": "bearer"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(no_store(&headers));
    assert!(
        headers.get(axum::http::header::SET_COOKIE).is_none(),
        "Bearer mode sets no cookie"
    );
    let token = body["token"].as_str().expect("token in body");
    assert_eq!(body["user"]["id"], json!("user-1"));
    let found = rig
        .sessions
        .lookup_valid(&tokens::hash_token(token), now_unix())
        .await
        .expect("store reads");
    assert_eq!(found.map(|r| r.user_id).as_deref(), Some("user-1"));
}

#[tokio::test]
async fn login_failures_are_uniform_with_dummy_verify() {
    let rig = native_rig();
    seed_login_user(&rig.rig).await;
    let (unknown_status, unknown_headers, unknown_body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/login",
        json!({"username": "nobody", "password": "whatever-99"}),
    )
    .await;
    let (wrong_status, wrong_headers, wrong_body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/login",
        json!({"username": "ada", "password": "wrong-password-99"}),
    )
    .await;
    assert_eq!(unknown_status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_body, wrong_body);
    assert_eq!(unknown_body["error"]["code"], json!("UNAUTHORIZED"));
    assert_eq!(
        unknown_body["error"]["message"],
        json!("Invalid username or password")
    );
    for headers in [&unknown_headers, &wrong_headers] {
        assert_eq!(
            headers.get(axum::http::header::WWW_AUTHENTICATE),
            Some(&"Bearer".parse().expect("challenge parses")),
        );
    }
    assert_eq!(rig.verifier.dummy_calls(), 1);
    assert_eq!(rig.verifier.verify_calls(), 1);
}

#[tokio::test]
async fn malformed_login_body_stays_in_the_envelope() {
    let rig = native_rig();
    let (status, _, body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/login",
        json!({"username": "ada"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("INVALID_INPUT"));
}

#[tokio::test]
async fn logout_clears_cookie_and_revokes() {
    let rig = native_rig();
    seed_login_user(&rig.rig).await;
    let (_, login_headers, _) = post_json(
        rig.router.clone(),
        "/api/v3/auth/login",
        json!({"username": "ada", "password": "correct-horse-99"}),
    )
    .await;
    let token = set_cookie_token(&login_headers).expect("login cookie");
    let hash = tokens::hash_token(&token);
    assert!(
        rig.sessions
            .mgr
            .owner_by_hash(&hash)
            .await
            .expect("lookup reads")
            .is_some(),
        "session resolves before logout"
    );
    let cookie = format!("{COOKIE_NAME}={token}");
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/logout")
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("request builds");
    let response = rig.router.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cleared = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("clear cookie")
        .to_str()
        .expect("ascii")
        .to_owned();
    assert!(cleared.contains("Max-Age=0"));
    assert!(
        rig.sessions
            .mgr
            .owner_by_hash(&hash)
            .await
            .expect("lookup reads")
            .is_none(),
        "token revoked by logout"
    );
    // Idempotent: logging out twice still clears with a 204.
    let again = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/logout")
        .header("cookie", format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .expect("request builds");
    let response = rig.router.clone().oneshot(again).await.expect("responds");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn setup_creates_first_admin_once_then_refuses() {
    let rig = native_rig();
    rig.rig.clock.set(now_unix());
    let (status, _, body) = get_json(rig.router.clone(), "/api/v3/auth/setup/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": true}));

    let (status, headers, body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/setup",
        json!({"username": "root", "password": "first-admin-password-1", "display_name": "Root"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(no_store(&headers));
    assert_eq!(body["user"]["role"], json!("admin"));
    assert_eq!(body["user"]["username"], json!("root"));
    assert!(body.get("token").is_none(), "setup defaults to cookie mode");
    let token = set_cookie_token(&headers).expect("setup session cookie");
    let found = rig
        .sessions
        .lookup_valid(&tokens::hash_token(&token), now_unix())
        .await
        .expect("store reads");
    assert_eq!(found.map(|r| r.user_id).as_deref(), Some("test-id-0001"));

    let (status, _, body) = get_json(rig.router.clone(), "/api/v3/auth/setup/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": false}));

    let (status, _, body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/setup",
        json!({"username": "second", "password": "second-admin-password-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("CONFLICT"));
    assert_eq!(
        body["error"]["message"],
        json!("Setup has already been completed")
    );
}

#[tokio::test]
async fn concurrent_setups_yield_exactly_one_admin() {
    let rig = native_rig();
    rig.rig.clock.set(now_unix());
    let first = post_json(
        rig.router.clone(),
        "/api/v3/auth/setup",
        json!({"username": "root", "password": "first-admin-password-1"}),
    );
    let second = post_json(
        rig.router.clone(),
        "/api/v3/auth/setup",
        json!({"username": "other", "password": "second-admin-password-1"}),
    );
    let ((a_status, _, _), (b_status, _, _)) = tokio::join!(first, second);
    assert!(
        (a_status == StatusCode::CREATED && b_status == StatusCode::CONFLICT)
            || (a_status == StatusCode::CONFLICT && b_status == StatusCode::CREATED),
        "exactly one setup must win, got {a_status} and {b_status}"
    );
    let (status, _, body) = get_json(rig.router.clone(), "/api/v3/auth/setup/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": false}));
}

#[tokio::test]
async fn setup_validates_input() {
    let rig = native_rig();
    let (status, _, body) = post_json(
        rig.router.clone(),
        "/api/v3/auth/setup",
        json!({"username": "root", "password": "short"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("INVALID_INPUT"));

    let (status, _, _) = post_json(
        rig.router.clone(),
        "/api/v3/auth/setup",
        json!({"username": "ab", "password": "long-enough-password-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Federated
// ---------------------------------------------------------------------------

fn oidc_config() -> OidcConfig {
    FakeOidcIdp::config()
}

fn oidc_idp() -> FakeOidcIdp {
    FakeOidcIdp::new(
        DiscoveryDoc {
            issuer: "https://idp.test".to_owned(),
            authorization_endpoint: "https://idp.test/authorize".to_owned(),
            token_endpoint: "https://idp.test/token".to_owned(),
            userinfo_endpoint: Some("https://idp.test/userinfo".to_owned()),
        },
        OidcTokens {
            access_token: "oidc-access".to_owned(),
            refresh_token: "oidc-refresh".to_owned(),
            id_token: "oidc-id".to_owned(),
        },
        RawClaims {
            sub: Some("oidc-sub-1".to_owned()),
            email: Some("Ada@Test.com".to_owned()),
            name: Some("Ada".to_owned()),
            preferred_username: None,
            nickname: None,
            picture: None,
            avatar: None,
        },
    )
}

fn oidc_router_with(config: OidcConfig) -> Router {
    let rig = TestRig::new().expect("test crypto key");
    let login = OidcLogin::new(
        FakeUserStore::new(),
        oidc_idp(),
        FakeOidcStates::new(),
        FakeOidcExchanges::new(),
        FakeSessionIssuer::new(),
    );
    let state = OidcRouteState::new(login, StaticOidcConfig(config), rig.deps.ids.clone(), "");
    Router::new().nest("/api/v3", oidc_router(state))
}

fn jellyfin_router_with(configured: bool) -> Router {
    let rig = TestRig::new().expect("test crypto key");
    let mut idp = FakeJellyfinIdp::new(configured);
    idp.accept(
        "jf",
        "secret-99",
        JellyfinProfile {
            jellyfin_user_id: "jf-user-1".to_owned(),
            username: "Jf".to_owned(),
            access_token: "jf-token".to_owned(),
            avatar_url: None,
        },
    );
    let state = JellyfinRouteState::new(
        FakeUserStore::new(),
        idp,
        FakeJellyfinLink::new(),
        FakeSessionIssuer::new(),
        rig.deps.ids.clone(),
        "",
    );
    Router::new().nest("/api/v3", jellyfin_router(state))
}

fn plex_router_with(client: FakePlexPinClient) -> Router {
    let rig = TestRig::new().expect("test crypto key");
    let state = PlexRouteState::new(
        FakeUserStore::new(),
        client,
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
        rig.deps.ids.clone(),
        "",
    );
    Router::new().nest("/api/v3", plex_router(state))
}

fn plex_client() -> FakePlexPinClient {
    let mut client = FakePlexPinClient::new();
    client.accounts.insert(
        "auth-1".to_owned(),
        PlexAccount {
            uuid: "plex-uuid-1".to_owned(),
            email: "plex@test.com".to_owned(),
            display_name: "Plex".to_owned(),
            thumb: None,
        },
    );
    client
}

#[tokio::test]
async fn oidc_authorize_returns_browser_url() {
    let app = oidc_router_with(oidc_config());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/oidc/authorize")
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    let url = body["authorize_url"].as_str().expect("authorize url");
    assert!(url.starts_with("https://idp.test/authorize?response_type=code&"));
    assert!(url.contains("state="));
    assert!(url.contains("code_challenge="));
    assert!(url.contains("code_challenge_method=S256"));
}

#[tokio::test]
async fn oidc_callback_and_exchange_mint_session() {
    let app = oidc_router_with(oidc_config());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/oidc/authorize")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let authorize: Value = serde_json::from_slice(&bytes).expect("body is json");
    let url = authorize["authorize_url"].as_str().expect("url");
    let state = url
        .split("state=")
        .nth(1)
        .expect("state param")
        .split("&code_challenge")
        .next()
        .expect("state value");

    let callback = format!("/api/v3/auth/oidc/callback?code=authcode-1&state={state}");
    let request = Request::builder()
        .method("GET")
        .uri(&callback)
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::FOUND);
    assert!(no_store(response.headers()));
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .expect("redirect")
        .to_str()
        .expect("ascii")
        .to_owned();
    assert!(location.starts_with("/auth/callback?code="), "{location}");
    assert!(!location.contains("://"), "same-site relative redirect");
    let code = location.split("code=").nth(1).expect("exchange code");

    let (status, headers, body) = post_json(
        app.clone(),
        "/api/v3/auth/oidc/exchange",
        json!({"code": code}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(no_store(&headers));
    assert_eq!(body["username"], json!("ada"));
    assert!(body.get("token").is_none(), "exchange defaults to cookie");
    let token = set_cookie_token(&headers).expect("session cookie");
    assert!(token.starts_with("test-session-"));

    // Single-use: replaying the code fails, and so does a forged state.
    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/oidc/exchange",
        json!({"code": code}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Invalid or expired code"));

    let request = Request::builder()
        .method("GET")
        .uri("/api/v3/auth/oidc/callback?code=x&state=forged")
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_disabled_is_a_fixed_503() {
    let mut config = oidc_config();
    config.enabled = false;
    let app = oidc_router_with(config);
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/oidc/authorize")
        .body(Body::empty())
        .expect("request builds");
    let response = app.oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(body["error"]["code"], json!("UPSTREAM_ERROR"));
    assert_eq!(body["error"]["message"], json!("Upstream service error"));
    assert!(body["error"]["details"]["error_id"].is_string());
}

#[tokio::test]
async fn jellyfin_login_round_trip() {
    let app = jellyfin_router_with(true);
    let (status, headers, body) = post_json(
        app.clone(),
        "/api/v3/auth/jellyfin/login",
        json!({"username": "jf", "password": "secret-99"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(no_store(&headers));
    assert_eq!(body["display_name"], json!("Jf"));
    assert!(body.get("token").is_none());
    assert!(set_cookie_token(&headers).is_some());

    let (status, headers, body) = post_json(
        app,
        "/api/v3/auth/jellyfin/login",
        json!({"username": "jf", "password": "wrong-99"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Invalid credentials"));
    assert_eq!(
        headers.get(axum::http::header::WWW_AUTHENTICATE),
        Some(&"Bearer".parse().expect("challenge parses")),
    );
}

#[tokio::test]
async fn jellyfin_unconfigured_is_still_a_plain_401() {
    let app = jellyfin_router_with(false);
    let (status, _, body) = post_json(
        app,
        "/api/v3/auth/jellyfin/login",
        json!({"username": "jf", "password": "secret-99"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Invalid credentials"));
}

#[tokio::test]
async fn plex_login_journey_from_start_to_session() {
    let client = plex_client();
    let app = plex_router_with(client.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/plex/start")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let started: Value = serde_json::from_slice(&bytes).expect("body is json");
    let pin_id = started["pin_id"].as_i64().expect("pin id");
    assert!(
        started["authorize_url"]
            .as_str()
            .expect("url")
            .starts_with("https://app.plex.tv/auth#?")
    );

    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/plex/poll/login",
        json!({"pin_id": pin_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"completed": false}));

    client.authorize_pin(pin_id, "auth-1");
    let (status, headers, body) = post_json(
        app,
        "/api/v3/auth/plex/poll/login",
        json!({"pin_id": pin_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(no_store(&headers));
    assert_eq!(body["completed"], json!(true));
    assert_eq!(body["user"]["display_name"], json!("Plex"));
    assert!(body.get("token").is_none());
    assert!(set_cookie_token(&headers).is_some());
}

#[tokio::test]
async fn plex_link_and_connect_return_no_session() {
    let mut client = plex_client();
    client.machine_id = Some("machine-1".to_owned());
    client
        .server_ids
        .insert("auth-1".to_owned(), vec!["machine-1".to_owned()]);
    client.server_tokens.insert(
        ("auth-1".to_owned(), "machine-1".to_owned()),
        "srv-token".to_owned(),
    );
    let app = plex_router_with(client.clone());

    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/plex/start")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let started: Value = serde_json::from_slice(&bytes).expect("body is json");
    let pin_id = started["pin_id"].as_i64().expect("pin id");

    let (status, _, body) = post_json_authed(
        app.clone(),
        "/api/v3/auth/plex/poll/link",
        json!({"pin_id": pin_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"completed": false}));

    client.authorize_pin(pin_id, "auth-1");
    let (status, link_headers, body) = post_json_authed(
        app.clone(),
        "/api/v3/auth/plex/poll/link",
        json!({"pin_id": pin_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["completed"], json!(true));
    assert_eq!(body["profile"]["uuid"], json!("plex-uuid-1"));
    assert_eq!(body["profile"]["server_access_token"], json!("srv-token"));
    assert!(
        link_headers.get(axum::http::header::SET_COOKIE).is_none(),
        "link mints no session"
    );

    let (status, _, body) = post_json_authed(
        app,
        "/api/v3/auth/plex/poll/connect",
        json!({"pin_id": pin_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"completed": true, "auth_token": "auth-1"}));
}

#[tokio::test]
async fn plex_link_and_connect_polls_401_without_session() {
    let app = plex_router_with(plex_client());
    for path in [
        "/api/v3/auth/plex/poll/link",
        "/api/v3/auth/plex/poll/connect",
    ] {
        let (status, headers, body) = post_json(app.clone(), path, json!({"pin_id": 424242})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} needs a session");
        assert_eq!(body["error"]["code"], json!("UNAUTHORIZED"));
        assert_eq!(
            headers.get(axum::http::header::WWW_AUTHENTICATE),
            Some(&"Bearer".parse().expect("challenge parses")),
            "{path} carries the Bearer challenge"
        );
    }
}

#[tokio::test]
async fn plex_poll_rejection_is_403() {
    let client = plex_client();
    let app = plex_router_with(client.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/plex/start")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let started: Value = serde_json::from_slice(&bytes).expect("body is json");
    let pin_id = started["pin_id"].as_i64().expect("pin id");

    client.authorize_pin(pin_id, "unknown-token");
    let (status, _, body) = post_json(
        app,
        "/api/v3/auth/plex/poll/login",
        json!({"pin_id": pin_id}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], json!("FORBIDDEN"));
    assert_eq!(body["error"]["message"], json!("Access denied"));
}

// ---------------------------------------------------------------------------
// Allowlist proof through the real middleware
// ---------------------------------------------------------------------------

/// Every route built here must be reachable without credentials (the handler
/// answers, not the middleware), while a non-public path still 401s.
#[tokio::test]
async fn public_auth_routes_answer_without_credentials() {
    let native = native_rig();
    native
        .rig
        .users
        .insert(UserRecord {
            id: "user-9".to_owned(),
            username: Some("carol".to_owned()),
            username_display: Some("Carol".to_owned()),
            display_name: "Carol".to_owned(),
            email: None,
            avatar_url: None,
            role: Role::Admin,
            created_at: 1_700_000_000,
            last_login_at: None,
        })
        .await
        .expect("seed user");

    let rig = TestRig::new().expect("test crypto key");
    let ids = rig.deps.ids.clone();
    let oidc_login = OidcLogin::new(
        FakeUserStore::new(),
        oidc_idp(),
        FakeOidcStates::new(),
        FakeOidcExchanges::new(),
        FakeSessionIssuer::new(),
    );
    let oidc = OidcRouteState::new(oidc_login, StaticOidcConfig(oidc_config()), ids.clone(), "");
    let mut jf_idp = FakeJellyfinIdp::new(true);
    jf_idp.accept(
        "jf",
        "secret-99",
        JellyfinProfile {
            jellyfin_user_id: "jf-user-1".to_owned(),
            username: "Jf".to_owned(),
            access_token: "jf-token".to_owned(),
            avatar_url: None,
        },
    );
    let jellyfin = JellyfinRouteState::new(
        FakeUserStore::new(),
        jf_idp,
        FakeJellyfinLink::new(),
        FakeSessionIssuer::new(),
        ids.clone(),
        "",
    );
    let plex = PlexRouteState::new(
        FakeUserStore::new(),
        plex_client(),
        FakePlexLink::new(),
        FakeSessionIssuer::new(),
        ids,
        "",
    );

    // Rebuild the native state (the rig router is already nested; merge needs
    // the un-nested routers).
    let verifier = FakePasswordVerifier::new();
    let credentials = FakeUserTable::with_user("ada", "correct-horse-99", "user-1", "Ada");
    let deps = UsersDeps {
        sessions: Arc::new(native.sessions.clone()),
        ..native.rig.deps.clone()
    };
    let native_state =
        NativeAuthState::new(native.sessions.clone(), verifier, credentials, deps, "");
    let v3 = Router::new()
        .merge(native_auth_router(native_state))
        .merge(oidc_router(oidc))
        .merge(jellyfin_router(jellyfin))
        .merge(plex_router(plex))
        .route("/auth/sessions", get(|| async { "guarded" }));
    let app = Router::new()
        .nest("/api/v3", v3)
        .layer(middleware::from_fn_with_state(
            SessionAuth::new(native.sessions.clone(), ""),
            require_session,
        ));

    // Guarded probe: the middleware answers, proving it is active.
    let (status, _, body) = get_json(app.clone(), "/api/v3/auth/sessions").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Not authenticated"));

    // Each public route answers from its handler (never the middleware 401).
    let (status, _, body) = get_json(app.clone(), "/api/v3/auth/setup/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": false}));

    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/logout")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/login",
        json!({"username": "ada", "password": "wrong-password-99"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["error"]["message"],
        json!("Invalid username or password")
    );

    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/setup",
        json!({"username": "late", "password": "late-admin-password-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["error"]["message"],
        json!("Setup has already been completed")
    );

    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/oidc/authorize")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::OK);

    let request = Request::builder()
        .method("GET")
        .uri("/api/v3/auth/oidc/callback?code=x&state=y")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(
        body["error"]["message"],
        json!("OIDC authentication failed")
    );

    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/oidc/exchange",
        json!({"code": "nope"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Invalid or expired code"));

    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/jellyfin/login",
        json!({"username": "jf", "password": "wrong-99"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], json!("Invalid credentials"));

    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/plex/start")
        .body(Body::empty())
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::OK);

    let (status, _, body) = post_json(
        app.clone(),
        "/api/v3/auth/plex/poll/login",
        json!({"pin_id": 424242}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login poll reachable");
    assert_eq!(body["completed"], json!(false), "login poll pending");

    // The link and connect polls hand out account Bearer tokens and stay behind the
    // session gate: the middleware answers before the handler runs.
    for path in [
        "/api/v3/auth/plex/poll/link",
        "/api/v3/auth/plex/poll/connect",
    ] {
        let (status, _, body) = post_json(app.clone(), path, json!({"pin_id": 424242})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} needs a session");
        assert_eq!(body["error"]["message"], json!("Not authenticated"));
    }
}

#[tokio::test]
async fn logout_without_credential_still_clears() {
    let rig = native_rig();
    let request = Request::builder()
        .method("POST")
        .uri("/api/v3/auth/logout")
        .body(Body::empty())
        .expect("request builds");
    let response = rig.router.clone().oneshot(request).await.expect("responds");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response
            .headers()
            .contains_key(axum::http::header::SET_COOKIE),
        "stale clients always get the clear"
    );
}
