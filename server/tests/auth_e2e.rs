//! Stage-3 E2E: real-app auth journeys plus the standing auth contract.
//!
//! Everything here runs against `create_app` with the production SQLite
//! bundle over scratch databases. The only fakes in the building are the
//! ones the task allows: none, in practice — HIBP screening is switched off
//! through its real config knob, and no test touches an IdP, mail, or the
//! network at all.
//!
//! Tests (each owns a scratch dir under `temp_dir`, parallel-safe):
//!
//! - `journey_a_*`: setup → cookie login → list/revoke sessions → logout-all
//!   → re-login works, old cookie dead.
//! - `journey_b_*`: admin creates user → role change → user login → user hits
//!   an admin route (403) → admin deletes user → user session dead.
//! - `journey_c_*`: app-password create → compat contracts accept → native
//!   paths reject → revoke → compat dead.
//! - `auth_on_every_endpoint`: the standing contract. Every `/api/v3` route
//!   in the OpenAPI doc must have a matrix row; every non-allowlisted route
//!   401s anonymously with a Bearer [REDACTED] admin routes 403 for plain users,
//!   and the admin is admitted everywhere. Add a route without a row and this
//!   fails by name.
//! - `setup_edges`: setup-status flip, once-only setup, login before setup.
//! - `login_p95_*`: 50 sequential logins against the 600ms login budget
//!   (BUDGETS.md). Argon2id work-factor cost counts - it is the budget.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::auth::compat_auth::jellyfin::{
    JellyfinPasswordStore, JellyfinStoreError, JellyfinUser, authenticate_by_name, resolve_token,
};
use droppedneedle::auth::compat_auth::subsonic::{
    AppSecret, PARAM_MISSING, SubsonicDenied, SubsonicParams, SubsonicPasswordStore,
    SubsonicStoreError, WRONG_CREDENTIALS, authenticate, md5_hex,
};
use droppedneedle::auth::prod::ProdAuth;
use droppedneedle::auth::session::cookies::COOKIE_NAME;
use droppedneedle::auth::session::tokens;
use droppedneedle::auth::users::UsersDeps;
use droppedneedle::auth::users::clock_now;
use droppedneedle::auth::users::models::UserRecord;
use droppedneedle::auth::users::stores::SystemClock;
use droppedneedle::auth::wiring::AuthSetup;
use droppedneedle::config::DEFAULT_PORT;
use droppedneedle::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle::docs::ApiDoc;
use droppedneedle::http_client::HttpClientFactory;
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::runtime_config::sections::SecuritySettings;
use droppedneedle::runtime_config::{ConfigStore, Crypto};
use droppedneedle::{AppConfig, AppState, create_app};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use utoipa::OpenApi as _;

/// Fixed host for every request; cookie mutations also send this as Origin.
const HOST: &str = "e2e.test";
const ORIGIN: &str = "http://e2e.test";

/// Scratch-dir sequence so parallel tests never share a database.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// One scratch deployment: migrated database, production adapters, config.
struct E2e {
    /// Held, never read: dropping it would close the pool out from under
    /// the adapters.
    #[allow(dead_code)]
    runtime: DbRuntime,
    bundle: ProdAuth,
    store: Arc<ConfigStore>,
    crypto: Arc<Crypto>,
    http: HttpClientFactory,
    ids: Arc<UuidGenerator>,
    clock: Arc<SystemClock>,
}

