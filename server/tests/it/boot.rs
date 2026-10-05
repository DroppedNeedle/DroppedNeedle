//! Boot-ready brief: the app binds an ephemeral port, answers `/health`
//! over real TCP, and stops cleanly.

use crate::common;

use std::time::Duration;

use droppedneedle::create_app;

#[tokio::test]
async fn boots_serves_health_and_stops() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = create_app(common::hooked_state());

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["status"], "ok");
    assert!(response_headers_seen(&address).await);

    server.abort();
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
}

/// A second request still carries the request id header, proving the
/// middleware wraps live traffic too.
async fn response_headers_seen(address: &std::net::SocketAddr) -> bool {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap();
    response.headers().contains_key("x-request-id")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn database_one_version_behind_is_backed_up_before_migrating() {
    use droppedneedle::db::{DbConfig, open_runtime};

    let dir = std::env::temp_dir().join(format!("dn-boot-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
    let backups: Vec<std::path::PathBuf> = std::fs::read_dir(dir.join("cache").join("backups"))
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
