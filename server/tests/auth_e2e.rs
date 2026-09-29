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
//!   fails by name. Curator rows read as admin rows for plain users; wrapped
//!   rows take only the shared secret (no Bearer challenge on rejection) and
//!   sessions never satisfy them.
//! - `setup_edges`: setup-status flip, once-only setup, login before setup.
//! - `trusted_tier_*`: trusted promotion admits curator pin writes (404 on
//!   the empty catalog) while admin approvals still 403.
//! - `playlist_lifecycle_*`: stateful playlist journey over one router
//!   clone: create → add tracks → read back → delete → 404.
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
use droppedneedle::runtime_config::{
    ConfigStore, Crypto, Secret, secret_sections::WrappedSettings,
};
use droppedneedle::{AppConfig, AppState, create_app, reads::ReadsSetup};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use utoipa::OpenApi as _;

/// Fixed host for every request; cookie mutations also send this as Origin.
const HOST: &str = "e2e.test";
const ORIGIN: &str = "http://e2e.test";
/// Wrapped shared secret saved into every scratch config.
const TEST_WRAPPED_KEY: &str = "e2e-wrapped-key-1";

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
    db_path: std::path::PathBuf,
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
        store
            .save_secret(WrappedSettings {
                api_key: Secret::new(TEST_WRAPPED_KEY),
            })
            .expect("wrapped key saves");

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
        let db_path = dir.join("app.db");
        Self {
            runtime,
            bundle,
            store,
            crypto,
            http,
            ids,
            clock,
            db_path,
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
        let wrapped_api_key = self
            .store
            .get_raw::<WrappedSettings>()
            .expect("wrapped settings read")
            .api_key
            .expose()
            .to_owned();
        let reads = ReadsSetup::build(
            self.runtime.pool(),
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            wrapped_api_key,
            None,
        );
        let connect_apps: droppedneedle::runtime_config::sections::ConnectApps =
            self.store.get().unwrap_or_default();
        let mut app_config = AppConfig::new(DEFAULT_PORT);
        app_config.root_app_dir = self
            .db_path
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let (stage6, _worker) = droppedneedle::stage6::Stage6Setup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            connect_apps,
        )
        .expect("stage6 bundle builds");
        let mut reads = reads;
        let acquire = droppedneedle::acquire::AcquireSetup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.store),
            &mut reads.collections,
        )
        .expect("acquire bundle builds");
        let state = AppState::new(
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            self.http.clone(),
            app_config,
            auth,
            reads,
            Arc::new(droppedneedle::providers::Providers::with_memory_cache()),
            stage6,
            acquire,
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

/// One request with a lenient body: JSON when it parses, null otherwise.
/// Every slice renders failures in the shared envelope, so in practice this
/// always decodes; the leniency only keeps failure messages readable. The
/// posture passes assert status only.
async fn call_lenient(
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
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, headers)
}

