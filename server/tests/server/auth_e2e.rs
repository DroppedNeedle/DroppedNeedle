//! Real-app auth journeys plus the standing auth contract.
//!
//! Everything here runs against `create_app` with the production SQLite
//! bundle over scratch databases. HIBP screening is switched off through
//! its real config knob; no test touches an IdP, mail, or the network.
//!
//! - `journey_*`: setup and sessions, the admin user lifecycle, and app
//!   passwords across the compat contracts.
//! - `auth_on_every_endpoint`: every documented operation answers as the
//!   posture its `security` declares, for anonymous, user, trusted, admin
//!   and wrapped-key callers. The public and wrapped-key operations are
//!   pinned by name. Two-user ownership lives in `auth_ownership`.
//! - Rate limits, setup races, and store-failure handling at the edge.
//! - `login_p95_*`: the login latency budget, run on request.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle::docs::ApiDoc;
use droppedneedle::{AppState, create_app};
use droppedneedle_auth::compat_auth::jellyfin::{authenticate_by_name, resolve_token};
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::compat_auth::subsonic::{
    SubsonicDenied, SubsonicParams, WRONG_CREDENTIALS, authenticate, md5_hex,
};
use droppedneedle_auth::prod::ProdAuth;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_auth::wiring::{AuthSetup, Upstreams};
use droppedneedle_kernel::config::AppConfig;
use droppedneedle_kernel::config::DEFAULT_PORT;
use droppedneedle_kernel::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::sections::SecuritySettings;
use droppedneedle_kernel::runtime_config::{
    ConfigStore, Crypto, Secret, secret_sections::WrappedSettings,
};
use droppedneedle_providers::Providers;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use utoipa::OpenApi as _;

/// Fixed host for every request; cookie mutations also send this as Origin.
pub(crate) const HOST: &str = "e2e.test";
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
    /// What every bundle below is built from; its settings are `store`.
    platform: Platform,
    bundle: ProdAuth,
    pub(crate) store: Arc<ConfigStore>,
    upstreams: Upstreams,
    lastfm: String,
    crypto: Arc<Crypto>,
    db_path: std::path::PathBuf,
    _scratch: crate::common::ScratchDir,
}

impl E2e {
    pub(crate) async fn open(tag: &str) -> Self {
        Self::open_with(tag, |config| config).await
    }

    /// As [`E2e::open`], with the database tuned by `tune`.
    pub(crate) async fn open_with(tag: &str, tune: impl FnOnce(DbConfig) -> DbConfig) -> Self {
        let scratch = crate::common::ScratchDir::new(&format!("auth-e2e-{tag}"));
        let dir = scratch.to_path_buf();
        let runtime = open_runtime(&tune(DbConfig::new(&dir.join("app.db"))))
            .await
            .expect("scratch runtime opens");
        let mut config = AppConfig::new(DEFAULT_PORT);
        config.root_app_dir = dir.clone();
        let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
            .config(config)
            .build()
            .await;
        let store = Arc::clone(&platform.settings);
        // Breach screening off via the real knob: setup and user creation
        // must never dial the network in this suite.
        let mut security: SecuritySettings = store.get().expect("security section reads");
        security.hibp_check = false;
        store.save_blocking(security).expect("hibp switch saves");
        store
            .save_secret_blocking(WrappedSettings {
                api_key: Secret::new(TEST_WRAPPED_KEY),
            })
            .expect("wrapped key saves");
        Self {
            runtime,
            bundle: ProdAuth::new(&platform),
            store,
            // Nothing listens on the discard port: no test reaches the real
            // plex.tv, Last.fm or breach check unless it points these at
            // its own mock.
            upstreams: Upstreams {
                plex_tv: "http://127.0.0.1:9".to_owned(),
                pwned_passwords: "http://127.0.0.1:9/range/".to_owned(),
            },
            lastfm: "http://127.0.0.1:9/".to_owned(),
            crypto: Arc::clone(&platform.crypto),
            platform,
            db_path: dir.join("app.db"),
            _scratch: scratch,
        }
    }

    /// Point the plex.tv and breach-check clients at local mocks.
    pub(crate) fn with_upstreams(mut self, upstreams: Upstreams) -> Self {
        self.upstreams = upstreams;
        self
    }

    /// Point the Last.fm client at a local mock.
    pub(crate) fn with_lastfm(mut self, lastfm: String) -> Self {
        self.lastfm = lastfm;
        self
    }

    /// The scratch database pool, for assertions on stored rows.
    pub(crate) fn pool(&self) -> &sqlx::SqlitePool {
        self.runtime.db().sqlx()
    }

    /// The scratch database, for stores a test builds over it.
    pub(crate) fn db(&self) -> &droppedneedle_kernel::db::Db {
        self.runtime.db()
    }

    fn auth_setup(&self) -> AuthSetup {
        let p = &self.platform;
        let providers = Providers::unpaced_at(
            Arc::new(droppedneedle_providers::ReqwestWire::new(&p.http)),
            droppedneedle_providers::ProviderEndpoints {
                lastfm: self.lastfm.clone(),
                ..droppedneedle_providers::ProviderEndpoints::closed()
            },
        );
        AuthSetup::build(
            p,
            self.bundle.clone(),
            &self.upstreams,
            &providers.lastfm,
            Arc::new(
                droppedneedle_media::remotes::plex::ConfiguredPlexServer::new(
                    p.http.shared().clone(),
                    self.store.live(),
                    droppedneedle_auth::federated::settings::InstallIds::new(Arc::clone(
                        &self.store,
                    )),
                ),
            ),
            droppedneedle::bootstrap::sign_in_links(p),
        )
    }