impl E2e {
    async fn open(tag: &str) -> Self {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-auth-e2e-{tag}-{}-{seq}",
            std::process::id()
        ));
        let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .expect("scratch runtime opens");
        let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32]).expect("test key"));
        let store = Arc::new(
            ConfigStore::open(
                &dir.join("config.json"),
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("scratch config opens"),
        );
        // Breach screening off via the real knob: setup and user creation
        // must never dial the network in this suite.
        let mut security: SecuritySettings = store.get().expect("security section reads");
        security.hibp_check = false;
        store.save(security).expect("hibp switch saves");

        let ids = Arc::new(UuidGenerator);
        let clock = Arc::new(SystemClock);
        let bundle = ProdAuth::new(
            runtime.pool(),
            runtime.lane(),
            Arc::clone(&crypto),
            Arc::clone(&ids) as Arc<dyn IdGenerator>,
            Arc::clone(&clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            &dir.join("avatars"),
        );
        let http = HttpClientFactory::new().expect("http factory builds");
        Self {
            runtime,
            bundle,
            store,
            crypto,
            http,
            ids,
            clock,
        }
    }

    /// A fresh router over the same database. Each build carries fresh rate
    /// buckets, so multi-pass tests rebuild instead of tripping the limiter.
    fn router(&self) -> Router {
        let auth = AuthSetup::build(
            self.bundle.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
        )
        .expect("prod auth bundle builds");
        let state = AppState::new(
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            self.http.clone(),
            AppConfig::new(DEFAULT_PORT),
            auth,
        );
        create_app(state)
    }

    fn users(&self) -> UsersDeps {
        // Rebuild is cheap, but the stores are what matter: clone the deps
        // through one throwaway bundle so the compat adapter reads live rows.
        AuthSetup::build(
            self.bundle.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
        )
        .expect("prod auth bundle builds")
        .users
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// One request through the real app. Every call carries Host; cookie
/// mutations additionally need Origin (pass `with_origin`).
async fn call(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value, HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .oneshot(builder.body(body).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is json")
    };
    (status, json, headers)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}")
}

/// Raw session token from a Set-Cookie response.
fn set_cookie_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::SET_COOKIE)?.to_str().ok()?;
    let first = value.split(';').next()?.trim();
    first
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .map(str::to_owned)
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// First-run setup on a fresh app. Returns (admin id, cookie-or-token).
async fn setup_admin(
    app: Router,
    username: &str,
    password: &str,
    transport: &str,
) -> (Value, Value) {
    let (status, body, headers) = call(
        app,
        "POST",
        "/api/v3/auth/setup",
        &[],
        Some(json!({
            "username": username,
            "password": password,
            "display_name": username,
            "transport": transport,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "setup must succeed: {body}");
    let credential = if transport == "bearer" {
        body["token"].clone()
    } else {
        Value::String(set_cookie_token(&headers).expect("setup sets a cookie"))
    };
    (body["user"].clone(), credential)
}

async fn login(
    app: Router,
    username: &str,
    password: &str,
    transport: &str,
) -> (StatusCode, Value, HeaderMap) {
    call(
        app,
        "POST",
        "/api/v3/auth/login",
        &[],
        Some(json!({
            "username": username,
            "password": password,
            "transport": transport,
        })),
    )
    .await
}

// ---------------------------------------------------------------------------
// Journey A: setup → login → sessions → logout-all → re-login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn journey_a_setup_login_sessions_logoutall_relogin() {
    let e2e = E2e::open("journey-a").await;

    let (status, body, _) = call(e2e.router(), "GET", "/api/v3/auth/setup/status", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": true}));

    let (_admin, setup_cookie) =
        setup_admin(e2e.router(), "e2e-admin", "e2e-admin-password-1", "cookie").await;
    let setup_cookie = setup_cookie.as_str().expect("setup cookie").to_owned();

    let (status, body, _) = call(e2e.router(), "GET", "/api/v3/auth/setup/status", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": false}));

    // Cookie login: session in the cookie, nothing sensitive in the body.
    let (status, body, headers) =
        login(e2e.router(), "e2e-admin", "e2e-admin-password-1", "cookie").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("token").is_none(), "cookie body leaks no token");
    assert_eq!(body["user"]["username"], json!("e2e-admin"));
    let session = set_cookie_token(&headers).expect("login sets a cookie");
    let jar = cookie(session.as_str());
    let origin = [("cookie", jar.as_str()), ("origin", ORIGIN)];

    // Two live sessions now (setup + login); the current one is marked.
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/auth/sessions",
        &[("cookie", jar.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sessions = body["sessions"].as_array().expect("session list");
    assert_eq!(sessions.len(), 2, "setup + login sessions: {body}");
    assert!(
        sessions.iter().any(|s| s["current"] == json!(true)),
        "one session is current: {body}"
    );
    assert!(
        sessions.iter().all(|s| s.get("token_hash").is_none()),
        "no credential material in listings: {body}"
    );
    let other = sessions
        .iter()
        .find(|s| s["current"] == json!(false))
        .expect("a non-current session")["id"]
        .as_str()
        .expect("session id")
        .to_owned();

    // Revoke the setup session; the login session survives.
    let (status, _, _) = call(
        e2e.router(),
        "DELETE",
        &format!("/api/v3/auth/sessions/{other}"),
        &origin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let setup_jar = cookie(setup_cookie.as_str());
    let (status, _, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/me",
        &[("cookie", setup_jar.as_str())],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "revoked setup cookie is dead"
    );
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/auth/sessions",
        &[("cookie", jar.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessions"].as_array().expect("list").len(), 1);

    // Logout-all kills the login session too.
    let (status, _, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/logout-all",
        &origin,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/me",
        &[("cookie", jar.as_str())],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "cookie dead after logout-all: {body}"
    );

    // Re-login works with the same password; the old cookie stays dead.
    let (status, _, headers) =
        login(e2e.router(), "e2e-admin", "e2e-admin-password-1", "cookie").await;
    assert_eq!(status, StatusCode::OK);
    let fresh = cookie(set_cookie_token(&headers).expect("fresh cookie").as_str());
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/me",
        &[("cookie", fresh.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/me",
        &[("cookie", jar.as_str())],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "pre-logout-all cookie stays dead"
    );
}

// ---------------------------------------------------------------------------
// Journey B: admin creates user → role change → user login → 403 → delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn journey_b_admin_user_lifecycle() {
    let e2e = E2e::open("journey-b").await;
    // Bearer [REDACTED] the admin leg: no Origin juggling, same real sessions.
    let (admin, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let admin_id = admin["id"].as_str().expect("admin id").to_owned();
    let admin_token = admin_token.as_str().expect("admin token").to_owned();
    let admin_auth = bearer(admin_token.as_str());
    let admin_headers = [("authorization", admin_auth.as_str())];

    // Admin creates a plain user.
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/admin/users",
        &admin_headers,
        Some(json!({"username": "molly", "password": "molly-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["role"], json!("user"));
    let molly_id = body["id"].as_str().expect("user id").to_owned();
    assert_ne!(molly_id, admin_id);

    // Role change takes effect on the next request (no cached role).
    let (status, body, _) = call(
        e2e.router(),
        "PUT",
        &format!("/api/v3/admin/users/{molly_id}/role"),
        &admin_headers,
        Some(json!({"role": "trusted"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], json!("trusted"));

    // The user logs in fine.
    let (status, body, headers) =
        login(e2e.router(), "molly", "molly-password-1234", "cookie").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["user"]["role"], json!("trusted"));
    let molly_jar = cookie(set_cookie_token(&headers).expect("user cookie").as_str());

    // ...but an admin route is forbidden, not leaked.
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/admin/users",
        &[("cookie", molly_jar.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "FORBIDDEN");

    // Admin deletes the user; the user's session dies with the account
    // (auth_tokens cascades off auth_users) and the password stops working.
    let (status, _, _) = call(
        e2e.router(),
        "DELETE",
        &format!("/api/v3/admin/users/{molly_id}"),
        &admin_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/me",
        &[("cookie", molly_jar.as_str())],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "deleted-user session is dead: {body}"
    );
    let (status, body, _) = login(e2e.router(), "molly", "molly-password-1234", "cookie").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        body["error"]["message"],
        json!("Invalid username or password")
    );
}

// ---------------------------------------------------------------------------
// Journey C: app passwords across the compat contracts, rejected natively
// ---------------------------------------------------------------------------

/// Compat-contract store over the REAL users tables: app-password rows plus
/// account rows only. Account passwords and native tokens are unreachable
/// here by construction, exactly like the stage-9 adapters will be.
#[derive(Clone)]
struct E2eCompatStore {
    users: UsersDeps,
    crypto: Arc<Crypto>,
}

impl E2eCompatStore {
    async fn user_row(&self, username_lower: &str) -> Option<UserRecord> {
        self.users
            .users
            .get_by_username(username_lower)
            .await
            .ok()?
    }

    fn jellyfin_user(row: &UserRecord) -> JellyfinUser {
        JellyfinUser {
            id: row.id.clone(),
            username: row.username.clone(),
            username_display: row.username_display.clone(),
            display_name: row.display_name.clone(),
            role: row.role.as_str().to_owned(),
        }
    }

    async fn touch(&self, secret: &str, client: Option<&str>) {
        let _ = self
            .users
            .app_passwords
            .touch(&tokens::hash_token(secret), clock_now(&self.users), client)
            .await;
    }
}

impl SubsonicPasswordStore for E2eCompatStore {
    async fn user_id_for_username(
        &self,
        username_lower: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        Ok(self.user_row(username_lower).await.map(|row| row.id))
    }

    async fn active_secrets(&self, user_id: &str) -> Result<Vec<AppSecret>, SubsonicStoreError> {
        let rows = self
            .users
            .app_passwords
            .list_active_by_user(user_id)
            .await
            .map_err(|_| SubsonicStoreError)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let plaintext = self
                .crypto
                .decrypt(&row.secret_encrypted)
                .map_err(|_| SubsonicStoreError)?;
            out.push(AppSecret {
                sha256: row.secret_sha256,
                plaintext,
            });
        }
        Ok(out)
    }

    async fn owner_of_secret(
        &self,
        secret_sha256: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        Ok(self
            .users
            .app_passwords
            .get_active_by_sha256(secret_sha256)
            .await
            .map_err(|_| SubsonicStoreError)?
            .map(|row| row.user_id))
    }

    async fn note_use(&self, secret_plaintext: &str, client: Option<&str>) {
        self.touch(secret_plaintext, client).await;
    }
}

impl JellyfinPasswordStore for E2eCompatStore {
    async fn user_for_token(
        &self,
        token: &str,
    ) -> Result<Option<JellyfinUser>, JellyfinStoreError> {
        let row = self
            .users
            .app_passwords
            .get_active_by_sha256(&tokens::hash_token(token))
            .await
            .map_err(|_| JellyfinStoreError)?;
        let Some(secret) = row else { return Ok(None) };
        let user = self
            .users
            .users
            .get_by_id(&secret.user_id)
            .await
            .map_err(|_| JellyfinStoreError)?;
        Ok(user.as_ref().map(Self::jellyfin_user))
    }

    async fn user_for_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<JellyfinUser>, JellyfinStoreError> {
        // v2 rule verbatim: stored lowercase username equals input stripped
        // and lowercased; display names never match.
        let want = username.trim().to_lowercase();
        let user = self.user_row(&want).await;
        let Some(user) = user else { return Ok(None) };
        if user.username.as_deref() != Some(want.as_str()) {
            return Ok(None);
        }
        let secret = self
            .users
            .app_passwords
            .get_active_by_sha256(&tokens::hash_token(password))
            .await
            .map_err(|_| JellyfinStoreError)?;
        match secret {
            Some(secret) if secret.user_id == user.id => Ok(Some(Self::jellyfin_user(&user))),
            _ => Ok(None),
        }
    }

    async fn note_use(&self, secret_plaintext: &str, client: Option<&str>) {
        self.touch(secret_plaintext, client).await;
    }
}

#[tokio::test]
async fn journey_c_app_password_compat_accepts_native_rejects() {
    let e2e = E2e::open("journey-c").await;
    let (admin, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let admin_id = admin["id"].as_str().expect("admin id").to_owned();
    let admin_token = admin_token.as_str().expect("admin token").to_owned();
    let admin_auth = bearer(admin_token.as_str());
    let admin_headers = [("authorization", admin_auth.as_str())];
    let compat = E2eCompatStore {
        users: e2e.users(),
        crypto: Arc::clone(&e2e.crypto),
    };

    // Create one app password; the secret shows exactly once.
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/me/app-passwords",
        &admin_headers,
        Some(json!({"name": "symfonium"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let secret = body["secret"].as_str().expect("one-time secret").to_owned();
    let row_id = body["id"].as_str().expect("row id").to_owned();
    assert_eq!(body["name"], json!("symfonium"));

    // Both compat contracts accept the secret (u+p and token schemes).
    let params = SubsonicParams::new(vec![("u", "e2e-owner"), ("p", secret.as_str())]);
    let principal = authenticate(&compat, &params)
        .await
        .expect("u+p accepts the app password");
    assert_eq!(principal.user_id, admin_id);
    let salt = "e2e-salt-1";
    let token = md5_hex(&format!("{secret}{salt}"));
    let params = SubsonicParams::new(vec![("u", "e2e-owner"), ("t", token.as_str()), ("s", salt)]);
    let principal = authenticate(&compat, &params)
        .await
        .expect("u+t+s accepts the app password");
    assert_eq!(principal.user_id, admin_id);
    let user = authenticate_by_name(&compat, "e2e-owner", secret.as_str(), None)
        .await
        .expect("jellyfin login accepts the app password");
    assert_eq!(user.id, admin_id);
    let user = resolve_token(&compat, Some(secret.as_str()))
        .await
        .expect("jellyfin token resolves");
    assert_eq!(user.id, admin_id);

    // The account password fails on compat exactly like an unknown
    // credential: code 40 on Subsonic, bare 401 on Jellyfin.
    let params = SubsonicParams::new(vec![("u", "e2e-owner"), ("p", "e2e-owner-password-1")]);
    assert_eq!(
        authenticate(&compat, &params).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS)),
        "account passwords never work on compat"
    );
    assert!(
        authenticate_by_name(&compat, "e2e-owner", "e2e-owner-password-1", None)
            .await
            .is_err(),
        "account passwords never work on jellyfin"
    );

    // And the app password fails on every native path.
    let (status, body, _) = login(e2e.router(), "e2e-owner", secret.as_str(), "bearer").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "no native login with it: {body}"
    );
    let foreign = bearer(secret.as_str());
    let (status, _, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/me",
        &[("authorization", foreign.as_str())],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "no native Bearer [REDACTED] it"
    );

    // Revoke: compat goes dead on both protocols.
    let (status, _, _) = call(
        e2e.router(),
        "DELETE",
        &format!("/api/v3/me/app-passwords/{row_id}"),
        &admin_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let params = SubsonicParams::new(vec![("u", "e2e-owner"), ("p", secret.as_str())]);
    assert_eq!(
        authenticate(&compat, &params).await,
        Err(SubsonicDenied::new(WRONG_CREDENTIALS)),
        "revoked secret fails subsonic"
    );
    assert!(
        authenticate_by_name(&compat, "e2e-owner", secret.as_str(), None)
            .await
            .is_err(),
        "revoked secret fails jellyfin"
    );
    assert!(
        resolve_token(&compat, Some(secret.as_str())).await.is_err(),
        "revoked secret resolves to nobody"
    );
    // Sanity: the params type is real — PARAM_MISSING exists for stage 9.
    let _ = PARAM_MISSING;
}

// ---------------------------------------------------------------------------
// Standing contract: auth on every endpoint
// ---------------------------------------------------------------------------

/// Expected anonymous posture for one mounted route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Posture {
    /// Allowlisted public (or outside the middleware): anything but 401.
    Public,
    /// Any signed-in user admitted; anonymous gets exactly 401.
    User,
    /// Admin only; anonymous 401, plain user 403.
    Admin,
}

/// One matrix row: method, OpenAPI path template, expected posture. The
/// template must match the doc byte for byte; `{id}` params are filled with
/// a dummy before the request goes out.
const MATRIX: &[(&str, &str, Posture)] = &[
    ("GET", "/health", Posture::Public),
    // Native auth: all four public.
    ("POST", "/api/v3/auth/login", Posture::Public),
    ("POST", "/api/v3/auth/logout", Posture::Public),
    ("POST", "/api/v3/auth/setup", Posture::Public),
    ("GET", "/api/v3/auth/setup/status", Posture::Public),
    // Federated journeys: public, own state-token auth.
    ("POST", "/api/v3/auth/oidc/authorize", Posture::Public),
    ("GET", "/api/v3/auth/oidc/callback", Posture::Public),
    ("POST", "/api/v3/auth/oidc/exchange", Posture::Public),
    ("POST", "/api/v3/auth/jellyfin/login", Posture::Public),
    ("POST", "/api/v3/auth/plex/start", Posture::Public),
    ("POST", "/api/v3/auth/plex/poll/login", Posture::Public),
    // Link/connect polls hand out account Bearer [REDACTED] session-gated (B1 fix).
    ("POST", "/api/v3/auth/plex/poll/link", Posture::User),
    ("POST", "/api/v3/auth/plex/poll/connect", Posture::User),
    (
        "POST",
        "/api/v3/auth/password-recovery/reset",
        Posture::Public,
    ),
    // Signed-in user routes.
    ("GET", "/api/v3/me", Posture::User),
    ("PATCH", "/api/v3/me", Posture::User),
    ("PUT", "/api/v3/me/username", Posture::User),
    ("PUT", "/api/v3/me/email", Posture::User),
    ("POST", "/api/v3/me/password", Posture::User),
    ("POST", "/api/v3/me/local-password", Posture::User),
    ("POST", "/api/v3/me/avatar", Posture::User),
    ("GET", "/api/v3/users/{id}/avatar", Posture::User),
    ("GET", "/api/v3/auth/sessions", Posture::User),
    ("DELETE", "/api/v3/auth/sessions/{id}", Posture::User),
    ("POST", "/api/v3/auth/device-sessions", Posture::User),
    ("POST", "/api/v3/auth/logout-all", Posture::User),
    ("GET", "/api/v3/me/app-passwords", Posture::User),
    ("POST", "/api/v3/me/app-passwords", Posture::User),
    ("DELETE", "/api/v3/me/app-passwords/{id}", Posture::User),
    ("GET", "/api/v3/me/connections/lastfm", Posture::User),
    ("PUT", "/api/v3/me/connections/lastfm", Posture::User),
    ("DELETE", "/api/v3/me/connections/lastfm", Posture::User),
    ("POST", "/api/v3/me/connections/lastfm/token", Posture::User),
    (
        "POST",
        "/api/v3/me/connections/lastfm/session",
        Posture::User,
    ),
    // Admin routes.
    ("GET", "/api/v3/admin/users", Posture::Admin),
    ("POST", "/api/v3/admin/users", Posture::Admin),
    ("GET", "/api/v3/admin/users/{id}", Posture::Admin),
    ("DELETE", "/api/v3/admin/users/{id}", Posture::Admin),
    ("PUT", "/api/v3/admin/users/{id}/role", Posture::Admin),
    (
        "DELETE",
        "/api/v3/admin/users/{id}/sessions",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/admin/users/{id}/recovery-code",
        Posture::Admin,
    ),
    ("GET", "/api/v3/admin/app-passwords", Posture::Admin),
    ("DELETE", "/api/v3/admin/app-passwords/{id}", Posture::Admin),
    ("GET", "/api/v3/admin/import/jellyfin", Posture::Admin),
    ("GET", "/api/v3/admin/import/plex", Posture::Admin),
    ("POST", "/api/v3/admin/import", Posture::Admin),
];

/// Fill `{param}` segments with a dummy id.
fn concretize(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest.find('}').expect("balanced template braces");
        let _ = &rest[start..=end];
        out.push_str("e2e-dummy-id");
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Every (method, template) pair the OpenAPI doc publishes.
fn documented_routes() -> Vec<(String, String)> {
    let doc = ApiDoc::openapi();
    let mut routes = Vec::new();
    for (path, item) in doc.paths.paths.iter() {
        for (method, op) in [
            ("GET", &item.get),
            ("POST", &item.post),
            ("PUT", &item.put),
            ("DELETE", &item.delete),
            ("PATCH", &item.patch),
        ] {
            if op.is_some() {
                routes.push((method.to_owned(), path.clone()));
            }
        }
    }
    routes
}

#[tokio::test]
async fn auth_on_every_endpoint() {
    // Coverage first, both directions: a documented route without a matrix
    // row fails by name, and a stale row fails too.
    for (method, path) in documented_routes() {
        assert!(
            MATRIX.iter().any(|(m, p, _)| *m == method && *p == path),
            "route {method} {path} is documented but has no auth-matrix row; add one"
        );
    }
    let documented = documented_routes();
    for (method, template, _) in MATRIX {
        assert!(
            documented.iter().any(|(m, p)| m == method && p == template),
            "matrix row {method} {template} matches no documented route; fix or drop it"
        );
    }

    let e2e = E2e::open("matrix").await;

    // Anonymous pass: protected rows 401 with the Bearer [REDACTED] public
    // rows answer anything but 401.
    for (method, template, posture) in MATRIX {
        let uri = concretize(template);
        let body = match *method {
            "POST" | "PUT" | "PATCH" => Some(json!({})),
            _ => None,
        };
        let (status, response_body, headers) = call(e2e.router(), method, &uri, &[], body).await;
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: anonymous pass tripped the limiter; rebuild routers per pass"
        );
        match posture {
            Posture::Public => assert_ne!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} is public but answered 401: {response_body}"
            ),
            Posture::User | Posture::Admin => {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} must 401 anonymously, got {status}: {response_body}"
                );
                assert_eq!(
                    error_code(&response_body),
                    "UNAUTHORIZED",
                    "{method} {uri}: envelope code"
                );
                assert_eq!(
                    headers
                        .get("www-authenticate")
                        .and_then(|v| v.to_str().ok()),
                    Some("Bearer"),
                    "{method} {uri}: Bearer [REDACTED]"
                );
            }
        }
    }

    // Allowlisted but not yet mounted: passes the gate (404), never 401.
    // If this 401s, the allowlist regressed; if it 200s, someone mounted it
    // and owes it a matrix row plus OpenAPI registration.
    let (status, body, _) = call(e2e.router(), "GET", "/api/v3/auth/providers", &[], None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "unmounted allowlisted path must pass the gate: {body}"
    );

    // Seed one admin and one user for the credentialed passes.
    let (_, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let admin_token = admin_token.as_str().expect("admin token").to_owned();
    let mut admin_auth = bearer(admin_token.as_str());
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/admin/users",
        &[("authorization", admin_auth.as_str())],
        Some(json!({"username": "molly", "password": "molly-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let molly_id = body["id"].as_str().expect("molly id").to_owned();
    let (status, body, _) = login(e2e.router(), "molly", "molly-password-1234", "bearer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let user_token = body["token"].as_str().expect("user token").to_owned();
    let mut user_auth = bearer(user_token.as_str());

    // User pass: admin rows 403, user rows admit (anything but 401/403).
    for (method, template, posture) in MATRIX {
        if !matches!(posture, Posture::User | Posture::Admin) {
            continue;
        }
        // Avatar reads are self-or-admin: the user pass reads the user's
        // own avatar (404, no bytes yet), which is the admitted path. The
        // foreign-id 403 is pinned in the users-slice briefs.
        let uri = if *template == "/api/v3/users/{id}/avatar" {
            format!("/api/v3/users/{molly_id}/avatar")
        } else {
            concretize(template)
        };
        let body = match *method {
            "POST" | "PUT" | "PATCH" => Some(json!({})),
            _ => None,
        };
        let (status, response_body, _) = call(
            e2e.router(),
            method,
            &uri,
            &[("authorization", user_auth.as_str())],
            body,
        )
        .await;
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: user pass tripped the limiter"
        );
        match posture {
            Posture::Admin => assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "user {method} {uri} must 403, got {status}: {response_body}"
            ),
            Posture::User => assert!(
                status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
                "user {method} {uri} must be admitted, got {status}: {response_body}"
            ),
            Posture::Public => unreachable!("filtered above"),
        }
        // Logout-all revokes the pass's own token by design; re-login so
        // the rows after it still run credentialed.
        if *template == "/api/v3/auth/logout-all" {
            let (status, body, _) =
                login(e2e.router(), "molly", "molly-password-1234", "bearer").await;
            assert_eq!(status, StatusCode::OK, "user re-login: {body}");
            user_auth = bearer(body["token"].as_str().expect("fresh user token"));
        }
    }

    // Admin pass: admitted everywhere (v2 spirit: auth passed means the body
    // ran, so 4xx/5xx from dummy input still count).
    for (method, template, posture) in MATRIX {
        if !matches!(posture, Posture::User | Posture::Admin) {
            continue;
        }
        let uri = concretize(template);
        let body = match *method {
            "POST" | "PUT" | "PATCH" => Some(json!({})),
            _ => None,
        };
        let (status, response_body, _) = call(
            e2e.router(),
            method,
            &uri,
            &[("authorization", admin_auth.as_str())],
            body,
        )
        .await;
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: admin pass tripped the limiter"
        );
        assert!(
            status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
            "admin {method} {uri} must be admitted, got {status}: {response_body}"
        );
        if *template == "/api/v3/auth/logout-all" {
            let (status, body, _) =
                login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
            assert_eq!(status, StatusCode::OK, "admin re-login: {body}");
            admin_auth = bearer(body["token"].as_str().expect("fresh admin token"));
        }
    }
}

// ---------------------------------------------------------------------------
// Setup edges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn setup_edges() {
    let e2e = E2e::open("setup-edges").await;

    // Empty database: setup required.
    let (status, body, _) = call(e2e.router(), "GET", "/api/v3/auth/setup/status", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": true}));

    // Login before setup: no users, so the uniform invalid-credentials 401
    // (v2 behavior; the dummy-hash verify runs behind it).
    let (status, body, _) = login(e2e.router(), "nobody", "no-such-password-1", "cookie").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
    assert_eq!(
        body["error"]["message"],
        json!("Invalid username or password")
    );

    // Setup flips the probe...
    let (status, _, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/setup",
        &[],
        Some(json!({"username": "root", "password": "root-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body, _) = call(e2e.router(), "GET", "/api/v3/auth/setup/status", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"setup_required": false}));

    // ...and a second setup is refused with the v2 conflict.
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/setup",
        &[],
        Some(json!({"username": "second", "password": "second-password-12"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "CONFLICT");
    assert_eq!(
        body["error"]["message"],
        json!("Setup has already been completed")
    );
}

// ---------------------------------------------------------------------------
// Login p95 probe against the login budget (BUDGETS.md)
// ---------------------------------------------------------------------------

/// Nearest-rank percentile over ascending samples.
fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    let rank = ((pct / 100.0 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

#[tokio::test]
async fn login_p95_within_login_budget() {
    const N: usize = 50;
    const BUDGET: Duration = Duration::from_millis(600);

    let e2e = E2e::open("login-p95").await;
    setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;

    // One warmup to prove the shape works before measuring.
    let (status, _, _) = login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    assert_eq!(status, StatusCode::OK);

    // Sequential logins against the one database. The router is rebuilt
    // every few logins for fresh rate buckets; the probe measures login
    // latency, not the limiter (pinned elsewhere), and a 429 still retries
    // honestly rather than recording a polluted sample.
    let mut samples = Vec::with_capacity(N);
    while samples.len() < N {
        let app = e2e.router();
        for _ in 0..4 {
            if samples.len() >= N {
                break;
            }
            let started = Instant::now();
            let (status, body, headers) =
                login(app.clone(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
            let elapsed = started.elapsed();
            if status == StatusCode::TOO_MANY_REQUESTS {
                let wait: u64 = headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1)
                    .clamp(1, 5);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            assert_eq!(status, StatusCode::OK, "probe login must succeed: {body}");
            samples.push(elapsed);
        }
    }
    samples.sort_unstable();

    let p50 = percentile(&samples, 50.0);
    let p95 = percentile(&samples, 95.0);
    let max = samples[samples.len() - 1];
    assert!(
        p95 <= BUDGET,
        "FINDING: login p95 {p95:?} exceeds the 600ms login budget \
         (tools/perf-harness/BUDGETS.md) over {N} sequential logins \
         (p50 {p50:?}, max {max:?}); Argon2id work-factor verify cost counts \
         and dominates by design - slowness here is the security feature"
    );
}
