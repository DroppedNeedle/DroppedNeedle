//! A migrated database for tests, built once per process.
//!
//! Applying every migration to a fresh file is the slow part of opening a
//! test database. The first caller migrates one file with the production
//! migrator and keeps its bytes; every later caller gets its own copy.
//!
//! The bytes are read only after the WAL is checkpointed and the last
//! connection is closed, so they are the whole database and no `-wal`
//! file goes with them. The header still marks the file as WAL, which is
//! what every copy opens in. The template's directory is removed as soon
//! as the bytes are read, so a run leaves nothing behind.

use std::io::Write as _;
use std::path::Path;
use std::sync::OnceLock;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::db::scratch::ScratchDir;

static TEMPLATE: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();

/// Write a private copy of the migrated database to `path`.
///
/// Refuses a path that already exists: a test that reopens its own file
/// must not have it replaced.
pub fn copy_to(path: &Path) -> Result<(), String> {
    let bytes = TEMPLATE.get_or_init(build).as_ref().map_err(Clone::clone)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("database copy at {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("database copy at {}: {error}", path.display()))
}

/// Migrate on a thread of its own, so this works from inside a runtime
/// and from plain synchronous code alike.
fn build() -> Result<Vec<u8>, String> {
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("template runtime: {error}"))?
            .block_on(migrate())
    })
    .join()
    .map_err(|_| "template thread panicked".to_owned())?
}

async fn migrate() -> Result<Vec<u8>, String> {
    let dir = ScratchDir::new("db-template").map_err(|error| error.to_string())?;
    let path = dir.path().join("app.db");
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|error| format!("template open: {error}"))?;
    super::apply_migrations(&pool)
        .await
        .map_err(|error| format!("template migrations: {error}"))?;
    let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&pool)
        .await
        .map_err(|error| format!("template checkpoint: {error}"))?;
    pool.close().await;
    if busy != 0 {
        return Err("template checkpoint was blocked".to_owned());
    }

    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    if std::fs::metadata(&wal).is_ok_and(|meta| meta.len() > 0) {
        return Err("template still has a write-ahead log".to_owned());
    }
    std::fs::read(&path).map_err(|error| format!("template read: {error}"))
}
