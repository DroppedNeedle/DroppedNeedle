//! The production graph: `bootstrap::build_with` on an empty data
//! directory, served over real TCP, then stopped through
//! `bootstrap::serve`. Every outside address is closed or a local listener.

use std::{
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use axum::{body::Body, routing::get};
use droppedneedle::bootstrap;
use droppedneedle_kernel::{
    config::AppConfig,
    db::{DbConfig, open_runtime},
    runtime_config::{
        ConfigStore, Crypto,
        sections::{PublicAddress, SecuritySettings},
    },
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower::ServiceExt as _;

use crate::common::{ScratchDir, closed_outbound, outbound_at};

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("boot-{name}"))
}

/// Config rooted at `dir`, with breach screening off so setup never
/// dials the network.
fn config_in(dir: &Path) -> AppConfig {
    let config = AppConfig::with_root(0, dir);
    let config_dir = config.config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    let store = ConfigStore::open(
        &config.config_file,
        Crypto::load_or_generate(&config_dir).unwrap(),
    )
    .unwrap();
    let mut security: SecuritySettings = store.get().unwrap();
    security.hibp_check = false;
    store.save(security).unwrap();
    config
}

/// The settings file of a server that is not running.
fn stored_settings(config: &AppConfig) -> ConfigStore {
    ConfigStore::open(
        &config.config_file,
        Crypto::load_or_generate(&config.config_dir()).unwrap(),
    )
    .unwrap()
}

/// The production graph with nothing listening at any outside address.
async fn build(
    config: AppConfig,
) -> Result<(axum::Router, bootstrap::Background), bootstrap::BootError> {
    let built = bootstrap::build_with(config, closed_outbound()).await?;
    Ok((built.router, built.background))
}

struct Running {
    address: SocketAddr,
    stop: oneshot::Sender<()>,
    server: JoinHandle<Result<(), bootstrap::ServeError>>,
}

async fn start(router: axum::Router, background: bootstrap::Background) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, signal) = oneshot::channel::<()>();
    let server = tokio::spawn(bootstrap::serve(listener, router, background, async {
        let _ = signal.await;
    }));
    Running {
        address,
        stop,
        server,
    }
}