    /// A fresh router over the same database. Each build carries fresh rate
    /// buckets, so multi-pass tests rebuild instead of tripping the limiter.
    pub(crate) fn router(&self) -> Router {
        create_app(self.state())
    }

    /// A fresh application state over the same database, for tests that
    /// also reach a bundle (the event hub) behind the router.
    pub(crate) fn state(&self) -> AppState {
        let p = &self.platform;
        let auth = self.auth_setup();
        let (library, _) = crate::common::library::library_at(p);
        let collections = crate::common::bundles::collections_at(p);
        let acquire = crate::common::bundles::acquire_at(
            p,
            auth.users.clone(),
            &library,
            &collections.stores,
        );
        let reads = crate::common::bundles::reads_at(
            p,
            auth.users.clone(),
            droppedneedle_catalog::reads::ReadsInputs {
                // Unpaced: no provider answers here, so
                // pacing would only add whole seconds of waiting to rows
                // that are bound to answer "provider down" anyway.
                providers: Arc::new(Providers::unpaced(Arc::new(
                    droppedneedle_providers::executor::ScriptedWire::new(),
                ))),
                ..crate::common::reads_inputs(&p.config.root_app_dir)
            },
            &library,
            collections.wired(),
            Some(&acquire),
        );
        let providers = Arc::new(Providers::unpaced(Arc::new(
            droppedneedle_providers::executor::ScriptedWire::new(),
        )));
        let (media, _worker) = droppedneedle_media::MediaSetup::build(
            p,
            auth.users.clone(),
            library.core.root_source(),
            Arc::new(
                droppedneedle_catalog::reads::collections::remote_import::PlaylistImportSink::new(
                    reads.collections.clone(),
                ),
            ),
            crate::common::bundles::idle_host(p, library.core.clone()),
            droppedneedle_media::MediaSetup::remotes(
                p,
                droppedneedle::bootstrap::Outbound::live().plex_tv,
            ),
            None,
            &providers,
            Arc::new(droppedneedle_acquire::requests::mix::MixGrantHooks::new(
                acquire.requests.mixer.clone(),
            )),
            droppedneedle_kernel::jobs::queue::JobQueue::new(p.db.clone(), p.clock.clone()),
            p.clock.clone(),
        )
        .expect("media bundle builds");
        let compat = droppedneedle_compat::CompatSetup::build(
            droppedneedle_compat::setup::CompatDeps::over_reads(
                p,
                auth.users.clone(),
                media.playback.clone(),
                media.stream.engine.clone(),
                library.scans.clone(),
                &reads,
            ),
        );
        let backup_dir = p.config.root_app_dir.join("backups");
        let jobs = droppedneedle::bootstrap::jobs::JobsSetup::build(
            p,
            self.runtime.checkpoint().clone(),
            media.jobs(p),
            None,
            droppedneedle::bootstrap::jobs::ScansEnded::default(),
        );
        // Concerts over the scratch database and the rig's providers, whose
        // addresses are closed, so no row can reach a real provider.
        let concerts = droppedneedle_catalog::concerts::ConcertsSetup::from_platform(p, &providers);
        let admin = droppedneedle::admin::AdminSetup::new(
            auth.users.clone(),
            acquire.requests.quota.clone(),
            providers.cache.clone(),
            providers.clone(),
        )
        .with_db(droppedneedle::admin::AdminDb::new(p.db.clone()))
        .with_backups(droppedneedle_kernel::db::BackupService::new(
            &self.db_path,
            &backup_dir,
            p.clock.clone(),
        ))
        .with_checkpoint(self.runtime.checkpoint().clone())
        .with_precache(jobs.precache_trigger());
        // The production plugins bundle over the scratch plugins folder,
        // launching nothing, with GitHub's addresses closed.
        let ticks = droppedneedle_plugins::wiring::tick_store(p);
        let plugin_host = droppedneedle_plugins::host::PluginHost::new(
            p.config.plugins_dir(),
            p.settings.clone(),
            Arc::new(crate::common::plugin_fakes::FakeLauncher::new()),
            ticks.clone(),
            library.core.clone(),
        );
        let plugins = droppedneedle_plugins::wiring::PluginsSetup::build(
            p,
            plugin_host,
            ticks,
            crate::common::closed_outbound().plugin_hosts(),
        );
        let settings =
            droppedneedle::settings::wiring::SettingsSetup::for_tests(Arc::clone(&p.ids))
                .expect("test settings bundle builds")
                .with_library_catalog(Arc::new(
                    droppedneedle::settings::library_catalog::SqliteLibraryPolicyCatalog {
                        pool: p.db.sqlx().clone(),
                    },
                ));
        AppState::new(
            Arc::clone(&p.ids),
            p.config.clone(),
            auth,
            reads,
            media,
            acquire,
            droppedneedle_library::wiring::LibraryServices::build(&library),
            compat,
            admin,
            settings,
            jobs,
            plugins,
            concerts,
            p.events.clone(),
        )
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
    let (status, headers, body) = crate::common::through_busy(|| async {
        let (status, body, headers) =
            call_once(app.clone(), method, uri, headers, body.clone()).await;
        (status, headers, body)
    })
    .await;
    (status, body, headers)
}

/// One request, answered as it came: a busy answer is not asked again.
async fn call_once(
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

/// One request answered by status alone; the body is never read.
async fn call_status(app: Router, method: &str, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
    let mut builder = Request::builder().method(method).uri(uri);
    builder = builder.header("host", HOST);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds")
        .status()
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// The session cookie's name when unmarked, as the plain-HTTP address sets it.
const PLAIN_COOKIE: &str = "droppedneedle_sid";
/// Its name when marked `Secure`, as the https address sets it.
const SECURE_COOKIE: &str = "__Secure-droppedneedle_sid";

fn cookie(token: &str) -> String {
    format!("{PLAIN_COOKIE}={token}")
}

/// Raw session token from a Set-Cookie response.
fn set_cookie_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::SET_COOKIE)?.to_str().ok()?;
    let first = value.split(';').next()?.trim();
    first
        .strip_prefix(&format!("{PLAIN_COOKIE}="))
        .map(str::to_owned)
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// First-run setup on a fresh app. Returns (admin id, cookie-or-token).
pub(crate) async fn setup_admin(
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

pub(crate) async fn login(
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

/// One browser holds the https address's `__Secure-` cookie and the plain
/// address's cookie on the same host name. The `__Secure-` one, when sent,
/// is the only one read, dead or alive; sign-out ends the session under
/// each name it carries and clears both.
#[tokio::test]
async fn the_secure_cookie_outranks_the_plain_one_and_sign_out_ends_both() {
    let e2e = E2e::open("two-names").await;
    let (_, admin) = setup_admin(e2e.router(), "e2e-admin", "e2e-admin-password-1", "bearer").await;
    let admin = bearer(admin.as_str().expect("admin token"));
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/admin/users",
        &[("authorization", admin.as_str())],
        Some(json!({"username": "ben", "password": "ben-password-1234"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session = |username: &'static str, password: &'static str| {
        let app = e2e.router();
        async move {
            let (status, body, _) = login(app, username, password, "bearer").await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["token"].as_str().expect("token").to_owned()
        }
    };
    let a = session("e2e-admin", "e2e-admin-password-1").await;
    let b = session("ben", "ben-password-1234").await;
    let c = session("e2e-admin", "e2e-admin-password-1").await;
    let me = |jar: String| {
        let app = e2e.router();
        async move {
            let (status, body, _) = call(app, "GET", "/api/v3/me", &[("cookie", &jar)], None).await;
            (status == StatusCode::OK).then(|| body["username"].as_str().unwrap_or("").to_owned())
        }
    };

    let both = format!("{SECURE_COOKIE}={b}; {PLAIN_COOKIE}={a}");
    assert_eq!(me(both.clone()).await.as_deref(), Some("ben"));
    let swapped = format!("{PLAIN_COOKIE}={a}; {SECURE_COOKIE}={b}");
    assert_eq!(me(swapped).await.as_deref(), Some("ben"));
    let unknown = format!("{SECURE_COOKIE}=not-a-session; {PLAIN_COOKIE}={a}");
    assert_eq!(
        me(unknown).await,
        None,
        "a dead secure cookie never falls through"
    );
    assert_eq!(
        me(format!("{PLAIN_COOKIE}={a}")).await.as_deref(),
        Some("e2e-admin")
    );

    // Sign-out at the plain address sends only its cookie: that one ends.
    let only_plain = format!("{PLAIN_COOKIE}={c}");
    let (status, _, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/logout",
        &[("cookie", only_plain.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        me(format!("{SECURE_COOKIE}={b}")).await.as_deref(),
        Some("ben")
    );
    let revoked = format!("{SECURE_COOKIE}={c}; {PLAIN_COOKIE}={a}");
    assert_eq!(
        me(revoked).await,
        None,
        "a revoked secure cookie never falls through"
    );

    // Sign-out at the https address sends both: both end, both are cleared.
    let (status, _, headers) = call(
        e2e.router(),
        "POST",
        "/api/v3/auth/logout",
        &[("cookie", both.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cleared: Vec<&str> = headers
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().expect("ascii"))
        .collect();
    assert_eq!(cleared.len(), 2, "{cleared:?}");
    assert!(
        cleared
            .iter()
            .any(|value| value.starts_with(&format!("{PLAIN_COOKIE}=; "))
                && value.contains("Max-Age=0")
                && !value.contains("Secure")),
        "{cleared:?}"
    );
    assert!(
        cleared
            .iter()
            .any(|value| value.starts_with(&format!("{SECURE_COOKIE}=; "))
                && value.contains("Max-Age=0")
                && value.ends_with("; Secure")),
        "{cleared:?}"
    );
    assert_eq!(me(format!("{PLAIN_COOKIE}={a}")).await, None);
    assert_eq!(me(format!("{SECURE_COOKIE}={b}")).await, None);
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

/// The role is read with the token on every request, so a demotion holds
/// from the demoted admin's next request on the token they already have.
#[tokio::test]
async fn a_demoted_admin_is_refused_on_the_next_request() {
    let e2e = E2e::open("demoted-admin").await;
    let (_, owner_token) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let owner_auth = bearer(owner_token.as_str().expect("owner token"));
    let owner_headers = [("authorization", owner_auth.as_str())];
    let (status, body, _) = call(
        e2e.router(),
        "POST",
        "/api/v3/admin/users",
        &owner_headers,
        Some(json!({"username": "ada", "password": "ada-password-1234", "role": "admin"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let ada_id = body["id"].as_str().expect("ada id").to_owned();
    let (status, body, _) = login(e2e.router(), "ada", "ada-password-1234", "bearer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ada_auth = bearer(body["token"].as_str().expect("ada token"));
    let ada_headers = [("authorization", ada_auth.as_str())];
    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/admin/users",
        &ada_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body, _) = call(
        e2e.router(),
        "PUT",
        &format!("/api/v3/admin/users/{ada_id}/role"),
        &owner_headers,
        Some(json!({"role": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body, _) = call(
        e2e.router(),
        "GET",
        "/api/v3/admin/users",
        &ada_headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(error_code(&body), "FORBIDDEN");
    assert_eq!(body["error"]["message"], json!("Admin role required"));
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

/// The posture an operation declares in the document's `security`, which
/// the mount stamps from the group it registered the route in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declared {
    /// `[{}]`: anyone, no session.
    Public,
    /// `[{"session": ["user"]}]`: any signed-in account.
    User,
    /// `[{"session": ["curator"]}]`: trusted or admin.
    Curator,
    /// `[{"session": ["admin"]}]`: admin only.
    Admin,
    /// `[{"wrapped_key": []}]`: the wrapped API key, never a session.
    WrappedKey,
}

/// One documented operation and the posture it declares.
struct Operation {
    method: &'static str,
    path: String,
    posture: Declared,
}

/// Every operation the document publishes. A key that is not an HTTP
/// method, or a `security` the matrix does not know, fails by name.
fn documented_operations() -> Vec<Operation> {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("document serializes");
    let paths = doc["paths"].as_object().expect("document has paths");
    let mut operations = Vec::new();
    for (path, item) in paths {
        for (key, operation) in item.as_object().expect("path item is an object") {
            let method = match key.as_str() {
                "get" => "GET",
                "put" => "PUT",
                "post" => "POST",
                "delete" => "DELETE",
                "options" => "OPTIONS",
                "head" => "HEAD",
                "patch" => "PATCH",
                "trace" => "TRACE",
                "summary" | "description" | "servers" | "parameters" => continue,
                other => panic!("{path}: `{other}` is not a method the auth matrix knows"),
            };
            let security = &operation["security"];
            let posture = declared(security).unwrap_or_else(|| {
                panic!(
                    "{method} {path}: security {security} is not a posture the auth matrix knows"
                )
            });
            operations.push(Operation {
                method,
                path: path.clone(),
                posture,
            });
        }
    }
    operations
}

/// The posture one `security` value states: exactly one requirement, of a
/// known scheme and scope.
fn declared(security: &Value) -> Option<Declared> {
    let [requirement] = security.as_array()?.as_slice() else {
        return None;
    };
    let requirement = requirement.as_object()?;
    let mut schemes = requirement.iter();
    let Some((scheme, scopes)) = schemes.next() else {
        return Some(Declared::Public);
    };
    if schemes.next().is_some() {
        return None;
    }
    let scopes: Vec<&str> = scopes
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<_>>()?;
    match (scheme.as_str(), scopes.as_slice()) {
        ("session", ["user"]) => Some(Declared::User),
        ("session", ["curator"]) => Some(Declared::Curator),
        ("session", ["admin"]) => Some(Declared::Admin),
        ("wrapped_key", []) => Some(Declared::WrappedKey),
        _ => None,
    }
}

/// The operations anyone may call, pinned by name. A new public operation
/// is a reviewed edit here: the document's public set must equal this list.
const PUBLIC_OPERATIONS: &[(&str, &str)] = &[
    ("GET", "/health"),
    ("GET", "/openapi.json"),
    ("POST", "/api/v3/auth/login"),
    ("POST", "/api/v3/auth/logout"),
    ("POST", "/api/v3/auth/setup"),
    ("GET", "/api/v3/auth/setup/status"),
    ("GET", "/api/v3/auth/providers"),
    ("POST", "/api/v3/auth/password-recovery/reset"),
    ("POST", "/api/v3/auth/oidc/authorize"),
    ("GET", "/api/v3/auth/oidc/callback"),
    ("POST", "/api/v3/auth/oidc/exchange"),
    ("POST", "/api/v3/auth/jellyfin/login"),
    ("POST", "/api/v3/auth/plex/start"),
    ("POST", "/api/v3/auth/plex/poll/login"),
    // Token-identified callbacks from MusicBrainz and Spotify.
    ("GET", "/api/v3/library/contributions/musicbrainz/callback"),
    ("GET", "/api/v3/acquire/spotify/auth/callback"),
];

/// The operations that take the wrapped API key, pinned the same way.
const WRAPPED_KEY_OPERATIONS: &[(&str, &str)] = &[
    ("GET", "/api/v3/wrapped/users"),
    ("GET", "/api/v3/wrapped/user/{user_id}"),
    ("GET", "/api/v3/wrapped/server"),
];

/// What one operation needs beyond its declared posture.
#[derive(Debug, Clone, Copy)]
enum Exception {
    /// `{id}` is filled with the caller's own user id.
    OwnId,
    /// Sent with this body in place of `{}`.
    Body(fn() -> Value),
    /// Read by status alone: the body never ends.
    StatusOnly,
    /// Read as bytes: the body is not JSON.
    Raw,
    /// An admitted caller gets exactly 503: the provider is down.
    ProviderDown,
    /// An admitted caller gets exactly 503 with the internal error code.
    HonestUnavailable,
    /// Revokes the caller's own session; the pass signs in again after it.
    SignsOut,
}

/// The operations the document alone cannot drive. Each applies in every
/// pass, and an entry that names no documented operation fails.
const EXCEPTIONS: &[(&str, &str, Exception, &str)] = &[
    (
        "GET",
        "/api/v3/users/{id}/avatar",
        Exception::OwnId,
        "self-or-admin: each pass reads its own avatar (404, no bytes yet)",
    ),
    (
        "POST",
        "/api/v3/held/{entry_id}/match",
        Exception::Body(item_match_body),
        "an empty object fails to decode before the handler runs",
    ),
    (
        "POST",
        "/api/v3/settings/musicbrainz/verify",
        Exception::Body(verify_body),
        "an empty object would decode as a tier update and probe MusicBrainz",
    ),
    (
        "GET",
        "/api/v3/events/stream",
        Exception::StatusOnly,
        "the live event stream never ends",
    ),
    (
        "GET",
        "/api/v3/covers/release-group/{release_group_id}",
        Exception::Raw,
        "covers answer image bytes",
    ),
    (
        "GET",
        "/api/v3/covers/release/{release_id}",
        Exception::Raw,
        "covers answer image bytes",
    ),
    (
        "GET",
        "/api/v3/covers/artist/{artist_id}",
        Exception::Raw,
        "covers answer image bytes",
    ),
    (
        "GET",
        "/api/v3/home/trending/artists",
        Exception::ProviderDown,
        "reads ListenBrainz charts live, and the harness closes its port",
    ),
    (
        "GET",
        "/api/v3/home/popular/albums",
        Exception::ProviderDown,
        "reads ListenBrainz charts live, and the harness closes its port",
    ),
    (
        "GET",
        "/api/v3/admin/import/jellyfin",
        Exception::HonestUnavailable,
        "no Jellyfin server is set up, so the import directory is unconfigured",
    ),
    (
        "GET",
        "/api/v3/admin/import/plex",
        Exception::HonestUnavailable,
        "no Plex server is set up, so the import directory is unconfigured",
    ),
    (
        "POST",
        "/api/v3/auth/plex/start/connect",
        Exception::HonestUnavailable,
        "plex.tv is unreachable, so no PIN can be minted",
    ),
    (
        "POST",
        "/api/v3/auth/logout-all",
        Exception::SignsOut,
        "it revokes the pass's own session by design",
    ),
];

fn item_match_body() -> Value {
    json!({"release_group_mbid": "e2e-dummy-id"})
}

/// A BrainzMash binding on a non-BrainzMash selection: refused with 400
/// before anything is probed.
fn verify_body() -> Value {
    json!({
        "access_revision": "matrix",
        "source_id": "matrix",
        "generation": 1,
        "disclosure_version": "matrix",
    })
}

fn exception(method: &str, path: &str) -> Option<Exception> {
    EXCEPTIONS
        .iter()
        .find(|(m, p, _, _)| *m == method && *p == path)
        .map(|(_, _, exception, _)| *exception)
}

/// Fill `{param}` segments with a dummy id.
fn concretize(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest.find('}').expect("balanced template braces");
        out.push_str("e2e-dummy-id");
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn request_body(method: &str, exception: Option<Exception>) -> Option<Value> {
    match (method, exception) {
        (_, Some(Exception::Body(body))) => Some(body()),
        ("POST" | "PUT" | "PATCH", _) => Some(json!({})),
        _ => None,
    }
}

/// The public and wrapped-key pins equal the document's sets exactly, and
/// every exception names a documented operation.
#[test]
fn public_and_own_credential_operations_are_pinned() {
    let operations = documented_operations();
    let with = |posture: Declared| -> std::collections::BTreeSet<(String, String)> {
        operations
            .iter()
            .filter(|op| op.posture == posture)
            .map(|op| (op.method.to_owned(), op.path.clone()))
            .collect()
    };
    let pinned = |list: &[(&str, &str)]| -> std::collections::BTreeSet<(String, String)> {
        list.iter()
            .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
            .collect()
    };
    assert_eq!(
        with(Declared::Public),
        pinned(PUBLIC_OPERATIONS),
        "the public operations changed; a new public operation is a reviewed edit to PUBLIC_OPERATIONS"
    );
    assert_eq!(
        with(Declared::WrappedKey),
        pinned(WRAPPED_KEY_OPERATIONS),
        "the wrapped-key operations changed; edit WRAPPED_KEY_OPERATIONS after review"
    );
    for (method, path, _, _) in EXCEPTIONS {
        assert!(
            operations
                .iter()
                .any(|op| op.method == *method && op.path == *path),
            "exception {method} {path} names no documented operation; drop it"
        );
    }
}

/// A signed-in caller of the matrix.
struct Caller {
    name: &'static str,
    username: &'static str,
    password: &'static str,
    id: String,
    auth: String,
    role: CallerRole,
}

#[derive(Clone, Copy)]
enum CallerRole {
    User,
    Trusted,
    Admin,
}

/// What a caller must get on an operation of a given posture.
enum Expect {
    Admitted,
    Forbidden,
    NoSession,
}

fn expect(role: CallerRole, posture: Declared) -> Expect {
    match (role, posture) {
        (_, Declared::WrappedKey) => Expect::NoSession,
        (_, Declared::User) => Expect::Admitted,
        (CallerRole::Trusted | CallerRole::Admin, Declared::Curator) => Expect::Admitted,
        (CallerRole::Admin, Declared::Admin) => Expect::Admitted,
        (CallerRole::User, Declared::Curator) => Expect::Forbidden,
        (CallerRole::User | CallerRole::Trusted, Declared::Admin) => Expect::Forbidden,
        (_, Declared::Public) => unreachable!("public operations are skipped"),
    }
}

/// The app under the matrix: one build, its limiter emptied before every
/// request so no row spends another's budget.
struct MatrixApp {
    app: Router,
    limits: Arc<droppedneedle_auth::session::rate_limit::RateLimiter>,
}

impl MatrixApp {
    fn fresh(&self) -> Router {
        self.limits.reset();
        self.app.clone()
    }
}

/// One signed-in pass over every gated and wrapped-key operation.
async fn credentialed_pass(matrix: &MatrixApp, operations: &[Operation], caller: &mut Caller) {
    let who = caller.name;
    for op in operations {
        if op.posture == Declared::Public {
            continue;
        }
        let method = op.method;
        let exception = exception(method, &op.path);
        let uri = match exception {
            Some(Exception::OwnId) => op.path.replace("{id}", &caller.id),
            _ => concretize(&op.path),
        };
        let auth = caller.auth.clone();
        let headers = [("authorization", auth.as_str())];
        let (status, body) = match exception {
            Some(Exception::StatusOnly) => (
                call_status(matrix.fresh(), method, &uri, &headers).await,
                Value::Null,
            ),
            Some(Exception::Raw) => {
                let (status, _, _) = call_raw(matrix.fresh(), method, &uri, &headers).await;
                (status, Value::Null)
            }
            _ => {
                let (status, body, _) = call_lenient(
                    matrix.fresh(),
                    method,
                    &uri,
                    &headers,
                    request_body(method, exception),
                )
                .await;
                (status, body)
            }
        };
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: the {who} pass tripped the limiter"
        );
        match expect(caller.role, op.posture) {
            Expect::NoSession => assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "a {who} session must not satisfy {method} {uri}, got {status}: {body}"
            ),
            Expect::Forbidden => assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{who} {method} {uri} must 403, got {status}: {body}"
            ),
            Expect::Admitted => {
                assert!(
                    status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
                    "{who} {method} {uri} must be admitted, got {status}: {body}"
                );
                match exception {
                    Some(Exception::ProviderDown) => assert_eq!(
                        status,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "{who} {method} {uri} must 503 while charts are down: {body}"
                    ),
                    Some(Exception::HonestUnavailable) => {
                        assert_eq!(
                            status,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "{who} {method} {uri} must 503 while unconfigured, got {status}: {body}"
                        );
                        assert_eq!(error_code(&body), "INTERNAL_ERROR", "{method} {uri}");
                    }
                    _ => assert!(
                        !status.is_server_error(),
                        "{who} {method} {uri} must not 5xx on dummy input, got {status}: {body}"
                    ),
                }
                if matches!(exception, Some(Exception::SignsOut)) {
                    let (status, body, _) =
                        login(matrix.fresh(), caller.username, caller.password, "bearer").await;
                    assert_eq!(status, StatusCode::OK, "{who} signs in again: {body}");
                    caller.auth = bearer(body["token"].as_str().expect("fresh token"));
                }
            }
        }
    }
}

/// Every documented operation answers as its declared posture says, for
/// an anonymous caller, a plain user, a trusted user, an admin and the
/// wrapped key.
#[tokio::test]
async fn auth_on_every_endpoint() {
    let operations = documented_operations();
    let e2e = E2e::open("matrix").await;
    let state = e2e.state();
    let matrix = MatrixApp {
        limits: Arc::clone(&state.auth.limits),
        app: create_app(state),
    };

    // Anonymous: session operations 401 with the Bearer challenge, public
    // ones anything but 401, wrapped-key ones 401 with no challenge.
    for op in &operations {
        let method = op.method;
        let uri = concretize(&op.path);
        let body = request_body(method, exception(method, &op.path));
        let (status, response_body, headers) = call(matrix.fresh(), method, &uri, &[], body).await;
        assert_ne!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "{method} {uri}: the anonymous pass tripped the limiter"
        );
        match op.posture {
            Declared::Public => assert_ne!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} is public but answered 401: {response_body}"
            ),
            Declared::WrappedKey => {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} must 401 anonymously, got {status}: {response_body}"
                );
                assert_eq!(error_code(&response_body), "UNAUTHORIZED", "{method} {uri}");
                assert!(
                    headers.get("www-authenticate").is_none(),
                    "{method} {uri}: a shared-secret rejection must not send a Bearer challenge"
                );
            }
            Declared::User | Declared::Curator | Declared::Admin => {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} must 401 anonymously, got {status}: {response_body}"
                );
                // A HEAD answer has no body, so only the others pin the code.
                if method != "HEAD" {
                    assert_eq!(error_code(&response_body), "UNAUTHORIZED", "{method} {uri}");
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

    // One admin, one plain user and one trusted user, made the way an
    // admin makes them.
    let (admin_user, admin_token) = setup_admin(
        matrix.fresh(),
        "e2e-owner",
        "e2e-owner-password-1",
        "bearer",
    )
    .await;
    let mut admin = Caller {
        name: "admin",
        username: "e2e-owner",
        password: "e2e-owner-password-1",
        id: admin_user["id"].as_str().expect("admin id").to_owned(),
        auth: bearer(admin_token.as_str().expect("admin token")),
        role: CallerRole::Admin,
    };
    let mut user = matrix_account(&matrix, &admin, "molly", "molly-password-1234", None).await;
    let mut trusted = matrix_account(
        &matrix,
        &admin,
        "tris",
        "tris-password-1234",
        Some("trusted"),
    )
    .await;

    credentialed_pass(&matrix, &operations, &mut user).await;
    credentialed_pass(&matrix, &operations, &mut trusted).await;
    credentialed_pass(&matrix, &operations, &mut admin).await;

    // The wrapped key admits where no session can.
    for op in operations
        .iter()
        .filter(|op| op.posture == Declared::WrappedKey)
    {
        let method = op.method;
        let uri = concretize(&op.path);
        let (status, response_body, _) = call(
            matrix.fresh(),
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

/// An account the admin creates, given `role` when set, signed in by bearer.
async fn matrix_account(
    matrix: &MatrixApp,
    admin: &Caller,
    username: &'static str,
    password: &'static str,
    role: Option<&str>,
) -> Caller {
    let (status, body, _) = call(
        matrix.fresh(),
        "POST",
        "/api/v3/admin/users",
        &[("authorization", admin.auth.as_str())],
        Some(json!({"username": username, "password": password})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("new user id").to_owned();
    if let Some(role) = role {
        let (status, body, _) = call(
            matrix.fresh(),
            "PUT",
            &format!("/api/v3/admin/users/{id}/role"),
            &[("authorization", admin.auth.as_str())],
            Some(json!({"role": role})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body, _) = login(matrix.fresh(), username, password, "bearer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Caller {
        name: if role.is_some() { "trusted" } else { "user" },
        username,
        password,
        id,
        auth: bearer(body["token"].as_str().expect("token")),
        role: if role.is_some() {
            CallerRole::Trusted
        } else {
            CallerRole::User
        },
    }
}

// ---------------------------------------------------------------------------
// Trusted tier: curator writes admit, admin reads still forbid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trusted_tier_edition_admitted_approvals_forbidden() {
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

    // Choosing an edition reaches the service: 400 for the dummy release
    // id proves the role gate admitted (a plain user 403s here).
    let (status, body, _) = call(
        e2e.router(),
        "PUT",
        "/api/v3/library/albums/e2e-dummy-id/edition",
        &tris_headers,
        Some(json!({"release_mbid": "e2e-dummy-id"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_code(&body), "RELEASE_MBID_INVALID");

    // A custom edition and "leave unmanaged" are an admin's word.
    for (path, body) in [
        (
            "/api/v3/library/albums/e2e-dummy-id/edition/custom",
            json!({"release_mbid": "e2e-dummy-id", "expected_identity_revision": 0}),
        ),
        (
            "/api/v3/library/albums/e2e-dummy-id/edition/leave-unmanaged",
            json!({"expected_identity_revision": 0}),
        ),
    ] {
        let (status, body, _) = call(e2e.router(), "POST", path, &tris_headers, Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }

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

/// The app with its limiter's clock held, so no bucket refills while a
/// test counts refusals, however slowly the host runs the requests.
fn held_clock_router(e2e: &E2e) -> Router {
    let state = e2e.state();
    state.auth.limits.hold_clock();
    create_app(state)
}

/// One request from a given client address (as `ConnectInfo` would carry),
/// asked again while the app answers busy.
async fn call_from(app: Router, peer: &str, uri: &str, body: Option<Value>) -> StatusCode {
    crate::common::through_busy(|| call_from_once(app.clone(), peer, uri, body.clone()))
        .await
        .0
}

/// [`call_from`] answered as it came.
async fn call_from_once(
    app: Router,
    peer: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, ()) {
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
    let response = app.oneshot(request).await.expect("router responds");
    (response.status(), response.headers().clone(), ())
}

#[tokio::test]
async fn rate_limits_key_by_client_and_username() {
    let e2e = E2e::open("rate-limits").await;
    let app = held_clock_router(&e2e);

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

    // One address hammering a username hits its own wall (burst 5) and
    // leaves the name open to everyone else. Two addresses in one IPv6 /64
    // are that one client.
    let guess = |peer: &'static str| {
        call_from(
            app.clone(),
            peer,
            "/api/v3/auth/login",
            Some(json!({"username": "victim", "password": "wrong-password-123"})),
        )
    };
    // Each asked once: a guess answered busy has already spent its token.
    let attempts = (0..8).map(|n| {
        let peer = if n % 2 == 0 {
            "[2001:db8:1:2::1]:1"
        } else {
            "[2001:db8:1:2:aaaa::9]:1"
        };
        call_from_once(
            app.clone(),
            peer,
            "/api/v3/auth/login",
            Some(json!({"username": "victim", "password": "wrong-password-123"})),
        )
    });
    let statuses: Vec<_> = futures_util::future::join_all(attempts)
        .await
        .into_iter()
        .map(|(status, _, ())| status)
        .collect();
    let limited = statuses
        .iter()
        .filter(|status| **status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(limited, 3, "{statuses:?}");
    assert_eq!(
        guess("[2001:db8:1:3::1]:1").await,
        StatusCode::UNAUTHORIZED,
        "another client can still try the same username"
    );
}

#[tokio::test]
async fn login_guesses_from_fresh_addresses_meet_the_username_wall() {
    let e2e = E2e::open("username-wall").await;
    let app = e2e.router();

    // No address repeats, so no per-client bucket ever fills: only the
    // username's own budget (burst 50) can refuse these.
    let peers: Vec<String> = (0..60).map(|n| format!("203.0.113.{n}:1")).collect();
    let attempts = peers.iter().map(|peer| {
        call_from(
            app.clone(),
            peer,
            "/api/v3/auth/login",
            Some(json!({"username": "victim", "password": "wrong-password-123"})),
        )
    });
    let statuses = futures_util::future::join_all(attempts).await;
    // The budget refills on the real clock, so the count of refusals moves
    // with how fast the attempts land. The wall is that some are refused.
    let limited = statuses
        .iter()
        .filter(|status| **status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert!(limited >= 5, "{statuses:?}");
    assert!(
        statuses.iter().all(|status| matches!(
            *status,
            StatusCode::UNAUTHORIZED | StatusCode::TOO_MANY_REQUESTS
        )),
        "{statuses:?}"
    );
}

#[tokio::test]
async fn failed_authentications_are_throttled_per_client() {
    let e2e = E2e::open("auth-failures").await;
    let app = held_clock_router(&e2e);

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
        .db()
        // A rename re-parses the whole schema, which outruns the request
        // budget on a busy host; the job budget fits it.
        .write(
            droppedneedle_kernel::db::Lane::Background,
            "test.break",
            |tx| {
                tx.execute_batch("ALTER TABLE auth_providers RENAME TO auth_providers_gone")?;
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            },
        )
        .await
        .expect("table renames");
    let (status, body, _) =
        login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
}

#[tokio::test]
async fn sign_in_waits_out_a_full_read_pool_as_busy_not_a_fault() {
    // A short checkout limit, so the busy answers come without the real
    // five-second wait.
    let e2e = E2e::open_with("pool-full", |config| {
        config.with_read_wait(Duration::from_millis(250))
    })
    .await;
    let (_, token) = setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    let auth = bearer(token.as_str().expect("setup token"));
    // Every reader of both read pools checked out: the stand-in for a host
    // too busy to hand one over before the checkout limit.
    let readers = droppedneedle_kernel::db::MAX_READERS as usize;
    let mut held = Vec::new();
    for _ in 0..readers {
        held.push(e2e.pool().acquire().await.expect("reader checks out"));
    }
    let started = Arc::new(std::sync::Barrier::new(readers + 1));
    let release = Arc::new(std::sync::Barrier::new(readers + 1));
    let mut holders = Vec::new();
    for _ in 0..readers {
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        let db = e2e.db().clone();
        holders.push(tokio::spawn(async move {
            db.read("test.hold", move |_| {
                started.wait();
                release.wait();
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            })
            .await
        }));
    }
    tokio::task::spawn_blocking(move || started.wait())
        .await
        .expect("readers check out");
    let headers = [("authorization", auth.as_str())];
    let setup = json!({"username": "second", "password": "second-password-12"});
    let sign_in = json!({
        "username": "e2e-owner",
        "password": "e2e-owner-password-1",
        "transport": "bearer",
    });
    let (signed_in, gated, set_up) = tokio::join!(
        call_once(
            e2e.router(),
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(sign_in)
        ),
        call_once(e2e.router(), "GET", "/api/v3/me", &headers, None),
        call_once(e2e.router(), "POST", "/api/v3/auth/setup", &[], Some(setup)),
    );
    for (status, body, headers) in [signed_in, gated, set_up] {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(
            headers.get("retry-after").map(|v| v.as_bytes()),
            Some(&b"1"[..])
        );
        assert_eq!(body["error"]["code"], "SERVER_BUSY", "{body}");
        assert!(body["error"]["details"]["error_id"].is_string(), "{body}");
    }
    drop(held);
    tokio::task::spawn_blocking(move || release.wait())
        .await
        .expect("readers release");
    for holder in holders {
        holder.await.expect("holder joins").expect("held read ends");
    }
    let (status, body, _) =
        login(e2e.router(), "e2e-owner", "e2e-owner-password-1", "bearer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn cookie_mutation_needs_the_exact_origin() {
    let e2e = E2e::open("csrf-origin").await;
    let (_, session) =
        setup_admin(e2e.router(), "e2e-owner", "e2e-owner-password-1", "cookie").await;
    let jar = cookie(session.as_str().expect("setup cookie"));
    // A bodiless POST is the one mutation a plain cross-origin form can send.
    let logout_all = |extra: &[(&'static str, &'static str)]| {
        let mut headers = vec![("cookie", jar.clone())];
        headers.extend(
            extra
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        );
        let app = e2e.router();
        async move {
            let headers: Vec<(&str, &str)> = headers
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .collect();
            call_status(app, "POST", "/api/v3/auth/logout-all", &headers).await
        }
    };
    // Another app on the same host shares the cookie and counts as
    // same-site, so only the full origin tells the two apart.
    let foreign: [&[(&str, &str)]; 3] = [
        &[("origin", "http://e2e.test:8096")],
        &[("origin", "https://e2e.test")],
        &[
            ("origin", "http://e2e.test:8096"),
            ("sec-fetch-site", "same-site"),
        ],
    ];
    for headers in foreign {
        assert_eq!(
            logout_all(headers).await,
            StatusCode::FORBIDDEN,
            "{headers:?}"
        );
    }
    assert_eq!(
        logout_all(&[("origin", "http://e2e.test:80")]).await,
        StatusCode::NO_CONTENT,
        "a spelled-out default port is the same origin"
    );
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
