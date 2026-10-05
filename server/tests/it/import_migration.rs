//! Migration tests: the import-application migration.
//!
//! The stage plan reserved the 0002 slot for this; 0002 and 0003 landed
//! first, so the import record is migration 0004. Tests cover a fresh
//! migrate plus an upgrade over a database migrated through 0003.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use droppedneedle::schema::{MIGRATOR, apply_migrations, latest_version};

async fn scratch_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

#[tokio::test]
async fn fresh_migrate_creates_import_runs() {
    let pool = scratch_pool().await;
    apply_migrations(&pool).await.unwrap();

    assert!(latest_version() >= 5);
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'import_runs'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(table.as_deref(), Some("import_runs"));
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('import_runs') ORDER BY cid")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        columns,
        vec![
            "id",
            "instance_id",
            "exported_at",
            "exit_code",
            "entity_counts",
            "applied_at"
        ]
    );
    let index: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_import_runs_instance'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(index.as_deref(), Some("idx_import_runs_instance"));
}

async fn migrate_through(pool: &SqlitePool, version: i64) {
    let selected: Vec<_> = MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version <= version)
        .cloned()
        .collect();
    let base = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(selected),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    base.run(pool).await.unwrap();
    let stamped: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(stamped, version);
}

#[tokio::test]
async fn upgrade_from_0001_keeps_data() {
    let pool = scratch_pool().await;
    migrate_through(&pool, 1).await;

    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let name: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "U");
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}

#[tokio::test]
async fn upgrade_over_0003_keeps_data() {
    let pool = scratch_pool().await;
    migrate_through(&pool, 3).await;

    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('u1', 'U', 'user', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, latest_version());
    let name: String = sqlx::query_scalar("SELECT display_name FROM auth_users WHERE id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "U");
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_runs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}
