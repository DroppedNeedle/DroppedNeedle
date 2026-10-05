//! The production graph: `bootstrap::build` on an empty data directory,
//! served over real TCP, then stopped through `bootstrap::serve`.

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

use crate::common::ScratchDir;

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
    let (router, background) = bootstrap::build(config_in(&dir)).await.unwrap();
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
    let older = {
        let all = &droppedneedle::schema::MIGRATOR;
        let keep = all.migrations.len() - 1;
        sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(all.migrations[..keep].to_vec()),
            ..sqlx::migrate::Migrator::DEFAULT
        }
    };
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    older.run(&pool).await.unwrap();
    pool.close().await;
    let behind = droppedneedle::schema::latest_version() - 1;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_behind_a_trusted_tls_proxy_sets_a_secure_cookie() {
    let dir = scratch("proxy");
    let (router, background) = bootstrap::build(config_in(&dir)).await.unwrap();
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
    let (router, mut background) = bootstrap::build(config).await.unwrap();
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
