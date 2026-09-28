//! Session-core briefs (stage-0 auth D1-D3): allowlist, transports, origin
//! check, rate classes, dummy-verify shape, debug CORS list.
//!
//! These briefs drive the wired slice through a real axum router with the
//! in-memory store and fake users (see `session::middleware` docs for the
//! layer order). No live IdP, no network, no sleeps.

use droppedneedle::auth::session;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use session::{
    allowlist,
    cookies::{self, COOKIE_NAME},
    cors,
    extract::{Transport, UNAUTHORIZED},
    login::{
        self, FakePasswordVerifier, FakeUserTable, INVALID_CREDENTIALS, LoginContext, LoginRequest,
        LoginService, NO_STORE,
    },
    middleware::{CurrentSession, SessionAuth, TrustedProxies, require_session},
    origin::{OriginDecision, check_origin},
    rate_limit::{self, RATE_CLASSES, RateLimiterSet, classify},
    store::{MemorySessionStore, SessionKind, SessionRecord, SessionStore as _, now_unix},
    tokens,
};
use tower::ServiceExt as _;

type TestSessions = MemorySessionStore;
type TestLogin = LoginService<TestSessions, FakePasswordVerifier, FakeUserTable>;

#[derive(Clone)]
struct TestState {
    login: TestLogin,
    verifier: FakePasswordVerifier,
}

const HOST: &str = "app.test";

fn test_state() -> (TestState, TestSessions) {
    let sessions = TestSessions::new();
    let verifier = FakePasswordVerifier::new();
    let users = FakeUserTable::with_user("ada", "correct-horse-99", "user-1", "Ada");
    let login = TestLogin::new(sessions.clone(), verifier.clone(), users);
    (TestState { login, verifier }, sessions)
}

async fn login_handler(
    State(state): State<TestState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let ctx = LoginContext {
        base_path: String::new(),
        secure: cookies::is_secure("http", &headers),
        user_agent: headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        now_unix: now_unix(),
    };
    match state.login.login(body, ctx).await {
        Ok(success) => {
            let user = json!({"id": success.user_id, "display_name": success.display_name});
            login::login_response(user, &success)
        }
        Err(error) => login::login_error_response(&error),
    }
}

async fn me_handler(axum::Extension(session): axum::Extension<CurrentSession>) -> Response {
    Json(json!({"user_id": session.user_id, "session_id": session.session_id})).into_response()
}

async fn things_handler() -> Response {
    Json(json!({"ok": true})).into_response()
}

async fn logout_handler() -> Response {
    let mut response = Json(json!({"ok": true})).into_response();
    cookies::push_set_cookie(response.headers_mut(), &cookies::clear_cookie_value(""));
    response
}

