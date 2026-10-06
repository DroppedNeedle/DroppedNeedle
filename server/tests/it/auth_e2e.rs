//! Real-app auth journeys plus the standing auth contract.
//!
//! Everything here runs against `create_app` with the production SQLite
//! bundle over scratch databases. HIBP screening is switched off through
//! its real config knob; no test touches an IdP, mail, or the network.
//!
//! - `journey_*`: setup and sessions, the admin user lifecycle, and app
//!   passwords across the compat contracts.
//! - `auth_on_every_endpoint`: every `/api/v3` route in the OpenAPI doc has
//!   a matrix row; protected routes 401 anonymously with a Bearer challenge,
//!   admin routes 403 for plain users, and the admin is admitted everywhere.
//! - Rate limits, setup races, and store-failure handling at the edge.
//! - `login_p95_*`: the login latency budget, run on request.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::auth::compat_auth::jellyfin::{authenticate_by_name, resolve_token};
use droppedneedle::auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle::auth::compat_auth::subsonic::{
    SubsonicDenied, SubsonicParams, WRONG_CREDENTIALS, authenticate, md5_hex,
};
use droppedneedle::auth::prod::ProdAuth;
use droppedneedle::auth::session::cookies::COOKIE_NAME;
use droppedneedle::auth::users::UsersDeps;
use droppedneedle::auth::users::stores::SystemClock;
use droppedneedle::auth::wiring::{AuthSetup, Upstreams};
use droppedneedle::config::DEFAULT_PORT;
use droppedneedle::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle::docs::ApiDoc;
use droppedneedle::http_client::HttpClientFactory;
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::providers::{InMemoryProviderCache, Providers};
use droppedneedle::reads::catalog::{
    Catalog,
    library::LocalCatalog,
    upstream::{CatalogSettings, Upstream},
};
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

/// One scratch deployment: migrated database, production adapters, config.
/// The federated suite reuses it with its upstreams pointed at mocks.
pub(crate) struct E2e {
    /// Held, never read: dropping it would close the pool out from under
    /// the adapters.
    #[allow(dead_code)]
    runtime: DbRuntime,
    bundle: ProdAuth,
    pub(crate) store: Arc<ConfigStore>,
    upstreams: Upstreams,
    crypto: Arc<Crypto>,
    http: HttpClientFactory,
    ids: Arc<UuidGenerator>,
    clock: Arc<SystemClock>,
    db_path: std::path::PathBuf,
    _scratch: crate::common::ScratchDir,
}