impl Running {
    async fn shut_down(self) {
        let _ = self.stop.send(());
        let outcome = tokio::time::timeout(Duration::from_secs(30), self.server)
            .await
            .expect("shutdown is bounded")
            .unwrap();
        assert!(outcome.is_ok(), "{outcome:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_graph_boots_on_an_empty_dir_and_answers() {
    let dir = scratch("graph");
    let (router, background) = build(config_in(&dir)).await.unwrap();
    let running = start(router, background).await;
    let client = reqwest::Client::new();

    let health = client
        .get(format!("http://{}/health", running.address))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert!(health.headers().contains_key("x-request-id"));
    let status = client
        .get(format!(
            "http://{}/api/v3/auth/setup/status",
            running.address
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["setup_required"], true, "{status}");
    running.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_one_version_behind_is_backed_up_before_migrating() {
    let dir = scratch("upgrade");
    let db_path = dir.join("cache").join("library.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let all = &droppedneedle_kernel::schema::MIGRATOR;
    let keep = all.migrations.len() - 1;
    // Migration numbers can skip, so the stamp one behind is the version of
    // the last migration kept, not latest minus one.
    let behind = all.migrations[keep - 1].version;
    let older = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(all.migrations[..keep].to_vec()),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    older.run(&pool).await.unwrap();
    pool.close().await;

    let runtime = open_runtime(&DbConfig::new(&db_path)).await.unwrap();
    let backups: Vec<PathBuf> = std::fs::read_dir(dir.join("cache").join("backups"))
        .expect("the backup sits next to the database")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let stamp: i64 = rusqlite::Connection::open(&backups[0])
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stamp, behind, "the backup holds the pre-upgrade schema");
    runtime.shutdown().await;
}

/// A running offline import holds the data lock exclusively: the server
/// refuses to boot before it opens, backs up or migrates the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_refuses_while_the_offline_tool_holds_the_database() {
    let dir = scratch("locked");
    let config = config_in(&dir);
    let db_path = config.library_db_path.clone();
    let _import = droppedneedle_kernel::db::datalock::DataLock::exclusive(&db_path).unwrap();
    let refused = build(config).await;
    assert!(
        matches!(refused, Err(bootstrap::BootError::DataLock(_))),
        "boot must refuse a held database"
    );
    assert!(!db_path.exists(), "the database was opened under the lock");
}

/// A v2 data directory is refused before the database is opened, backed
/// up or migrated: v3's migrations would otherwise run over the v2
/// database. The refusal names the command that upgrades it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_refuses_a_v2_data_directory_and_leaves_it_alone() {
    let dir = scratch("v2-dir");
    droppedneedle::tooling::fixture::build_v2_fixture(&dir).expect("fixture builds");
    let db_path = dir.join("cache").join("library.db");
    let config_path = dir.join("config").join("config.json");
    let database = std::fs::read(&db_path).unwrap();
    let settings = std::fs::read(&config_path).unwrap();

    let Err(refused) = build(AppConfig::with_root(0, &dir)).await else {
        panic!("boot must refuse a v2 data directory");
    };
    assert!(
        matches!(
            refused,
            bootstrap::BootError::DataDir(droppedneedle::bootstrap::data_dirs::BootRefusal::V2)
        ),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("droppedneedle-tool upgrade"),
        "{refused}"
    );
    assert_eq!(std::fs::read(&db_path).unwrap(), database);
    assert_eq!(std::fs::read(&config_path).unwrap(), settings);
    assert!(
        !dir.join("cache").join("backups").exists(),
        "a backup was taken"
    );
    assert!(
        !dir.join("config").join("data_enc.key").exists(),
        "a v3 key was written into the v2 config folder"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_behind_a_trusted_tls_proxy_sets_a_secure_cookie() {
    let dir = scratch("proxy");
    let (router, background) = build(config_in(&dir)).await.unwrap();
    let running = start(router, background).await;
    let client = reqwest::Client::new();
    let credentials = serde_json::json!({"username": "admin", "password": "correct horse battery"});

    let setup = client
        .post(format!("http://{}/api/v3/auth/setup", running.address))
        .header("content-type", "application/json")
        .body(credentials.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(setup.status(), 201);
    let plain = setup.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(!plain.contains("Secure"), "plain HTTP: {plain}");

    // The test client is a loopback peer, trusted by default.
    let login = client
        .post(format!("http://{}/api/v3/auth/login", running.address))
        .header("x-forwarded-proto", "https")
        .header("content-type", "application/json")
        .body(credentials.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("; Secure"), "{cookie}");
    running.shut_down().await;
}

/// A proxy that hides HTTPS sends no `X-Forwarded-Proto`, so the server
/// takes every sign-in for plain HTTP. Once the admin states the `https`
/// address, the answer to a sign-in from a page there sets a `Secure`
/// cookie, and the answer to a sign-in from anywhere else sets the cookie
/// it always did.
///
/// This is the server's side only. The two addresses share a host name, so
/// they share a cookie, and a browser that holds the `Secure` one drops the
/// plain cookie the http address is answered with: it cannot sign in there
/// until it signs out at the https address.
///
/// First-run setup stands for the other places that set the cookie: the
/// address is stated from the host before the first boot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_to_a_login_from_a_stated_https_address_sets_a_secure_cookie() {
    const HTTPS_PAGE: &str = "https://music.test";
    const HTTP_PAGE: &str = "http://music.test:8080";
    const ADMIN: &str = r#"{"username": "admin", "password": "correct horse battery"}"#;
    let dir = scratch("stated-https");
    let config = config_in(&dir);
    stored_settings(&config)
        .save(PublicAddress {
            origins: vec![HTTPS_PAGE.to_owned()],
        })
        .unwrap();
    let (router, background) = build(config).await.unwrap();
    let running = start(router, background).await;
    let direct = format!("http://{}", running.address);
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{direct}/api/v3/auth/setup"))
        .header("origin", HTTPS_PAGE)
        .header("content-type", "application/json")
        .body(ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(setup.status(), 201);
    let cookie = setup.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("; Secure"), "setup: {cookie}");
    let admin = cookie.split(';').next().unwrap().to_owned();
    let state = |origins: &'static [&'static str]| {
        let save = client
            .put(format!("{direct}/api/v3/settings/public-address"))
            .header("origin", &direct)
            .header("cookie", &admin)
            .header("content-type", "application/json")
            .body(serde_json::json!({"origins": origins}).to_string());
        async move { assert_eq!(save.send().await.unwrap().status(), 200) }
    };
    let secure_from = |origin: Option<&'static str>| {
        let mut login = client
            .post(format!("{direct}/api/v3/auth/login"))
            .header("content-type", "application/json")
            .body(ADMIN);
        if let Some(origin) = origin {
            login = login.header("origin", origin);
        }
        async move {
            let login = login.send().await.unwrap();
            assert_eq!(login.status(), 200, "{origin:?}");
            let cookie = login.headers()["set-cookie"].to_str().unwrap().to_owned();
            assert!(cookie.contains("HttpOnly; SameSite=Lax"), "{cookie}");
            cookie.contains("; Secure")
        }
    };

    state(&[]).await;
    assert!(!secure_from(Some(HTTPS_PAGE)).await, "nothing stated");
    state(&[HTTPS_PAGE, HTTP_PAGE]).await;

    assert!(secure_from(Some(HTTPS_PAGE)).await);
    // A browser on a plain-HTTP page would throw a `Secure` cookie away, so
    // the answer never carries one there.
    assert!(!secure_from(Some(HTTP_PAGE)).await);
    assert!(!secure_from(Some("https://other.test")).await);
    assert!(!secure_from(None).await);
    running.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_open_stream_does_not_keep_background_loops_running() {
    let dir = scratch("shutdown");
    let mut config = config_in(&dir);
    config.shutdown_grace = Duration::from_secs(2);
    let (router, mut background) = build(config).await.unwrap();
    let mut stop = background.stop_signal();
    let (stopped_tx, stopped_rx) = oneshot::channel();
    background.push(
        "probe",
        tokio::spawn(async move {
            let _ = stop.wait_for(|stopped| *stopped).await;
            let _ = stopped_tx.send(Instant::now());
        }),
    );
    // One chunk, then a body that never ends.
    let endless = || async {
        let first = futures_util::stream::once(async {
            Ok::<_, Infallible>(axum::body::Bytes::from_static(b"x"))
        });
        Body::from_stream(futures_util::StreamExt::chain(
            first,
            futures_util::stream::pending(),
        ))
    };
    let running = start(router.route("/endless", get(endless)), background).await;
    let mut stream = reqwest::get(format!("http://{}/endless", running.address))
        .await
        .unwrap();
    assert!(stream.chunk().await.unwrap().is_some());

    let signalled = Instant::now();
    let shutdown = tokio::spawn(running.shut_down());
    let stopped_at = tokio::time::timeout(Duration::from_secs(1), stopped_rx)
        .await
        .expect("loops hear the signal while the stream is still open")
        .unwrap();
    assert!(stopped_at.duration_since(signalled) < Duration::from_secs(1));
    shutdown.await.unwrap();
    drop(stream);
}

/// A listed reverse proxy (loopback is trusted by default).
const PROXY: &str = "127.0.0.1:40000";
/// Any other client.
const STRANGER: &str = "203.0.113.7:40000";

/// Answer `request` as if `peer` had sent it.
async fn answer_from(
    router: &axum::Router,
    peer: &str,
    mut request: axum::http::Request<Body>,
) -> axum::response::Response {
    let peer: SocketAddr = peer.parse().unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    router.clone().oneshot(request).await.unwrap()
}

/// The `Strict-Transport-Security` value on the answer to a GET from
/// `peer`. `forwarded_https` adds the header a TLS proxy sets.
async fn hsts_header(
    router: &axum::Router,
    peer: &str,
    forwarded_https: bool,
    uri: &str,
) -> Option<String> {
    let mut request = axum::http::Request::builder()
        .uri(uri)
        .header("host", "music.test");
    if forwarded_https {
        request = request.header("x-forwarded-proto", "https");
    }
    let response = answer_from(router, peer, request.body(Body::empty()).unwrap()).await;
    response
        .headers()
        .get("strict-transport-security")
        .map(|value| value.to_str().unwrap().to_owned())
}

/// Save the HSTS fields as the signed-in admin, through the proxy. Returns
/// the status and the `Strict-Transport-Security` value of that response.
async fn save_hsts(
    router: &axum::Router,
    cookie: &str,
    max_age: i64,
    include_subdomains: bool,
    preload: bool,
) -> (u16, Option<String>) {
    let body = serde_json::json!({
        "hibp_check": false,
        "hsts_max_age": max_age,
        "hsts_include_subdomains": include_subdomains,
        "hsts_preload": preload,
    });
    let request = axum::http::Request::builder()
        .method("PUT")
        .uri("/api/v3/settings/security")
        .header("host", "music.test")
        .header("x-forwarded-proto", "https")
        .header("sec-fetch-site", "same-origin")
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = answer_from(router, PROXY, request).await;
    let header = response
        .headers()
        .get("strict-transport-security")
        .map(|value| value.to_str().unwrap().to_owned());
    (response.status().as_u16(), header)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hsts_is_sent_only_over_https_and_only_while_the_setting_is_on() {
    let dir = scratch("hsts");
    let config = config_in(&dir);
    std::fs::create_dir_all(config.static_dir.join("_app")).unwrap();
    std::fs::write(config.static_dir.join("index.html"), "<html></html>").unwrap();
    std::fs::write(config.static_dir.join("_app/app.js"), "console.log(1);").unwrap();
    let (router, background) = build(config).await.unwrap();
    let running = start(router.clone(), background).await;

    let setup = reqwest::Client::new()
        .post(format!("http://{}/api/v3/auth/setup", running.address))
        .header("content-type", "application/json")
        .body(r#"{"username": "admin", "password": "correct horse battery"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(setup.status(), 201);
    let cookie = setup.headers()["set-cookie"].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_owned();

    // An API answer, a web UI file, the SPA page, a 401, a 404 and a compat
    // path.
    let everywhere = [
        "/api/v3/auth/setup/status",
        "/_app/app.js",
        "/library",
        "/api/v3/auth/sessions",
        "/api/v3/no-such-route",
        "/subsonic/rest/ping",
    ];

    for uri in everywhere {
        assert_eq!(
            hsts_header(&router, PROXY, true, uri).await,
            None,
            "off: {uri}"
        );
    }

    let (status, _) = save_hsts(&router, &cookie, 31_536_000, false, false).await;
    assert_eq!(status, 200);
    for uri in everywhere {
        assert_eq!(
            hsts_header(&router, PROXY, true, uri).await.as_deref(),
            Some("max-age=31536000"),
            "{uri}"
        );
        assert_eq!(
            hsts_header(&router, PROXY, false, uri).await,
            None,
            "plain HTTP: {uri}"
        );
        assert_eq!(
            hsts_header(&router, STRANGER, true, uri).await,
            None,
            "forwarded scheme from an unlisted peer: {uri}"
        );
    }

    // Refused at save, so the header keeps its last good value: a negative
    // max-age, one above two years, and preload without subdomains.
    for (max_age, include_subdomains, preload) in [
        (-1, false, false),
        (63_072_001, false, false),
        (300, false, true),
    ] {
        let (status, _) = save_hsts(&router, &cookie, max_age, include_subdomains, preload).await;
        assert_eq!(status, 400, "{max_age} {include_subdomains} {preload}");
    }
    assert_eq!(
        hsts_header(&router, PROXY, true, "/library")
            .await
            .as_deref(),
        Some("max-age=31536000")
    );

    let (status, _) = save_hsts(&router, &cookie, 300, true, true).await;
    assert_eq!(status, 200);
    assert_eq!(
        hsts_header(&router, PROXY, true, "/library")
            .await
            .as_deref(),
        Some("max-age=300; includeSubDomains; preload")
    );

    // The save that turns it off is itself answered without the header.
    assert_eq!(
        save_hsts(&router, &cookie, 0, false, false).await,
        (200, None)
    );
    for uri in everywhere {
        assert_eq!(
            hsts_header(&router, PROXY, true, uri).await,
            None,
            "off again: {uri}"
        );
    }
    // A live clone would hold shutdown for its whole grace period.
    drop(router);
    running.shut_down().await;
}

/// The two content-security headers on the answer to a GET: the enforced
/// policy and the report-only one.
async fn policies(router: &axum::Router, uri: &str) -> (Option<String>, Option<String>) {
    let request = axum::http::Request::builder()
        .uri(uri)
        .header("host", "music.test")
        .body(Body::empty())
        .unwrap();
    let response = answer_from(router, STRANGER, request).await;
    assert_eq!(
        response.headers()["x-content-type-options"],
        "nosniff",
        "{uri}"
    );
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    (
        header("content-security-policy"),
        header("content-security-policy-report-only"),
    )
}

/// A web UI build whose page holds one inline script.
fn web_ui_in(config: &AppConfig) {
    std::fs::create_dir_all(&config.static_dir).unwrap();
    std::fs::write(
        config.static_dir.join("index.html"),
        "<div><script>boot()</script></div>",
    )
    .unwrap();
}

/// The Security settings choose how the page's policy is sent and who may
/// frame the app, from the next request on. What goes into the policy is an
/// origin and nothing else: text that could add a source or a directive is
/// refused at save.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_page_policy_follows_the_saved_mode_and_framing_list() {
    const PAGE: &str = "/library";
    const API: &str = "/api/v3/auth/setup/status";
    const BARE: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";
    const FRAMING_ONLY: &str = "frame-src 'self' https://www.youtube.com; frame-ancestors 'none'";
    // sha256 of `boot()`.
    const BOOT: &str = "'sha256-MeZS89WlF0u+o0hCvHTBt4q1WHU+U+sJKgbdRUc36mY='";
    let dir = scratch("page-policy");
    let config = config_in(&dir);
    web_ui_in(&config);
    let (router, background) = build(config).await.unwrap();
    let running = start(router.clone(), background).await;
    let cookie = admin_cookie(running.address).await;
    let save = |body: serde_json::Value| {
        save_as_browser(
            &router,
            STRANGER,
            &cookie,
            "http://music.test",
            &[],
            ("security", body),
        )
    };
    let framing = |policy: &str| {
        let (_, sources) = policy.rsplit_once("frame-ancestors ").unwrap();
        sources.to_owned()
    };

    // As installed: reported, except who may frame the page and where its
    // frames go, which are enforced: nobody may frame the app.
    let (enforced, reported) = policies(&router, PAGE).await;
    let reported = reported.expect("the page reports");
    assert_eq!(enforced.as_deref(), Some(FRAMING_ONLY));
    assert!(reported.contains(BOOT), "{reported}");
    assert_eq!(framing(&reported), "'none'");
    assert_eq!(policies(&router, API).await, (Some(BARE.to_owned()), None));
    // What a browser that has the page cached asks: the status, and the
    // tag it would cache next.
    let revalidate = |etag: String| async {
        let request = axum::http::Request::builder()
            .uri(PAGE)
            .header("host", "music.test")
            .header("if-none-match", etag)
            .body(Body::empty())
            .unwrap();
        let response = answer_from(&router, STRANGER, request).await;
        let etag = response.headers()["etag"].to_str().unwrap().to_owned();
        (response.status().as_u16(), etag)
    };
    let (code, cached_reporting) = revalidate("\"none\"".to_owned()).await;
    assert_eq!(code, 200);
    assert_eq!(revalidate(cached_reporting.clone()).await.0, 304);

    let (code, body) = save(serde_json::json!({
        "hibp_check": false,
        "content_security_policy": "enforce",
        "frame_ancestors": ["HTTPS://Home.Example.com:443/", "http://192.168.1.5:7575"],
    }))
    .await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(
        body["frame_ancestors"],
        serde_json::json!(["https://home.example.com", "http://192.168.1.5:7575"])
    );
    let (enforced, reported) = policies(&router, PAGE).await;
    let enforced = enforced.expect("the page enforces");
    assert_eq!(reported, None);
    assert!(enforced.contains(BOOT), "{enforced}");
    assert_eq!(
        framing(&enforced),
        "https://home.example.com http://192.168.1.5:7575"
    );
    // A browser keeps a cached page's headers that a 304 leaves out, so the
    // page it cached under the old settings is sent again in full.
    let (code, cached_enforcing) = revalidate(cached_reporting.clone()).await;
    assert_eq!(code, 200);
    assert_ne!(cached_enforcing, cached_reporting);
    assert_eq!(revalidate(cached_enforcing.clone()).await.0, 304);
    // Only the page can be framed: every other answer still refuses.
    assert_eq!(policies(&router, API).await, (Some(BARE.to_owned()), None));

    // Refused, with the field named, and the policy keeps its last value.
    for bad in [
        "https://home.example.com/dash",
        "https://home.example.com;script-src",
        "https://home.example.com; script-src *",
        "https://home.example.com,evil.test",
        "https://*.example.com",
        "https://%2A.example.com",
        "https://'self'",
        "*",
        "'self'",
        "http://[::1]:7575",
    ] {
        let (code, body) = save(serde_json::json!({
            "hibp_check": false,
            "content_security_policy": "enforce",
            "frame_ancestors": [bad],
        }))
        .await;
        assert_eq!(code, 400, "{bad}: {body}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("security_settings.frame_ancestors"),
            "{bad}: {message}"
        );
    }
    let (code, _) = save(serde_json::json!({
        "hibp_check": false,
        "content_security_policy": "loud",
    }))
    .await;
    assert!((400..500).contains(&code), "{code}");
    assert_eq!(policies(&router, PAGE).await, (Some(enforced), None));

    let (code, body) = save(serde_json::json!({
        "hibp_check": false,
        "content_security_policy": "off",
        "frame_ancestors": ["https://home.example.com"],
    }))
    .await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(policies(&router, PAGE).await, (None, None));
    assert_eq!(policies(&router, API).await, (None, None));
    assert_eq!(revalidate(cached_enforcing).await.0, 200);

    // A save that leaves both out puts them back as installed.
    let (code, body) = save(serde_json::json!({"hibp_check": false})).await;
    assert_eq!(code, 200, "{body}");
    let (enforced, reported) = policies(&router, PAGE).await;
    assert_eq!(enforced.as_deref(), Some(FRAMING_ONLY));
    assert_eq!(framing(&reported.expect("the page reports")), "'none'");

    drop(router);
    running.shut_down().await;
}

/// An admin whose browser shows a blank page under the policy changes its
/// mode from the host, with the server stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_policy_mode_set_from_the_host_is_served_at_the_next_boot() {
    use droppedneedle::tooling::host_settings::{HostSettingsError, set_content_security_policy};
    use droppedneedle_kernel::runtime_config::sections::ContentSecurityPolicyMode as Mode;

    let dir = scratch("page-policy-host");
    let config = config_in(&dir);
    web_ui_in(&config);
    let dirs = droppedneedle::bootstrap::data_dirs::DataDirs::from_config(&config);
    let config_file = config.config_file.clone();

    let (router, background) = build(config.clone()).await.unwrap();
    let running = start(router.clone(), background).await;
    let (enforced, reported) = policies(&router, "/").await;
    assert!(reported.is_some() && !enforced.unwrap().contains("script-src"));
    // A running server holds the settings in memory: refused, nothing
    // written.
    let before = std::fs::read(&config_file).unwrap();
    assert!(matches!(
        set_content_security_policy(&dirs, Mode::Off),
        Err(HostSettingsError::ServerRunning(_))
    ));
    assert_eq!(std::fs::read(&config_file).unwrap(), before);
    drop(router);
    running.shut_down().await;

    // Only that field changes.
    assert_eq!(
        set_content_security_policy(&dirs, Mode::Enforce).unwrap(),
        Mode::Enforce
    );
    let stored: SecuritySettings = stored_settings(&config).get().unwrap();
    assert_eq!(
        stored,
        SecuritySettings {
            hibp_check: false,
            content_security_policy: Mode::Enforce,
            ..SecuritySettings::default()
        }
    );
    let (router, background) = build(config.clone()).await.unwrap();
    let running = start(router.clone(), background).await;
    let (enforced, reported) = policies(&router, "/").await;
    assert!(enforced.is_some() && reported.is_none());
    drop(router);
    running.shut_down().await;

    assert_eq!(
        set_content_security_policy(&dirs, Mode::Off).unwrap(),
        Mode::Off
    );
    let (router, background) = build(config).await.unwrap();
    let running = start(router.clone(), background).await;
    assert_eq!(policies(&router, "/").await, (None, None));
    drop(router);
    running.shut_down().await;
}

/// Set up the admin over TCP and return the session cookie.
async fn admin_cookie(address: SocketAddr) -> String {
    let setup = reqwest::Client::new()
        .post(format!("http://{address}/api/v3/auth/setup"))
        .header("content-type", "application/json")
        .body(r#"{"username": "admin", "password": "correct horse battery"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(setup.status(), 201);
    let cookie = setup.headers()["set-cookie"].to_str().unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

/// A settings save by the signed-in admin as a browser with no
/// `Sec-Fetch-Site` sends it: to `Host: music.test`, from a page at
/// `origin`, with any `extra` headers. Returns the status and the body.
async fn save_as_browser(
    router: &axum::Router,
    peer: &str,
    cookie: &str,
    origin: &str,
    extra: &[(&str, &str)],
    (path, body): (&str, serde_json::Value),
) -> (u16, serde_json::Value) {
    let mut request = axum::http::Request::builder()
        .method("PUT")
        .uri(format!("/api/v3/settings/{path}"))
        .header("host", "music.test")
        .header("origin", origin)
        .header("cookie", cookie)
        .header("content-type", "application/json");
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let request = request.body(Body::from(body.to_string())).unwrap();
    let response = answer_from(router, peer, request).await;
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// An unrelated save, standing in for every write the web UI makes.
fn any_save() -> (&'static str, serde_json::Value) {
    ("security", serde_json::json!({"hibp_check": false}))
}

fn public_address(origins: &[&str]) -> (&'static str, serde_json::Value) {
    ("public-address", serde_json::json!({"origins": origins}))
}

/// A proxy drops the port from `Host`: the browser is on
/// `http://music.test:8080` and the server sees itself at
/// `http://music.test`. Every save is refused until the admin states the
/// address, and nothing but that saved address widens the check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stated_public_address_is_the_only_thing_that_widens_the_origin_check() {
    const PAGE: &str = "http://music.test:8080";
    const OTHER_APP: &str = "http://music.test:8096";
    let dir = scratch("public-address");
    let (router, background) = build(config_in(&dir)).await.unwrap();
    let running = start(router.clone(), background).await;
    let cookie = admin_cookie(running.address).await;
    let save = |peer: &'static str,
                origin: &'static str,
                extra: &'static [(&'static str, &'static str)],
                what: (&'static str, serde_json::Value)| {
        let (router, cookie) = (router.clone(), cookie.clone());
        async move { save_as_browser(&router, peer, &cookie, origin, extra, what).await }
    };
    let status = |peer, origin, extra| {
        let saved = save(peer, origin, extra, any_save());
        async move { saved.await.0 }
    };

    assert_eq!(status(PROXY, PAGE, &[]).await, 403);

    // Refused from the page it would allow: the admin opens the app at the
    // address the server works out, and saves it there.
    let stated = public_address(&["HTTP://Music.Test:8080/"]);
    assert_eq!(save(PROXY, PAGE, &[], stated.clone()).await.0, 403);
    let (code, body) = save(PROXY, "http://music.test", &[], stated).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["origins"], serde_json::json!([PAGE]));

    assert_eq!(status(PROXY, PAGE, &[]).await, 200);
    assert_eq!(status(PROXY, "http://music.test", &[]).await, 200);
    // Another app on the same host, another scheme, a look-alike host.
    assert_eq!(status(PROXY, OTHER_APP, &[]).await, 403);
    assert_eq!(status(PROXY, "https://music.test:8080", &[]).await, 403);
    assert_eq!(
        status(PROXY, "http://music.test.evil.test:8080", &[]).await,
        403
    );
    // Forwarded headers from a peer that is not a trusted proxy change
    // nothing, and neither does a `Sec-Fetch-Site` short of same-origin.
    let forged: &[(&str, &str)] = &[
        ("x-forwarded-host", "music.test:8096"),
        ("x-forwarded-proto", "https"),
        ("sec-fetch-site", "same-site"),
    ];
    assert_eq!(status(STRANGER, OTHER_APP, forged).await, 403);
    assert_eq!(
        status(STRANGER, "https://music.test:8096", forged).await,
        403
    );
    assert_eq!(status(STRANGER, PAGE, forged).await, 200);

    // A bad entry is refused by name and the list stays as it was. The
    // unrelated saves above did not clear it either.
    let (code, body) = save(
        PROXY,
        PAGE,
        &[],
        public_address(&[OTHER_APP, "https://music.test/music"]),
    )
    .await;
    assert_eq!(code, 400, "{body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("\"https://music.test/music\"") && message.contains("base path"),
        "{body}"
    );
    assert_eq!(status(PROXY, OTHER_APP, &[]).await, 403);
    assert_eq!(status(PROXY, PAGE, &[]).await, 200);

    // A wildcard is refused however it is spelt, and a body without
    // `origins` is a mistake, never a request to clear the list.
    let spelt_out = public_address(&["https://%2A.music.test"]);
    let (code, body) = save(PROXY, PAGE, &[], spelt_out).await;
    assert_eq!(code, 400, "{body}");
    for body in [
        serde_json::json!({}),
        serde_json::json!({"origin": [OTHER_APP]}),
    ] {
        let (code, body) = save(PROXY, PAGE, &[], ("public-address", body)).await;
        assert_eq!(code, 400, "{body}");
        assert_eq!(body["error"]["code"], "INVALID_INPUT", "{body}");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("origins"), "{body}");
        assert_eq!(status(PROXY, PAGE, &[]).await, 200, "the list was emptied");
    }

    // Cleared from the stated page itself, which is then refused again.
    let (code, body) = save(PROXY, PAGE, &[], public_address(&[])).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["origins"], serde_json::json!([]));
    assert_eq!(status(PROXY, PAGE, &[]).await, 403);
    assert_eq!(status(PROXY, "http://music.test", &[]).await, 200);

    drop(router);
    running.shut_down().await;
}

/// A settings file with no public address section, or `null` in its
/// place, states no address. A section without `origins` is unreadable,
/// never an empty list: a save cannot write one, so it is a hand edit.
#[test]
fn a_stored_public_address_without_origins_is_unreadable_not_empty() {
    let dir = scratch("public-address-stored");
    let config = config_in(&dir);
    let read = |section: Option<serde_json::Value>| {
        let mut file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config.config_file).unwrap()).unwrap();
        let sections = file.as_object_mut().unwrap();
        match section {
            Some(section) => sections.insert("public_address".to_owned(), section),
            None => sections.remove("public_address"),
        };
        std::fs::write(&config.config_file, file.to_string()).unwrap();
        stored_settings(&config).get::<PublicAddress>()
    };
    assert_eq!(read(None).unwrap(), PublicAddress::default());
    assert_eq!(
        read(Some(serde_json::Value::Null)).unwrap(),
        PublicAddress::default()
    );
    assert!(read(Some(serde_json::json!({}))).is_err());
    let kept = read(Some(serde_json::json!({"origins": ["https://music.test"]})));
    assert_eq!(kept.unwrap().origins, ["https://music.test"]);
}

