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
use droppedneedle::{
    AppConfig, bootstrap,
    db::{DbConfig, open_runtime},
    runtime_config::{ConfigStore, Crypto, sections::SecuritySettings},
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
    let all = &droppedneedle::schema::MIGRATOR;
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
    let _import = droppedneedle::tooling::datalock::DataLock::exclusive(&db_path).unwrap();
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
            bootstrap::BootError::DataDir(droppedneedle::tooling::upgrade::BootRefusal::V2)
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

    // Cleared from the stated page itself, which is then refused again.
    let (code, body) = save(PROXY, PAGE, &[], public_address(&[])).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["origins"], serde_json::json!([]));
    assert_eq!(status(PROXY, PAGE, &[]).await, 403);
    assert_eq!(status(PROXY, "http://music.test", &[]).await, 200);

    drop(router);
    running.shut_down().await;
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