/// One request returning raw bytes. Covers serve SVG/PNG rather than
/// JSON, so the posture passes read them without the JSON decode.
async fn call_raw(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads")
        .to_vec();
    (status, headers, bytes)
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
    /// Curator (admin or trusted) only; anonymous 401, plain user 403.
    Curator,
    /// Wrapped shared-secret only: anonymous 401 with no Bearer [REDACTED]
    /// sessions never satisfy these, the key admits (keyed pass below).
    WrappedKey,
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
    // Stage-4 library reads.
    ("GET", "/api/v3/library/albums", Posture::User),
    ("GET", "/api/v3/library/albums/{id}", Posture::User),
    ("GET", "/api/v3/library/albums/{id}/tracks", Posture::User),
    ("GET", "/api/v3/library/albums/{id}/copies", Posture::User),
    ("GET", "/api/v3/library/artists", Posture::User),
    ("GET", "/api/v3/library/artists/{id}", Posture::User),
    ("GET", "/api/v3/library/artists/{id}/albums", Posture::User),
    (
        "GET",
        "/api/v3/library/artists/{id}/appearances",
        Posture::User,
    ),
    ("GET", "/api/v3/library/tracks", Posture::User),
    ("GET", "/api/v3/library/tracks/{id}", Posture::User),
    ("GET", "/api/v3/library/tracks/{id}/lyrics", Posture::User),
    ("GET", "/api/v3/library/stats", Posture::User),
    ("GET", "/api/v3/library/recently-added", Posture::User),
    ("GET", "/api/v3/library/genres", Posture::User),
    ("GET", "/api/v3/library/genres/{name}/tracks", Posture::User),
    ("GET", "/api/v3/local-library/albums", Posture::User),
    ("GET", "/api/v3/local-library/search", Posture::User),
    ("GET", "/api/v3/local-library/recent", Posture::User),
    ("GET", "/api/v3/local-library/decades", Posture::User),
    ("GET", "/api/v3/local-library/suggestions", Posture::User),
    // Stage-4 unified search.
    ("GET", "/api/v3/search", Posture::User),
    ("GET", "/api/v3/search/{bucket}", Posture::User),
    ("GET", "/api/v3/search/suggest", Posture::User),
    ("POST", "/api/v3/search/enrich/batch", Posture::User),
    // Stage-4 discover + queue + radio + batches.
    ("GET", "/api/v3/discover", Posture::User),
    ("POST", "/api/v3/discover/refresh", Posture::User),
    ("POST", "/api/v3/discover/activity", Posture::User),
    ("POST", "/api/v3/discover/radio", Posture::User),
    ("POST", "/api/v3/discover/radio/plan", Posture::User),
    (
        "POST",
        "/api/v3/discover/playlist-suggestions",
        Posture::User,
    ),
    ("GET", "/api/v3/discover/queue", Posture::User),
    ("GET", "/api/v3/discover/queue/status", Posture::User),
    ("POST", "/api/v3/discover/queue/generate", Posture::User),
    (
        "GET",
        "/api/v3/discover/queue/enrich/{release_group_mbid}",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/discover/queue/preview/{release_group_mbid}",
        Posture::User,
    ),
    ("POST", "/api/v3/discover/queue/ignore", Posture::User),
    ("GET", "/api/v3/discover/queue/ignored", Posture::User),
    ("POST", "/api/v3/discover/queue/validate", Posture::User),
    (
        "GET",
        "/api/v3/discover/queue/youtube-search",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/discover/queue/youtube-track-search",
        Posture::User,
    ),
    ("GET", "/api/v3/discover/queue/youtube-quota", Posture::User),
    (
        "POST",
        "/api/v3/discover/queue/youtube-cache-check",
        Posture::User,
    ),
    ("GET", "/api/v3/discover/track-preview", Posture::User),
    ("GET", "/api/v3/discover/album-preview", Posture::User),
    ("POST", "/api/v3/discover/batches", Posture::User),
    ("GET", "/api/v3/discover/batches", Posture::User),
    ("GET", "/api/v3/discover/batches/{batch_id}", Posture::User),
    (
        "DELETE",
        "/api/v3/discover/batches/{batch_id}",
        Posture::User,
    ),
    // Stage-4 home + now playing.
    ("GET", "/api/v3/home", Posture::User),
    ("GET", "/api/v3/home/integration-status", Posture::User),
    ("GET", "/api/v3/home/genre/{genre_name}", Posture::User),
    ("GET", "/api/v3/home/trending/artists", Posture::User),
    ("GET", "/api/v3/home/popular/albums", Posture::User),
    ("GET", "/api/v3/home/your-top/albums", Posture::User),
    ("GET", "/api/v3/now-playing", Posture::User),
    // Stage-4 collections: playlists.
    ("GET", "/api/v3/playlists", Posture::User),
    ("POST", "/api/v3/playlists", Posture::User),
    ("GET", "/api/v3/playlists/{playlist_id}", Posture::User),
    ("PUT", "/api/v3/playlists/{playlist_id}", Posture::User),
    ("DELETE", "/api/v3/playlists/{playlist_id}", Posture::User),
    (
        "PATCH",
        "/api/v3/playlists/{playlist_id}/visibility",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/playlists/{playlist_id}/tracks",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/playlists/{playlist_id}/tracks/remove",
        Posture::User,
    ),
    (
        "DELETE",
        "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
        Posture::User,
    ),
    (
        "PATCH",
        "/api/v3/playlists/{playlist_id}/tracks/reorder",
        Posture::User,
    ),
    (
        "PATCH",
        "/api/v3/playlists/{playlist_id}/tracks/{track_id}",
        Posture::User,
    ),
    ("POST", "/api/v3/playlists/check-tracks", Posture::User),
    (
        "POST",
        "/api/v3/playlists/{playlist_id}/resolve-sources",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/playlists/{playlist_id}/cover",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/playlists/{playlist_id}/cover",
        Posture::User,
    ),
    (
        "DELETE",
        "/api/v3/playlists/{playlist_id}/cover",
        Posture::User,
    ),
    // Stage-4 collections: favorites, follows, approvals, pins.
    ("GET", "/api/v3/favorites", Posture::User),
    ("PUT", "/api/v3/favorites/{kind}/{item_id}", Posture::User),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/follow-status",
        Posture::User,
    ),
    ("PUT", "/api/v3/artists/{artist_mbid}/follow", Posture::User),
    (
        "PUT",
        "/api/v3/artists/{artist_mbid}/auto-download",
        Posture::User,
    ),
    ("GET", "/api/v3/following/artists", Posture::User),
    ("GET", "/api/v3/following/new-releases", Posture::User),
    (
        "GET",
        "/api/v3/following/new-releases/recent",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/following/new-releases/unseen-count",
        Posture::User,
    ),
    ("POST", "/api/v3/following/new-releases/seen", Posture::User),
    (
        "GET",
        "/api/v3/requests/auto-download-approvals",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/requests/auto-download-approval-batches",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/library/albums/{album_id}/edition-pin",
        Posture::User,
    ),
    (
        "PUT",
        "/api/v3/library/albums/{album_id}/edition-pin",
        Posture::Curator,
    ),
    (
        "DELETE",
        "/api/v3/library/albums/{album_id}/edition-pin",
        Posture::Curator,
    ),
    // Stage-4 platform: covers, version, wrapped.
    (
        "GET",
        "/api/v3/covers/release-group/{release_group_id}",
        Posture::User,
    ),
    ("GET", "/api/v3/covers/release/{release_id}", Posture::User),
    ("GET", "/api/v3/covers/artist/{artist_id}", Posture::User),
    ("GET", "/api/v3/version", Posture::User),
    ("GET", "/api/v3/version/check-update", Posture::User),
    ("GET", "/api/v3/version/releases", Posture::User),
    ("GET", "/api/v3/wrapped/users", Posture::WrappedKey),
    ("GET", "/api/v3/wrapped/user/{user_id}", Posture::WrappedKey),
    ("GET", "/api/v3/wrapped/server", Posture::WrappedKey),
    // Stage-6 remote sources.
    ("GET", "/api/v3/remotes/{source}/hub", Posture::User),
    ("GET", "/api/v3/remotes/{source}/stats", Posture::User),
    ("GET", "/api/v3/remotes/{source}/albums", Posture::User),
    ("GET", "/api/v3/remotes/{source}/albums/{id}", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/albums/{id}/tracks",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/artists", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/artists/index",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/remotes/{source}/artists/{id}",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/tracks", Posture::User),
    ("GET", "/api/v3/remotes/{source}/search", Posture::User),
    ("GET", "/api/v3/remotes/{source}/recent", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/recently-added",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/favorites", Posture::User),
    ("GET", "/api/v3/remotes/{source}/genres", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/genres/songs",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/playlists", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/playlists/{id}",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/remotes/{source}/playlists/{id}/import",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/remotes/{source}/info/artists/{id}",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/remotes/{source}/info/albums/{id}",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/lyrics/{id}", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/top/{artist}",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/remotes/{source}/similar/{id}",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/mix/{id}", Posture::User),
    ("GET", "/api/v3/remotes/{source}/sessions", Posture::User),
    ("GET", "/api/v3/remotes/{source}/history", Posture::User),
    ("GET", "/api/v3/remotes/{source}/images/{id}", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/covers/playlists/{id}",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/match", Posture::User),
    ("GET", "/api/v3/remotes/{source}/connection", Posture::User),
    ("PUT", "/api/v3/remotes/{source}/connection", Posture::User),
    (
        "DELETE",
        "/api/v3/remotes/{source}/connection",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/navidrome/folders", Posture::User),
    ("PUT", "/api/v3/remotes/navidrome/folders", Posture::User),
    // Stage-6 stream gateway.
    ("GET", "/api/v3/stream/{source}/{key}", Posture::User),
    ("HEAD", "/api/v3/stream/{source}/{key}", Posture::User),
    // Stage-6 playback reporting (GET /now-playing keeps its stage-4 row;
    // stage 6 serves it from the live registry now).
    ("POST", "/api/v3/playback/start", Posture::User),
    ("POST", "/api/v3/playback/progress", Posture::User),
    ("POST", "/api/v3/playback/stop", Posture::User),
    ("POST", "/api/v3/scrobble/submit", Posture::User),
    ("POST", "/api/v3/scrobble/now-playing", Posture::User),
    ("POST", "/api/v3/now-playing", Posture::User),
    ("DELETE", "/api/v3/now-playing", Posture::User),
    // Stage-7 acquisition requests.
    ("POST", "/api/v3/requests/albums", Posture::User),
    ("POST", "/api/v3/requests/tracks", Posture::User),
    ("POST", "/api/v3/requests/batches", Posture::User),
    ("POST", "/api/v3/requests/batches/cancel", Posture::User),
    ("GET", "/api/v3/requests/active", Posture::User),
    ("GET", "/api/v3/requests/active/count", Posture::User),
    (
        "DELETE",
        "/api/v3/requests/active/{musicbrainz_id}",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/requests/retry/{musicbrainz_id}",
        Posture::User,
    ),
    ("GET", "/api/v3/requests/history", Posture::User),
    (
        "DELETE",
        "/api/v3/requests/history/{musicbrainz_id}",
        Posture::User,
    ),
    ("POST", "/api/v3/requests/sync", Posture::Admin),
    ("GET", "/api/v3/requests/wanted", Posture::User),
    (
        "POST",
        "/api/v3/requests/wanted/{musicbrainz_id}/stop",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/requests/wanted/{musicbrainz_id}/resume",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/requests/wanted/{musicbrainz_id}/seen",
        Posture::User,
    ),
    ("GET", "/api/v3/requests/approvals", Posture::Admin),
    ("GET", "/api/v3/requests/approvals/count", Posture::Admin),
    (
        "POST",
        "/api/v3/requests/approvals/{musicbrainz_id}/approve",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/approvals/{musicbrainz_id}/reject",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/revoke",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/auto-download-approval-batches/{batch_id}/approve",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/auto-download-approval-batches/{batch_id}/reject",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/requests/personal-mix-approvals",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/personal-mix-approvals/{user_id}/approve",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/personal-mix-approvals/{user_id}/reject",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/personal-mix-approvals/{user_id}/revoke",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/requests/personal-mix/refresh",
        Posture::User,
    ),
    (
        "POST",
        "/api/v3/albums/{album_id}/edition/acquire",
        Posture::Curator,
    ),
    // Stage-7 acquisition imports.
    (
        "GET",
        "/api/v3/acquire/lidarr-import/config",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/acquire/lidarr-import/config",
        Posture::Admin,
    ),
    ("POST", "/api/v3/acquire/lidarr-import/test", Posture::Admin),
    (
        "GET",
        "/api/v3/acquire/lidarr-import/artists",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/acquire/lidarr-import/import",
        Posture::Admin,
    ),
    ("GET", "/api/v3/acquire/spotify/settings", Posture::Admin),
    ("PUT", "/api/v3/acquire/spotify/settings", Posture::Admin),
    (
        "GET",
        "/api/v3/acquire/spotify/redirect-uri",
        Posture::Admin,
    ),
    ("GET", "/api/v3/acquire/spotify/auth/url", Posture::User),
    (
        "GET",
        "/api/v3/acquire/spotify/auth/callback",
        Posture::Public,
    ),
    ("GET", "/api/v3/acquire/spotify/playlists", Posture::User),
    (
        "POST",
        "/api/v3/acquire/spotify/playlists/{id}/import",
        Posture::User,
    ),
    ("GET", "/api/v3/acquire/spotify/jobs/{id}", Posture::User),
    ("GET", "/api/v3/acquire/health", Posture::User),
    ("GET", "/api/v3/acquire/slskd/status", Posture::User),
    ("GET", "/api/v3/acquire/sabnzbd/status", Posture::Admin),
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
            ("HEAD", &item.head),
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
            Posture::WrappedKey => {
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
                assert!(
                    headers.get("www-authenticate").is_none(),
                    "{method} {uri}: a shared-secret rejection must not send a Bearer challenge"
                );
            }
            Posture::User | Posture::Admin | Posture::Curator => {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} must 401 anonymously, got {status}: {response_body}"
                );
                // HEAD answers carry no body (the router strips it), so only
                // methods with a body pin the envelope code here.
                if *method != "HEAD" {
                    assert_eq!(
                        error_code(&response_body),
                        "UNAUTHORIZED",
                        "{method} {uri}: envelope code"
                    );
                }
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
        if !matches!(
            posture,
            Posture::User | Posture::Admin | Posture::Curator | Posture::WrappedKey
        ) {
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
        let mut body = match *method {
            "POST" | "PUT" | "PATCH" => Some(json!({})),
            _ => None,
        };
        // Pin writes gate the role inside the service, after the body
        // parses: a shaped body reaches the curator check (403 for plain
        // users) instead of failing body validation first.
        if *template == "/api/v3/library/albums/{album_id}/edition-pin" && *method == "PUT" {
            body = Some(json!({"release_mbid": "e2e-dummy-id"}));
        }
        // Covers answer SVG/PNG bytes, so the user pass reads them raw;
        // every other row still decodes JSON for the failure message.
        let (status, response_body) = if template.starts_with("/api/v3/covers/") {
            let (status, _, _) = call_raw(
                e2e.router(),
                method,
                &uri,
                &[("authorization", user_auth.as_str())],
            )
            .await;
            (status, Value::Null)
        } else {
            let (status, response_body, _) = call_lenient(
                e2e.router(),
                method,
                &uri,
                &[("authorization", user_auth.as_str())],
                body,
            )
            .await;
            (status, response_body)
        };
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: user pass tripped the limiter"
        );
        match posture {
            Posture::Admin | Posture::Curator => assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "user {method} {uri} must 403, got {status}: {response_body}"
            ),
            Posture::WrappedKey => assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "user session must not satisfy {method} {uri}, got {status}: {response_body}"
            ),
            Posture::User => {
                assert!(
                    status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
                    "user {method} {uri} must be admitted, got {status}: {response_body}"
                );
                assert!(
                    !status.is_server_error(),
                    "user {method} {uri} must not 5xx on dummy input, got {status}: {response_body}"
                );
            }
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
    // ran, so 4xx from dummy input still counts; 5xx never does). Wrapped
    // rows are the exception: no session satisfies the shared-secret gate.
    for (method, template, posture) in MATRIX {
        if !matches!(
            posture,
            Posture::User | Posture::Admin | Posture::Curator | Posture::WrappedKey
        ) {
            continue;
        }
        let uri = concretize(template);
        let mut body = match *method {
            "POST" | "PUT" | "PATCH" => Some(json!({})),
            _ => None,
        };
        // Shaped pin body, as in the user pass: the admin reaches the
        // service (404 on the empty catalog) instead of failing validation.
        if *template == "/api/v3/library/albums/{album_id}/edition-pin" && *method == "PUT" {
            body = Some(json!({"release_mbid": "e2e-dummy-id"}));
        }
        let (status, response_body) = if template.starts_with("/api/v3/covers/") {
            let (status, _, _) = call_raw(
                e2e.router(),
                method,
                &uri,
                &[("authorization", admin_auth.as_str())],
            )
            .await;
            (status, Value::Null)
        } else {
            let (status, response_body, _) = call_lenient(
                e2e.router(),
                method,
                &uri,
                &[("authorization", admin_auth.as_str())],
                body,
            )
            .await;
            (status, response_body)
        };
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: admin pass tripped the limiter"
        );
        // Honest-503 rows: the import directories are disabled until the
        // live clients land (stage-3 posture, documented on the handlers).
        // The 503 is the contract here, not a failure, so it pins exactly.
        if *method == "GET"
            && matches!(
                *template,
                "/api/v3/admin/import/jellyfin" | "/api/v3/admin/import/plex"
            )
        {
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "admin {method} {uri} must 503 while disabled, got {status}: {response_body}"
            );
            assert_eq!(
                error_code(&response_body),
                "INTERNAL_ERROR",
                "{method} {uri}: envelope code"
            );
        } else if matches!(posture, Posture::WrappedKey) {
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "admin session must not satisfy {method} {uri}, got {status}: {response_body}"
            );
        } else {
            assert!(
                status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
                "admin {method} {uri} must be admitted, got {status}: {response_body}"
            );
            assert!(
                !status.is_server_error(),
                "admin {method} {uri} must not 5xx on dummy input, got {status}: {response_body}"
            );
        }
        if *template == "/api/v3/auth/logout-all" {
            let (status, body, _) =
                login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
            assert_eq!(status, StatusCode::OK, "admin re-login: {body}");
            admin_auth = bearer(body["token"].as_str().expect("fresh admin token"));
        }
    }

    // Keyed pass: the wrapped secret admits where sessions cannot.
    for (method, template, posture) in MATRIX {
        if !matches!(posture, Posture::WrappedKey) {
            continue;
        }
        let uri = concretize(template);
        let (status, response_body, _) = call(
            e2e.router(),
            method,
            &uri,
            &[("x-wrapped-api-key", TEST_WRAPPED_KEY)],
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "keyed {method} {uri} must pass, got {status}: {response_body}"
        );
    }
}