/// An admin who can only reach the app through the proxy states the address
/// from the host, with the server stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_address_set_from_the_host_is_honoured_at_the_next_boot() {
    use droppedneedle::tooling::host_settings::{
        HostSettingsError, public_address as stored, set_public_address,
    };
    const PAGE: &str = "https://music.test";
    let dir = scratch("public-address-host");
    let config = config_in(&dir);
    let dirs = droppedneedle::bootstrap::data_dirs::DataDirs::from_config(&config);
    let config_file = config.config_file.clone();
    let typed = || vec!["HTTPS://Music.Test:443/".to_owned()];

    let (router, background) = build(config.clone()).await.unwrap();
    let running = start(router.clone(), background).await;
    let cookie = admin_cookie(running.address).await;
    let (code, _) = save_as_browser(&router, PROXY, &cookie, PAGE, &[], any_save()).await;
    assert_eq!(code, 403);
    // A running server holds the settings in memory: refused, nothing read
    // or written.
    let before = std::fs::read(&config_file).unwrap();
    assert!(matches!(
        set_public_address(&dirs, typed()),
        Err(HostSettingsError::ServerRunning(_))
    ));
    assert!(matches!(
        stored(&dirs),
        Err(HostSettingsError::ServerRunning(_))
    ));
    assert_eq!(std::fs::read(&config_file).unwrap(), before);
    drop(router);
    running.shut_down().await;

    let before = std::fs::read(&config_file).unwrap();
    let refused = set_public_address(&dirs, vec!["https://music.test/music".to_owned()])
        .expect_err("a path is refused");
    assert!(
        matches!(refused, HostSettingsError::Refused(_))
            && refused.to_string().contains("\"https://music.test/music\""),
        "{refused}"
    );
    assert_eq!(std::fs::read(&config_file).unwrap(), before);
    assert_eq!(stored(&dirs).unwrap(), Vec::<String>::new());

    assert_eq!(set_public_address(&dirs, typed()).unwrap(), [PAGE]);
    assert_eq!(stored(&dirs).unwrap(), [PAGE]);

    let (router, background) = build(config.clone()).await.unwrap();
    let running = start(router.clone(), background).await;
    let (code, body) = save_as_browser(&router, PROXY, &cookie, PAGE, &[], any_save()).await;
    assert_eq!(code, 200, "{body}");
    let (code, _) = save_as_browser(
        &router,
        PROXY,
        &cookie,
        "https://music.test:8443",
        &[],
        any_save(),
    )
    .await;
    assert_eq!(code, 403);
    drop(router);
    running.shut_down().await;

    assert_eq!(
        set_public_address(&dirs, Vec::new()).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(stored(&dirs).unwrap(), Vec::<String>::new());
}