fn test_app(auth: SessionAuth<TestSessions>, state: TestState) -> Router {
    Router::new()
        .route("/api/v3/auth/login", post(login_handler))
        .route("/api/v3/auth/logout", post(logout_handler))
        .route(
            "/api/v3/auth/providers",
            get(|| async { Json(json!({"providers": []})) }),
        )
        .route(
            "/api/v3/auth/setup",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/auth/setup/status",
            get(|| async { Json(json!({"setup_required": false})) }),
        )
        .route(
            "/api/v3/auth/password-recovery/reset",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/auth/jellyfin/login",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/auth/plex/start",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/auth/plex/poll/login",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/auth/oidc/exchange",
            post(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/me/connections/spotify/auth/callback",
            get(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/library/contributions/musicbrainz/callback",
            get(|| async { Json(json!({"ok": true})) }),
        )
        .route(
            "/api/v3/openapi.json",
            get(|| async { Json(json!({"openapi": "3.1.0"})) }),
        )
        .route("/api/v3/me", get(me_handler))
        .route("/api/v3/things", post(things_handler))
        .layer(middleware::from_fn_with_state(
            auth,
            require_session::<TestSessions>,
        ))
        .with_state(state)
}

async fn seed_session(sessions: &TestSessions, raw_token: &str) {
    let now = now_unix();
    let record = SessionRecord {
        id: "sess-seed".to_owned(),
        user_id: "user-1".to_owned(),
        token_hash: tokens::hash_token(raw_token),
        kind: SessionKind::Standard,
        label: None,
        issued_at: now,
        expires_at: tokens::expires_at(now),
        last_seen_at: now,
        revoked: false,
        user_agent: None,
    };
    sessions.insert(record).await.unwrap();
}

async fn body_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn login_body(username: &str, password: &str, transport: Option<&str>) -> Body {
    let mut value = json!({"username": username, "password": password});
    if let Some(transport) = transport {
        value["transport"] = Value::String(transport.to_owned());
    }
    Body::from(serde_json::to_vec(&value).unwrap())
}

#[tokio::test]
async fn cookie_login_sets_cookie_and_body_holds_no_token() {
    let (state, sessions) = test_state();
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/auth/login")
                .header("host", HOST)
                .header("content-type", "application/json")
                .body(login_body("ada", "correct-horse-99", Some("cookie")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("cache-control").unwrap(), NO_STORE);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        set_cookie.starts_with(&format!("{COOKIE_NAME}=")),
        "{set_cookie}"
    );
    assert!(set_cookie.contains("Path=/api/v3"), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=2592000"), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(
        !set_cookie.contains("Secure"),
        "plain HTTP must not mark Secure: {set_cookie}"
    );

    let body = body_json(response).await;
    assert_eq!(body["id"], "user-1");
    assert!(
        body.get("token").is_none(),
        "cookie body must not leak the token: {body}"
    );
}

#[tokio::test]
async fn cookie_session_authenticates_and_bearer_still_wins() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let app = test_app(SessionAuth::new(sessions, ""), state);

    let cookie = format!("{COOKIE_NAME}={raw}");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["user_id"], "user-1");

    // Bearer-then-cookie: a valid Bearer [REDACTED] beside a junk cookie still passes.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("cookie", format!("{COOKIE_NAME}=junk"))
                .header("authorization", format!("Bearer {raw}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn bearer_login_returns_token_once_and_sets_no_cookie() {
    let (state, sessions) = test_state();
    let app = test_app(SessionAuth::new(sessions.clone(), ""), state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/auth/login")
                .header("host", HOST)
                .header("content-type", "application/json")
                .body(login_body("ada", "correct-horse-99", Some("bearer")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(response.headers().get("cache-control").unwrap(), NO_STORE);
    let body = body_json(response).await;
    let token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 44);

    // The returned token authenticates as Bearer (shared store holds the mint).
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn login_defaults_to_cookie_and_marks_secure_on_proxied_https() {
    let (state, sessions) = test_state();
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/auth/login")
                .header("host", HOST)
                .header("x-forwarded-proto", "https")
                .header("content-type", "application/json")
                .body(login_body("ada", "correct-horse-99", None))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
    assert!(body_json(response).await.get("token").is_none());
}

#[tokio::test]
async fn origin_check_rejects_cross_origin_cookie_but_exempts_bearer() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let cookie = format!("{COOKIE_NAME}={raw}");

    // Cross-origin cookie mutation -> 403, no auth challenge (session is valid).
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/things")
                .header("host", HOST)
                .header("origin", "https://evil.test")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get("www-authenticate").is_none());
    assert_eq!(body_json(response).await["error"]["code"], "FORBIDDEN");

    // Bearer [REDACTED] the same foreign origin -> passes.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/things")
                .header("host", HOST)
                .header("origin", "https://evil.test")
                .header("authorization", format!("Bearer {raw}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Same-origin cookie mutation -> passes; missing origin on POST fails closed.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/things")
                .header("host", HOST)
                .header("origin", format!("http://{HOST}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/things")
                .header("host", HOST)
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Safe methods need no origin even on cookies.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn allowlist_is_open_and_everything_else_is_401() {
    let (state, sessions) = test_state();
    let app = test_app(SessionAuth::new(sessions, ""), state);

    let public: &[(&str, &str)] = &[
        ("GET", "/api/v3/auth/setup/status"),
        ("GET", "/api/v3/auth/providers"),
        ("POST", "/api/v3/auth/setup"),
        ("POST", "/api/v3/auth/password-recovery/reset"),
        ("POST", "/api/v3/auth/logout"),
        ("POST", "/api/v3/auth/plex/start"),
        ("POST", "/api/v3/auth/plex/poll/login"),
        ("POST", "/api/v3/auth/jellyfin/login"),
        ("POST", "/api/v3/auth/oidc/exchange"),
        ("GET", "/api/v3/me/connections/spotify/auth/callback"),
        ("GET", "/api/v3/library/contributions/musicbrainz/callback"),
        ("GET", "/api/v3/openapi.json"),
    ];
    for (method, path) in public {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(*method)
                    .uri(*path)
                    .header("host", HOST)
                    .header("content-type", "application/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{method} {path} must be public"
        );
    }

    // Unmounted but allowlisted paths pass the gate (404 from the router, not 401).
    for path in ["/api/v3/auth/oidc/authorize", "/api/v3/auth/oidc/callback"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{path} must pass the gate"
        );
    }

    // Protected paths without creds -> 401 envelope + Bearer [REDACTED]
    for (method, path) in [
        ("GET", "/api/v3/me"),
        ("POST", "/api/v3/things"),
        ("GET", "/api/v3/nope"),
        ("POST", "/api/v3/auth/plex/poll/link"),
        ("POST", "/api/v3/auth/plex/poll/connect"),
        ("POST", "/api/v3/auth/plex/poll"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        assert_eq!(
            response.headers().get("www-authenticate").unwrap(),
            "Bearer"
        );
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], UNAUTHORIZED);
        assert!(body["error"]["details"].is_null());
    }

    // A bogus token fails exactly like a missing one.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("authorization", "Bearer bogus")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get("www-authenticate").unwrap(),
        "Bearer"
    );
}

#[tokio::test]
async fn logout_is_public_and_clears_the_cookie() {
    let (state, sessions) = test_state();
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/auth/logout")
                .header("host", HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/api/v3"), "{set_cookie}");
}

#[tokio::test]
async fn login_failures_are_indistinguishable_and_run_one_verify_each() {
    let (state, sessions) = test_state();
    let app = test_app(SessionAuth::new(sessions, ""), state.clone());

    let bad_user = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/auth/login")
                .header("host", HOST)
                .header("content-type", "application/json")
                .body(login_body("nobody", "whatever", Some("cookie")))
                .unwrap(),
        )
        .await
        .unwrap();
    let bad_password = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v3/auth/login")
                .header("host", HOST)
                .header("content-type", "application/json")
                .body(login_body("ada", "wrong", Some("cookie")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(bad_user.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(bad_password.status(), StatusCode::UNAUTHORIZED);
    let user_body = body_json(bad_user).await;
    let password_body = body_json(bad_password).await;
    assert_eq!(
        user_body, password_body,
        "no user enumeration by body shape"
    );
    assert_eq!(user_body["error"]["code"], UNAUTHORIZED);
    assert_eq!(user_body["error"]["message"], INVALID_CREDENTIALS);

    // Timing shape: unknown users cost one dummy verify, known users one real verify.
    assert_eq!(state.verifier.dummy_calls(), 1);
    assert_eq!(state.verifier.verify_calls(), 1);
}

#[tokio::test]
async fn rate_classes_keep_v2_numbers_and_new_rows_match_login() {
    assert_eq!(RATE_CLASSES.len(), 7);
    assert_eq!(classify("/api/v3/me").name, "default");
    assert_eq!(
        (
            classify("/api/v3/me").rate_per_sec,
            classify("/api/v3/me").capacity
        ),
        (30.0, 60)
    );
    assert_eq!(classify("/api/v3/auth/login").name, "login");
    assert_eq!(classify("/api/v3/auth/setup").name, "setup");
    assert_eq!(classify("/api/v3/auth/plex/poll").name, "plex");

    let start = Instant::now();
    let limits = RateLimiterSet::new(start);
    let (login_bucket, _) = limits.bucket_for("/api/v3/auth/login");
    for _ in 0..5 {
        assert!(login_bucket.try_acquire_at(start).allowed);
    }
    let denied = login_bucket.try_acquire_at(start);
    assert!(!denied.allowed);
    assert!(denied.retry_after_secs >= 1);

    let (setup_bucket, _) = limits.bucket_for("/api/v3/auth/setup");
    for _ in 0..3 {
        assert!(setup_bucket.try_acquire_at(start).allowed);
    }
    assert!(!setup_bucket.try_acquire_at(start).allowed);

    // 429 shape: fixed envelope, Retry-After, zeroed budget headers.
    let response = rate_limit::rate_limited_response(classify("/api/v3/auth/login"), 2);
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers().get("retry-after").unwrap(), "2");
    assert_eq!(response.headers().get("x-ratelimit-limit").unwrap(), "5");
    assert_eq!(
        response.headers().get("x-ratelimit-remaining").unwrap(),
        "0"
    );
    assert_eq!(body_json(response).await["error"]["code"], "RATE_LIMITED");
}

#[tokio::test]
async fn rate_layer_caps_login_burst_over_http() {
    let (state, sessions) = test_state();
    let limits = Arc::new(RateLimiterSet::new(Instant::now()));
    let app = Router::new()
        .route("/api/v3/auth/login", post(login_handler))
        .layer(middleware::from_fn_with_state(
            SessionAuth::new(sessions, ""),
            require_session::<TestSessions>,
        ))
        .layer(middleware::from_fn_with_state(
            limits,
            rate_limit::rate_limit,
        ))
        .with_state(state);

    let mut statuses = Vec::new();
    for _ in 0..6 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v3/auth/login")
                    .header("host", HOST)
                    .header("content-type", "application/json")
                    .body(login_body("ada", "wrong", Some("cookie")))
                    .unwrap(),
            )
            .await
            .unwrap();
        statuses.push(response.status());
    }
    assert_eq!(
        statuses,
        vec![StatusCode::UNAUTHORIZED; 5]
            .into_iter()
            .chain([StatusCode::TOO_MANY_REQUESTS])
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn debug_cors_list_stays_localhost_only() {
    assert!(cors::is_debug_origin("http://localhost:5173"));
    assert!(cors::is_debug_origin("http://127.0.0.1:3000"));
    assert!(!cors::is_debug_origin("https://music.example.com"));
    assert!(!allowlist::is_public("/api/v3/docs"));
}

#[tokio::test]
async fn bearer_then_cookie_order_is_pinned() {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer tok-b".parse().unwrap());
    headers.insert("cookie", format!("{COOKIE_NAME}=tok-c").parse().unwrap());
    assert_eq!(
        session::extract::extract(&headers),
        Some(("tok-b".to_owned(), Transport::Bearer))
    );
    assert_eq!(
        check_origin(
            &HeaderMap::new(),
            HOST,
            &axum::http::Method::POST,
            Transport::Bearer
        ),
        OriginDecision::Allow
    );
}

// ---------------------------------------------------------------------------
// Trusted proxies (M1) + extraction edges (m2)
// ---------------------------------------------------------------------------

/// Attach a peer address to a built request, simulating `ConnectInfo`.
fn with_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    request
}

async fn cookie_mutation(
    app: Router,
    cookie: &str,
    origin: &str,
    forwarded_host: Option<&str>,
    peer: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/v3/things")
        .header("host", HOST)
        .header("origin", origin)
        .header("cookie", cookie);
    if let Some(host) = forwarded_host {
        builder = builder.header("x-forwarded-host", host);
    }
    let request = builder.body(Body::empty()).unwrap();
    let request = match peer {
        Some(addr) => with_peer(request, addr),
        None => request,
    };
    app.oneshot(request).await.unwrap()
}

#[tokio::test]
async fn forwarded_host_from_trusted_proxy_satisfies_origin_check() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let trusted = TrustedProxies::parse("10.0.0.0/8").unwrap();
    let app = test_app(
        SessionAuth::new(sessions, "").with_trusted_proxies(trusted),
        state,
    );
    let cookie = format!("{COOKIE_NAME}={raw}");

    // The proxy's host satisfies an origin the direct host would reject.
    let response = cookie_mutation(
        app.clone(),
        &cookie,
        "https://proxy.test",
        Some("proxy.test"),
        Some("10.9.8.7:1234"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Control: without the forwarded host the same origin is foreign.
    let response = cookie_mutation(
        app,
        &cookie,
        "https://proxy.test",
        None,
        Some("10.9.8.7:1234"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn forwarded_host_from_untrusted_peer_is_ignored() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let cookie = format!("{COOKIE_NAME}={raw}");

    // Spoofed forwarded host matching the attacker's origin: ignored, 403.
    let response = cookie_mutation(
        app.clone(),
        &cookie,
        "https://proxy.test",
        Some("proxy.test"),
        Some("203.0.113.9:1234"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // No peer info at all fails closed the same way.
    let response =
        cookie_mutation(app, &cookie, "https://proxy.test", Some("proxy.test"), None).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn spoofed_forwarded_proto_cannot_move_secure() {
    let mut claimed_https = HeaderMap::new();
    claimed_https.insert("x-forwarded-proto", "https".parse().unwrap());
    assert!(!cookies::is_secure_trusted("http", &claimed_https, false));
    assert!(cookies::is_secure_trusted("http", &claimed_https, true));

    let mut claimed_http = HeaderMap::new();
    claimed_http.insert("x-forwarded-proto", "http".parse().unwrap());
    assert!(cookies::is_secure_trusted("https", &claimed_http, false));
}

#[tokio::test]
async fn bearer_scheme_is_case_insensitive_over_http() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("authorization", format!("bEaReR {raw}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn empty_bearer_does_not_fall_through_to_cookie() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v3/me")
                .header("host", HOST)
                .header("authorization", "Bearer ")
                .header("cookie", format!("{COOKIE_NAME}={raw}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn non_utf8_cookie_header_does_not_abort_scan() {
    let (state, sessions) = test_state();
    let raw = tokens::mint_token().unwrap();
    seed_session(&sessions, &raw).await;
    let app = test_app(SessionAuth::new(sessions, ""), state);
    let mut request = Request::builder()
        .uri("/api/v3/me")
        .header("host", HOST)
        .body(Body::empty())
        .unwrap();
    request.headers_mut().append(
        axum::http::header::COOKIE,
        HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
    );
    request.headers_mut().append(
        axum::http::header::COOKIE,
        format!("{COOKIE_NAME}={raw}").parse().unwrap(),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