/// Stage-4 wiring: one live route per reads namespace answers 200 behind
/// the real gate, and the collections principal translation resolves the
/// admin role (the approvals read would 403 otherwise).
#[tokio::test]
async fn reads_routes_are_mounted() {
    let e2e = E2e::open("reads-mounted").await;
    let (_, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let auth = bearer(admin_token.as_str().expect("admin token"));

    // Empty scratch catalog: library and search answer shaped empties.
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/library/albums",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 0);

    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/search?q=ab",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["artists"].as_array().expect("artists").len(), 0);

    // Discover and home run the stage-4 fakes; now playing answers from
    // the stage-6 live registry (empty here, still 200).
    for uri in ["/api/v3/discover", "/api/v3/home", "/api/v3/now-playing"] {
        let (status, body, _) = call(
            e2e.router(),
            "GET",
            uri,
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }

    // Collections: empty stores plus the translated admin role.
    for uri in [
        "/api/v3/playlists",
        "/api/v3/favorites",
        "/api/v3/requests/auto-download-approvals",
    ] {
        let (status, body, _) = call(
            e2e.router(),
            "GET",
            uri,
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }

    // Covers answer the placeholder SVG; version reports the build.
    let (status, headers, bytes) = call_raw(
        e2e.router(),
        "GET",
        "/api/v3/covers/artist/e2e-dummy-id",
        &[("authorization", auth.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("image/svg+xml")
    );
    assert!(bytes.starts_with(b"<svg"));

    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/version",
        &[("authorization", auth.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["version"].as_str().is_some_and(|v| !v.is_empty()),
        "{body}"
    );

    // Wrapped: the key admits, a wrong key 401s without a challenge.
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/wrapped/users",
        &[("x-wrapped-api-key", TEST_WRAPPED_KEY)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["users"].as_array().expect("users").len(), 0);

    let (status, body, headers) = call(
        e2e.router(),
        "GET",
        "/api/v3/wrapped/users",
        &[("x-wrapped-api-key", "wrong-key")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(headers.get("www-authenticate").is_none());
}

// ---------------------------------------------------------------------------
// Trusted tier: curator writes admit, admin reads still forbid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trusted_tier_pins_admitted_approvals_forbidden() {
    let e2e = E2e::open("trusted-tier").await;
    let (_, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let admin_auth = bearer(admin_token.as_str().expect("admin token"));
    let admin_headers = [("authorization", admin_auth.as_str())];

    // Admin creates a plain user, then promotes to trusted (journey_b
    // mechanism: the role change takes effect on the next request).
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/admin/users",
        &admin_headers,
        Some(json!({"username": "tris", "password": "tris-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let tris_id = body["id"].as_str().expect("user id").to_owned();
    let (status, body, _) = call(
        e2e.router(),
        "PUT",
        &format!("/api/v3/admin/users/{tris_id}/role"),
        &admin_headers,
        Some(json!({"role": "trusted"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], json!("trusted"));

    let (status, body, _) = login(e2e.router(), "tris", "tris-password-1234", "bearer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tris_auth = bearer(body["token"].as_str().expect("user token"));
    let tris_headers = [("authorization", tris_auth.as_str())];

    // Curator pin write reaches the service: 404 on the empty edition
    // catalog proves the role gate admitted (a plain user 403s here).
    let (status, body, _) = call(
        e2e.router(),
        "PUT",
        "/api/v3/library/albums/e2e-dummy-id/edition-pin",
        &tris_headers,
        Some(json!({"release_mbid": "e2e-dummy-id"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "NOT_FOUND");

    // Admin-only approvals still forbid trusted callers.
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/requests/auto-download-approvals",
        &tris_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "FORBIDDEN");
}

// ---------------------------------------------------------------------------
// Stateful playlist journey over one router clone
// ---------------------------------------------------------------------------

#[tokio::test]
async fn playlist_lifecycle_persists_across_requests() {
    let e2e = E2e::open("playlist-journey").await;
    let (_, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let auth = bearer(admin_token.as_str().expect("admin token"));
    let headers = [("authorization", auth.as_str())];
    // One router for every step: the collections stores live in the router
    // state, so each clone shares them and writes read back.
    let app = e2e.router();

    let (status, body, _) = call(
        app.clone(),
        "POST",
        "/api/v3/playlists",
        &headers,
        Some(json!({"name": "Journey mix"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("playlist id").to_owned();

    let (status, body, _) = call(
        app.clone(),
        "POST",
        &format!("/api/v3/playlists/{id}/tracks"),
        &headers,
        Some(json!({"tracks": [
            {"track_name": "Roads", "artist_name": "Portishead", "album_name": "Dummy"},
            {"track_name": "Teardrop", "artist_name": "Massive Attack", "album_name": "Mezzanine"},
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tracks"].as_array().expect("added tracks").len(), 2);

    let (status, body, _) = call(
        app.clone(),
        "GET",
        &format!("/api/v3/playlists/{id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], json!("Journey mix"));
    let tracks = body["tracks"].as_array().expect("persisted tracks");
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["track_name"], json!("Roads"));
    assert_eq!(tracks[1]["track_name"], json!("Teardrop"));

    let (status, body, _) = call(
        app.clone(),
        "DELETE",
        &format!("/api/v3/playlists/{id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body, _) = call(
        app.clone(),
        "GET",
        &format!("/api/v3/playlists/{id}"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(error_code(&body), "NOT_FOUND");
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