async fn set_up(address: SocketAddr) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("http://{address}/api/v3/auth/setup"))
        .header("content-type", "application/json")
        .body(r#"{"username": "admin", "password": "correct horse battery"}"#)
        .send()
        .await
        .unwrap()
        .status()
}

/// The graph reaches an outside service where `Outbound` says: with breach
/// screening on, setup asks the listener standing in for the breach check.
/// With nothing listening at any address the server still sets up and
/// answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outside_calls_go_where_outbound_points() {
    let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = hits.clone();
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", provider.local_addr().unwrap());
    let stand_in = tokio::spawn(async move {
        let record = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            seen.lock().unwrap().push(uri.path().to_owned());
            async { "" }
        });
        axum::serve(provider, record).await
    });

    let dir = scratch("outbound");
    let bootstrap::Built {
        router, background, ..
    } = bootstrap::build_with(AppConfig::with_root(0, &dir), outbound_at(&origin))
        .await
        .unwrap();
    let running = start(router, background).await;
    assert_eq!(set_up(running.address).await, 201);
    let hits = hits.lock().unwrap().clone();
    assert!(
        hits.iter().any(|path| path.starts_with("/pwned/range/")),
        "{hits:?}"
    );
    running.shut_down().await;
    stand_in.abort();

    // Nothing listens anywhere.
    let dir = scratch("outbound-closed");
    let bootstrap::Built {
        router, background, ..
    } = bootstrap::build_with(AppConfig::with_root(0, &dir), closed_outbound())
        .await
        .unwrap();
    let running = start(router, background).await;
    assert_eq!(set_up(running.address).await, 201);
    let health = reqwest::get(format!("http://{}/health", running.address))
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    running.shut_down().await;
}