impl E2e {
    pub(crate) async fn open(tag: &str) -> Self {
        let scratch = crate::common::ScratchDir::new(&format!("auth-e2e-{tag}"));
        let dir = scratch.to_path_buf();
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
            // Nothing listens on the discard port: no test reaches the real
            // plex.tv or Last.fm unless it points these at its own mock.
            upstreams: Upstreams {
                plex_tv: "http://127.0.0.1:9".to_owned(),
                lastfm: "http://127.0.0.1:9/".to_owned(),
            },
            crypto,
            http,
            ids,
            clock,
            db_path,
            _scratch: scratch,
        }
    }

    /// Point the plex.tv and Last.fm clients at local mocks.
    pub(crate) fn with_upstreams(mut self, upstreams: Upstreams) -> Self {
        self.upstreams = upstreams;
        self
    }

    /// The scratch database pool, for assertions on stored rows.
    pub(crate) fn pool(&self) -> &sqlx::SqlitePool {
        self.runtime.pool()
    }

    fn auth_setup(&self) -> AuthSetup {
        AuthSetup::build_with_upstreams(
            self.bundle.clone(),
            Arc::clone(&self.store),
            Arc::clone(&self.crypto),
            &self.http,
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
            &self.upstreams,
        )
        .expect("prod auth bundle builds")
    }

    /// A fresh router over the same database. Each build carries fresh rate
    /// buckets, so multi-pass tests rebuild instead of tripping the limiter.
    pub(crate) fn router(&self) -> Router {
        let auth = self.auth_setup();
        let reads = ReadsSetup::build(
            self.runtime.pool(),
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            crate::common::reads_inputs(
                Arc::clone(&self.store),
                self.db_path.parent().expect("db dir"),
            ),
            None,
        )
        .with_collections(droppedneedle::reads::collections::db::CollectionsDb::new(
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
        ))
        .with_catalog(Catalog::new(
            Upstream::new(
                &self.http,
                Arc::new(Providers::new(Arc::new(InMemoryProviderCache::new()))),
                Arc::clone(&self.store) as Arc<dyn CatalogSettings>,
                auth.users.clone(),
            ),
            LocalCatalog::new(self.runtime.pool().clone()),
        ));
        let connect_apps: droppedneedle::runtime_config::sections::ConnectApps =
            self.store.get().unwrap_or_default();
        let mut app_config = AppConfig::new(DEFAULT_PORT);
        app_config.root_app_dir = self
            .db_path
            .parent()
            .map(|parent| parent.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let library = droppedneedle::library::wiring::LibrarySetup::for_tests(
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
        )
        .expect("library bundle builds");
        let (media, _worker) = droppedneedle::media::MediaSetup::build(
            &self.db_path,
            &app_config,
            auth.users.clone(),
            Arc::clone(&self.crypto),
            self.http.shared().clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            connect_apps.clone(),
            Some(library.root_source()),
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
            Arc::clone(&self.store),
            Arc::new(droppedneedle::remotes::adapter::PlaylistImportSink::new(
                reads.collections.clone(),
            )),
        )
        .expect("media bundle builds");
        let mut reads = reads;
        let acquire = droppedneedle::acquire::AcquireSetup::build(
            droppedneedle::acquire::db::AcquireDb::from_runtime(&self.runtime),
            &app_config,
            auth.users.clone(),
            &self.http,
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            Arc::clone(&self.store),
            &mut reads.collections,
        )
        .expect("acquire bundle builds");
        let compat = droppedneedle::compat::CompatSetup::build(
            droppedneedle::compat::setup::CompatDeps::over_reads(
                auth.users.clone(),
                Arc::clone(&self.crypto),
                media.playback.clone(),
                media.stream.engine.clone(),
                library.clone(),
                &reads,
                droppedneedle::compat::settings::LiveSettings::from_config(Arc::clone(&self.store)),
            ),
        );
        let providers = Arc::new(droppedneedle::providers::Providers::with_memory_cache());
        let backup_dir = self
            .db_path
            .parent()
            .map(|parent| parent.join("backups"))
            .unwrap_or_else(std::env::temp_dir);
        let jobs = droppedneedle::jobs::wiring::JobsSetup::for_tests(auth.users.clone());
        // Concerts over the scratch database; the sources point at the
        // discard port so no row can reach a real provider.
        let concerts = droppedneedle::concerts::ConcertsSetup::new(
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
            self.http.shared().clone(),
            Arc::clone(&self.store),
            droppedneedle::concerts::Endpoints {
                ticketmaster: "http://127.0.0.1:9".to_owned(),
                skiddle: "http://127.0.0.1:9".to_owned(),
                geocoding: "http://127.0.0.1:9".to_owned(),
            },
        );
        let admin = droppedneedle::admin::AdminSetup::new(
            auth.users.clone(),
            acquire.requests.quota.clone(),
            Arc::new(droppedneedle::providers::InMemoryProviderCache::new()),
            providers.clone(),
        )
        .with_db(droppedneedle::admin::AdminDb::new(
            self.runtime.pool().clone(),
            self.runtime.lane().clone(),
        ))
        .with_backups(droppedneedle::db::BackupService::new(
            &self.db_path,
            &backup_dir,
        ))
        .with_checkpoint(self.runtime.checkpoint().clone())
        .with_precache(jobs.precache_trigger());
        let plugins = droppedneedle::plugins::wiring::PluginsSetup::for_tests(
            auth.users.clone(),
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            jobs.registry().clone(),
        )
        .expect("test plugins bundle builds");
        let settings = droppedneedle::settings::wiring::SettingsSetup::for_tests(
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            auth.users.clone(),
        )
        .expect("test settings bundle builds")
        .with_impact_buckets(Arc::new(
            droppedneedle::settings::services::SqliteImpactBuckets {
                pool: self.runtime.pool().clone(),
            },
        ))
        .with_library_catalog(Arc::new(
            droppedneedle::settings::library_catalog::SqliteLibraryPolicyCatalog {
                pool: self.runtime.pool().clone(),
            },
        ));
        let state = AppState::new(
            Arc::clone(&self.ids) as Arc<dyn IdGenerator>,
            self.http.clone(),
            app_config,
            auth,
            reads,
            providers,
            media,
            acquire,
            library,
            compat,
            admin,
            settings,
            jobs,
            plugins,
            concerts,
        );
        create_app(state)
    }

    pub(crate) fn users(&self) -> UsersDeps {
        // Rebuild is cheap, but the stores are what matter: clone the deps
        // through one throwaway bundle so the compat adapter reads live rows.
        self.auth_setup().users
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// One request through the real app. Every call carries Host; cookie
/// mutations additionally need Origin (pass `with_origin`).
pub(crate) async fn call(
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
    // Bearer tokens for the admin leg: no Origin juggling, same real sessions.
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

#[tokio::test]
async fn journey_c_app_password_compat_accepts_native_rejects() {
    let e2e = E2e::open("journey-c").await;
    let (admin, admin_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let admin_id = admin["id"].as_str().expect("admin id").to_owned();
    let admin_token = admin_token.as_str().expect("admin token").to_owned();
    let admin_auth = bearer(admin_token.as_str());
    let admin_headers = [("authorization", admin_auth.as_str())];
    let compat = ProdCompatPasswords::new(e2e.users(), Arc::clone(&e2e.crypto));

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
        "no native Bearer use of it"
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
    /// Wrapped shared-secret only: anonymous 401 with no Bearer challenge;
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
    ("GET", "/api/v3/auth/providers", Posture::Public),
    // Federated journeys: public, own state-token auth.
    ("POST", "/api/v3/auth/oidc/authorize", Posture::Public),
    ("GET", "/api/v3/auth/oidc/callback", Posture::Public),
    ("POST", "/api/v3/auth/oidc/exchange", Posture::Public),
    ("POST", "/api/v3/auth/jellyfin/login", Posture::Public),
    ("POST", "/api/v3/auth/plex/start", Posture::Public),
    ("POST", "/api/v3/auth/plex/poll/login", Posture::Public),
    // Link/connect polls hand out account Bearer tokens, so they stay session-gated (B1 fix).
    ("POST", "/api/v3/auth/plex/start/link", Posture::User),
    ("POST", "/api/v3/auth/plex/start/connect", Posture::Admin),
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
    // Stage-10 settings surface: every /settings route is admin-only;
    // section prefs are per-user.
    ("GET", "/api/v3/me/section-prefs", Posture::User),
    ("PUT", "/api/v3/me/section-prefs", Posture::User),
    ("GET", "/api/v3/settings/advanced", Posture::Admin),
    ("PUT", "/api/v3/settings/advanced", Posture::Admin),
    ("GET", "/api/v3/settings/cache-ttls", Posture::Admin),
    ("GET", "/api/v3/settings/connect-apps", Posture::Admin),
    ("PUT", "/api/v3/settings/connect-apps", Posture::Admin),
    (
        "GET",
        "/api/v3/settings/download-client/config",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/download-client/config",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/download-client/test",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/download-clients/policy",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/download-clients/policy",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/download-clients/policy-summary",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/download-clients/policy/impact",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/download-clients/sabnzbd",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/download-clients/sabnzbd",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/download-clients/sabnzbd/test",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/download-clients/source-priority",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/download-clients/source-priority",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/download-clients/wanted",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/download-clients/wanted",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/events", Posture::Admin),
    ("PUT", "/api/v3/settings/events", Posture::Admin),
    (
        "POST",
        "/api/v3/settings/events/test-skiddle",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/events/test-ticketmaster",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/free-music", Posture::Admin),
    ("PUT", "/api/v3/settings/free-music", Posture::Admin),
    ("GET", "/api/v3/settings/get-it", Posture::Admin),
    ("PUT", "/api/v3/settings/get-it", Posture::Admin),
    ("GET", "/api/v3/settings/home", Posture::Admin),
    ("PUT", "/api/v3/settings/home", Posture::Admin),
    ("GET", "/api/v3/settings/library-management", Posture::Admin),
    ("PUT", "/api/v3/settings/library-management", Posture::Admin),
    (
        "GET",
        "/api/v3/settings/library-management/activation-health",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/impact",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/validate",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/profiles",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/library-management/profiles/{profile_id}",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/library-management/profiles/{profile_id}",
        Posture::Admin,
    ),
    (
        "DELETE",
        "/api/v3/settings/library-management/profiles/{profile_id}",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/profiles/{profile_id}/copy",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/library-management/profiles/{profile_id}/preset-diff",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/profiles/{profile_id}/export",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/profile-imports/preview",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library-management/profile-imports",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/indexers", Posture::Admin),
    ("POST", "/api/v3/settings/indexers", Posture::Admin),
    ("POST", "/api/v3/settings/indexers/reorder", Posture::Admin),
    (
        "GET",
        "/api/v3/settings/indexers/search-backend",
        Posture::Admin,
    ),
    (
        "PUT",
        "/api/v3/settings/indexers/search-backend",
        Posture::Admin,
    ),
    ("POST", "/api/v3/settings/indexers/test", Posture::Admin),
    ("PUT", "/api/v3/settings/indexers/{id}", Posture::Admin),
    ("DELETE", "/api/v3/settings/indexers/{id}", Posture::Admin),
    ("GET", "/api/v3/settings/jellyfin", Posture::Admin),
    ("PUT", "/api/v3/settings/jellyfin", Posture::Admin),
    ("POST", "/api/v3/settings/jellyfin/verify", Posture::Admin),
    ("GET", "/api/v3/settings/lastfm", Posture::Admin),
    ("PUT", "/api/v3/settings/lastfm", Posture::Admin),
    ("GET", "/api/v3/settings/library", Posture::Admin),
    ("PUT", "/api/v3/settings/library", Posture::Admin),
    (
        "GET",
        "/api/v3/settings/library/path-mapping",
        Posture::Admin,
    ),
    ("POST", "/api/v3/settings/library/paths", Posture::Admin),
    ("DELETE", "/api/v3/settings/library/paths", Posture::Admin),
    (
        "POST",
        "/api/v3/settings/library/policy-apply-preview",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library/policy-impact",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/library/policy-tree",
        Posture::Admin,
    ),
    (
        "GET",
        "/api/v3/settings/library/restorable-roots",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/library/restore-roots",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/library/schedule", Posture::Admin),
    ("PUT", "/api/v3/settings/library/schedule", Posture::Admin),
    ("GET", "/api/v3/settings/library/sync", Posture::Admin),
    ("PUT", "/api/v3/settings/library/sync", Posture::Admin),
    ("GET", "/api/v3/settings/library/watcher", Posture::Admin),
    ("PUT", "/api/v3/settings/library/watcher", Posture::Admin),
    ("GET", "/api/v3/settings/listenbrainz", Posture::Admin),
    ("PUT", "/api/v3/settings/listenbrainz", Posture::Admin),
    (
        "POST",
        "/api/v3/settings/listenbrainz/verify",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/musicbrainz", Posture::Admin),
    ("PUT", "/api/v3/settings/musicbrainz", Posture::Admin),
    (
        "POST",
        "/api/v3/settings/musicbrainz/activate",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/musicbrainz/brainzmash/consent",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/musicbrainz/brainzmash/stage",
        Posture::Admin,
    ),
    (
        "POST",
        "/api/v3/settings/musicbrainz/verify",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/navidrome", Posture::Admin),
    ("PUT", "/api/v3/settings/navidrome", Posture::Admin),
    ("POST", "/api/v3/settings/navidrome/verify", Posture::Admin),
    ("GET", "/api/v3/settings/oidc", Posture::Admin),
    ("PUT", "/api/v3/settings/oidc", Posture::Admin),
    ("POST", "/api/v3/settings/oidc/verify", Posture::Admin),
    ("GET", "/api/v3/settings/plex", Posture::Admin),
    ("PUT", "/api/v3/settings/plex", Posture::Admin),
    ("GET", "/api/v3/settings/plex/libraries", Posture::Admin),
    ("POST", "/api/v3/settings/plex/verify", Posture::Admin),
    ("GET", "/api/v3/settings/preferences", Posture::Admin),
    ("PUT", "/api/v3/settings/preferences", Posture::Admin),
    ("GET", "/api/v3/settings/primary-source", Posture::Admin),
    ("PUT", "/api/v3/settings/primary-source", Posture::Admin),
    ("GET", "/api/v3/settings/prowlarr/config", Posture::Admin),
    ("PUT", "/api/v3/settings/prowlarr/config", Posture::Admin),
    ("POST", "/api/v3/settings/prowlarr/test", Posture::Admin),
    ("GET", "/api/v3/settings/scrobble", Posture::Admin),
    ("PUT", "/api/v3/settings/scrobble", Posture::Admin),
    ("GET", "/api/v3/settings/security", Posture::Admin),
    ("PUT", "/api/v3/settings/security", Posture::Admin),
    (
        "POST",
        "/api/v3/settings/security/verify-hibp",
        Posture::Admin,
    ),
    ("GET", "/api/v3/settings/wrapped", Posture::Admin),
    ("PUT", "/api/v3/settings/wrapped", Posture::Admin),
    ("GET", "/api/v3/settings/youtube", Posture::Admin),
    ("PUT", "/api/v3/settings/youtube", Posture::Admin),
    ("POST", "/api/v3/settings/youtube/verify", Posture::Admin),
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
    // Stage-10 admin UX.
    ("GET", "/api/v3/admin/backups", Posture::Admin),
    ("POST", "/api/v3/admin/backups", Posture::Admin),
    (
        "GET",
        "/api/v3/admin/backups/{name}/restore-report",
        Posture::Admin,
    ),
    ("GET", "/api/v3/admin/cache/stats", Posture::Admin),
    ("POST", "/api/v3/admin/cache/clear", Posture::Admin),
    ("GET", "/api/v3/admin/queue-stats", Posture::Admin),
    ("GET", "/api/v3/admin/provider-stats", Posture::Admin),
    ("GET", "/api/v3/admin/users/{id}/quota", Posture::Admin),
    ("PUT", "/api/v3/admin/users/{id}/quota", Posture::Admin),
    // Stage-10 plugins + scrobble. Plugin management and the panel bundle
    // are admin-only; sources, guarded plugin HTTP, and the caller's own
    // scrobble settings admit any signed-in user.
    ("GET", "/api/v3/plugins", Posture::Admin),
    ("POST", "/api/v3/plugins/install", Posture::Admin),
    ("POST", "/api/v3/plugins/install/preview", Posture::Admin),
    ("POST", "/api/v3/plugins/{name}/update", Posture::Admin),
    ("PUT", "/api/v3/plugins/{name}", Posture::Admin),
    ("DELETE", "/api/v3/plugins/{name}", Posture::Admin),
    ("GET", "/api/v3/plugins/sources", Posture::User),
    ("GET", "/api/v3/plugins/ext/{name}/{subpath}", Posture::User),
    (
        "POST",
        "/api/v3/plugins/ext/{name}/{subpath}",
        Posture::User,
    ),
    (
        "DELETE",
        "/api/v3/plugins/ext/{name}/{subpath}",
        Posture::User,
    ),
    ("GET", "/api/v3/plugins/{name}/ui/panel.js", Posture::Admin),
    ("GET", "/api/v3/me/scrobble-preferences", Posture::User),
    ("PUT", "/api/v3/me/scrobble-preferences", Posture::User),
    ("PUT", "/api/v3/me/connections/listenbrainz", Posture::User),
    ("GET", "/api/v3/me/connections/listenbrainz", Posture::User),
    (
        "DELETE",
        "/api/v3/me/connections/listenbrainz",
        Posture::User,
    ),
    // Stage-10 jobs: the playlist export trigger and the precache trigger
    // are both admin-only.
    (
        "POST",
        "/api/v3/settings/navidrome/playlist-sync",
        Posture::Admin,
    ),
    ("POST", "/api/v3/admin/precache/run", Posture::Admin),
    // Stage-12 gap step: admin reimport behind the request card.
    (
        "POST",
        "/api/v3/downloads/tasks/{task_id}/reimport",
        Posture::Admin,
    ),
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
    // Stage-12 gap step.
    (
        "GET",
        "/api/v3/local-library/albums/match/{mbid}",
        Posture::User,
    ),
    // Stage-4 unified search.
    ("GET", "/api/v3/search", Posture::User),
    ("GET", "/api/v3/search/{bucket}", Posture::User),
    ("GET", "/api/v3/search/suggest", Posture::User),
    ("POST", "/api/v3/search/enrich/batch", Posture::User),
    // Catalog: MusicBrainz artist and album pages. The dummy id is not an
    // MBID or a library album, so every row answers 400 without dialing out.
    ("GET", "/api/v3/artists/{artist_mbid}", Posture::User),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/extended",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/releases",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/similar",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/top-songs",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/top-albums",
        Posture::User,
    ),
    ("GET", "/api/v3/artists/{artist_mbid}/lastfm", Posture::User),
    (
        "GET",
        "/api/v3/artists/{artist_mbid}/purchase-options",
        Posture::User,
    ),
    ("GET", "/api/v3/albums/{album_id}", Posture::User),
    ("GET", "/api/v3/albums/{album_id}/basic", Posture::User),
    ("GET", "/api/v3/albums/{album_id}/tracks", Posture::User),
    ("GET", "/api/v3/albums/{album_id}/editions", Posture::User),
    ("POST", "/api/v3/albums/{album_id}/refresh", Posture::User),
    ("PUT", "/api/v3/albums/{album_id}/edition", Posture::Curator),
    (
        "DELETE",
        "/api/v3/albums/{album_id}/edition",
        Posture::Curator,
    ),
    ("GET", "/api/v3/albums/{album_id}/similar", Posture::User),
    (
        "GET",
        "/api/v3/albums/{album_id}/more-by-artist",
        Posture::User,
    ),
    ("GET", "/api/v3/albums/{album_id}/lastfm", Posture::User),
    (
        "GET",
        "/api/v3/albums/{album_id}/purchase-options",
        Posture::User,
    ),
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
    ("GET", "/api/v3/following/concerts", Posture::User),
    ("GET", "/api/v3/following/concerts/cities", Posture::User),
    ("PUT", "/api/v3/following/concerts/cities", Posture::User),
    (
        "GET",
        "/api/v3/following/concerts/city-search",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/following/concerts/unseen-count",
        Posture::User,
    ),
    ("POST", "/api/v3/following/concerts/seen", Posture::User),
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
    ("GET", "/api/v3/library/albums/{id}/artwork", Posture::User),
    ("GET", "/api/v3/version", Posture::User),
    ("GET", "/api/v3/version/check-update", Posture::User),
    ("GET", "/api/v3/version/releases", Posture::User),
    ("GET", "/api/v3/wrapped/users", Posture::WrappedKey),
    ("GET", "/api/v3/wrapped/user/{user_id}", Posture::WrappedKey),
    ("GET", "/api/v3/wrapped/server", Posture::WrappedKey),
    // Remote sources.
    ("GET", "/api/v3/me/connections", Posture::User),
    ("GET", "/api/v3/remotes/{source}/moods", Posture::User),
    ("GET", "/api/v3/remotes/{source}/filters", Posture::User),
    (
        "GET",
        "/api/v3/remotes/{source}/most-played/albums",
        Posture::User,
    ),
    (
        "GET",
        "/api/v3/remotes/{source}/most-played/artists",
        Posture::User,
    ),
    ("GET", "/api/v3/remotes/{source}/analytics", Posture::User),
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
    // Stage-12 gap step.
    ("GET", "/api/v3/remotes/{source}/random", Posture::User),
    ("GET", "/api/v3/remotes/{source}/discovery", Posture::User),
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
    ("GET", "/api/v3/stream/{source}/{*key}", Posture::User),
    ("HEAD", "/api/v3/stream/{source}/{*key}", Posture::User),
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
    ("DELETE", "/api/v3/me/connections/spotify", Posture::User),
    ("GET", "/api/v3/system/health", Posture::User),
    ("GET", "/api/v3/acquire/health", Posture::User),
    ("GET", "/api/v3/acquire/slskd/status", Posture::User),
    ("GET", "/api/v3/acquire/sabnzbd/status", Posture::Admin),
    // Stage-8 library engine.
    ("GET", "/api/v3/library/roots", Posture::User),
    ("POST", "/api/v3/library/roots", Posture::Admin),
    ("POST", "/api/v3/library/scan", Posture::Admin),
    ("GET", "/api/v3/library/scan/runs", Posture::User),
    ("GET", "/api/v3/library/scan/runs/{id}", Posture::User),
    ("POST", "/api/v3/library/identify", Posture::Curator),
    ("GET", "/api/v3/library/reviews", Posture::User),
    (
        "POST",
        "/api/v3/library/reviews/{id}/approve",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/reviews/{id}/reject",
        Posture::Curator,
    ),
    ("POST", "/api/v3/library/manage/preview", Posture::Curator),
    ("POST", "/api/v3/library/manage/apply", Posture::Curator),
    ("POST", "/api/v3/library/manage/undo", Posture::Curator),
    (
        "POST",
        "/api/v3/library/manage/baseline/restore",
        Posture::Curator,
    ), // Library contributions; the release-editor callback is token identified.
    (
        "POST",
        "/api/v3/library/albums/{id}/contributions",
        Posture::Curator,
    ),
    ("GET", "/api/v3/library/contributions/{id}", Posture::User),
    (
        "PUT",
        "/api/v3/library/contributions/{id}/draft",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/rebuild",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/cancel",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/discogs/search",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/discogs/select",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/discogs/remove",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/duplicates",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/attach",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/seed",
        Posture::Curator,
    ),
    (
        "PUT",
        "/api/v3/library/contributions/{id}/musicbrainz/result",
        Posture::Curator,
    ),
    (
        "POST",
        "/api/v3/library/contributions/{id}/musicbrainz/verify",
        Posture::Curator,
    ),
    (
        "GET",
        "/api/v3/library/contributions/musicbrainz/callback",
        Posture::Public,
    ),
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

// Multi-thread: the plugins role lookup bridges the async user store with
// `block_in_place`, which needs a multi-thread runtime (production runs
// one; the other e2e tests never reach those extractors).
#[tokio::test(flavor = "multi_thread")]
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

    // Anonymous pass: protected rows 401 with the Bearer challenge; public
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
                    "{method} {uri}: Bearer challenge"
                );
            }
        }
    }

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
        if matches!(
            (*method, *template),
            ("PUT", "/api/v3/library/albums/{album_id}/edition-pin")
                | ("PUT", "/api/v3/albums/{album_id}/edition")
        ) {
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
        // service (404 on the empty catalog, 400 for the dummy album id)
        // instead of failing validation.
        if matches!(
            (*method, *template),
            ("PUT", "/api/v3/library/albums/{album_id}/edition-pin")
                | ("PUT", "/api/v3/albums/{album_id}/edition")
        ) {
            body = Some(json!({"release_mbid": "e2e-dummy-id"}));
        }
        // Shaped binding body: a BrainzMash binding on a non-Brainzmash
        // selection answers 400 without probing. The empty object would
        // decode as a default tier update and probe MusicBrainz for real,
        // which the matrix must never do.
        if *template == "/api/v3/settings/musicbrainz/verify" && *method == "POST" {
            body = Some(json!({
                "access_revision": "matrix",
                "source_id": "matrix",
                "generation": 1,
                "disclosure_version": "matrix",
            }));
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
        // Honest-503 rows: no Jellyfin or Plex server is set up in the
        // scratch config, so the import directories report unconfigured,
        // and plex.tv is unreachable, so the admin's Plex settings sign-in
        // cannot mint a PIN. The 503 is the contract here, not a failure,
        // so it pins exactly.
        if matches!(
            (*method, *template),
            ("GET", "/api/v3/admin/import/jellyfin")
                | ("GET", "/api/v3/admin/import/plex")
                | ("POST", "/api/v3/auth/plex/start/connect")
        ) {
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

#[tokio::test]
async fn concurrent_setups_yield_exactly_one_admin() {
    let e2e = E2e::open("setup-race").await;
    let app = e2e.router();
    let setup = |username: &'static str| {
        call(
            app.clone(),
            "POST",
            "/api/v3/auth/setup",
            &[],
            Some(json!({"username": username, "password": "first-admin-password-1"})),
        )
    };
    let ((first, _, _), (second, _, _)) = tokio::join!(setup("root"), setup("other"));
    let mut statuses = [first, second];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);
}

// ---------------------------------------------------------------------------
// Rate limits and edge failures
// ---------------------------------------------------------------------------

/// One request from a given client address (as `ConnectInfo` would carry).
async fn call_from(app: Router, peer: &str, uri: &str, body: Option<Value>) -> StatusCode {
    let peer: std::net::SocketAddr = peer.parse().expect("peer address");
    let mut builder = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(uri)
        .header("host", HOST);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let mut request = builder
        .body(Body::from(
            body.map(|json| json.to_string()).unwrap_or_default(),
        ))
        .expect("request builds");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    app.oneshot(request)
        .await
        .expect("router responds")
        .status()
}

#[tokio::test]
async fn rate_limits_key_by_client_and_username() {
    let e2e = E2e::open("rate-limits").await;
    let app = e2e.router();

    // One client burning the setup budget (burst 3) leaves others alone.
    // An empty body fails validation, so no request does real work.
    let mut statuses = Vec::new();
    for _ in 0..4 {
        statuses.push(
            call_from(
                app.clone(),
                "198.51.100.1:1",
                "/api/v3/auth/setup",
                Some(json!({})),
            )
            .await,
        );
    }
    assert_eq!(
        statuses.last(),
        Some(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    let other = call_from(
        app.clone(),
        "198.51.100.2:1",
        "/api/v3/auth/setup",
        Some(json!({})),
    )
    .await;
    assert_eq!(
        other,
        StatusCode::BAD_REQUEST,
        "another client keeps its budget"
    );

    // The setup class covers only the setup call, never its status probe.
    for _ in 0..6 {
        let status = call_from(
            app.clone(),
            "198.51.100.1:1",
            "/api/v3/auth/setup/status",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // Guessing one username from many addresses at once meets the
    // per-username wall (burst 5) even though every address is fresh.
    let peers: Vec<String> = (0..8).map(|n| format!("203.0.113.{n}:1")).collect();
    let attempts = peers.iter().map(|peer| {
        call_from(
            app.clone(),
            peer,
            "/api/v3/auth/login",
            Some(json!({"username": "victim", "password": "wrong-password-123"})),
        )
    });
    let statuses = futures_util::future::join_all(attempts).await;
    let limited = statuses
        .iter()
        .filter(|status| **status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(limited, 3, "{statuses:?}");
}

#[tokio::test]
async fn failed_authentications_are_throttled_per_client() {
    let e2e = E2e::open("auth-failures").await;
    let app = e2e.router();

    // The gate answers before the request limiter, so it charges its own
    // per-address bucket (burst 20) for every failed authentication.
    let mut last = StatusCode::OK;
    for _ in 0..25 {
        last = call_from(app.clone(), "198.51.100.7:1", "/api/v3/me", None).await;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    let other = call_from(app, "198.51.100.8:1", "/api/v3/me", None).await;
    assert_eq!(
        other,
        StatusCode::UNAUTHORIZED,
        "another client is untouched"
    );
}

#[tokio::test]
async fn login_store_failure_is_a_500_not_bad_credentials() {
    let e2e = E2e::open("login-store-down").await;
    setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    e2e.runtime
        .lane()
        .write(droppedneedle::db::Lane::Foreground, "test.break", |tx| {
            tx.execute_batch("ALTER TABLE auth_providers RENAME TO auth_providers_gone")?;
            Ok(())
        })
        .await
        .expect("table renames");
    let (status, body, _) =
        login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
}

#[tokio::test]
async fn password_change_ends_every_other_session() {
    let e2e = E2e::open("password-change").await;
    let (_, setup_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let (_, body, _) = login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let other = bearer(body["token"].as_str().expect("second session"));
    let current = bearer(setup_token.as_str().expect("setup token"));
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/me/password",
        &[("authorization", current.as_str())],
        Some(json!({
            "current_password": "e2e-owner-password-1",
            "new_password": "e2e-owner-password-2",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (auth, expected) in [
        (&current, StatusCode::OK),
        (&other, StatusCode::UNAUTHORIZED),
    ] {
        let (status, _, _) = call(
            e2e.router(),
            "GET",
            "/api/v3/me",
            &[("authorization", auth.as_str())],
            None,
        )
        .await;
        assert_eq!(status, expected);
    }
}

// ---------------------------------------------------------------------------
// Login p95 probe against the login budget
// ---------------------------------------------------------------------------

/// Nearest-rank percentile over ascending samples.
fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    let rank = ((pct / 100.0 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

#[tokio::test]
#[ignore = "login timing budget; run explicitly with --release: cargo test --release --test it login_p95 -- --ignored"]
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
         over {N} sequential logins \
         (p50 {p50:?}, max {max:?}); Argon2id work-factor verify cost counts \
         and dominates by design - slowness here is the security feature"
    );
}
