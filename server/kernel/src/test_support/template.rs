//! A migrated database for tests, built once per process.
//!
//! Applying the schema to a fresh file is the slow part of opening a test
//! database. The first caller migrates one file with the production runner
//! and keeps its bytes; every later caller gets its own copy.
//!
//! The bytes are read after the runner's connection closed: as the last
//! connection it checkpoints the WAL and removes it, so the bytes are the
//! whole database and no `-wal` file goes with them. The header still marks
//! the file as WAL, which is what every copy opens in. The template's
//! directory is removed as soon as the bytes are read, so a run leaves
//! nothing behind.

use std::io::Write as _;
use std::path::Path;
use std::sync::OnceLock;

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

fn build() -> Result<Vec<u8>, String> {
    let dir = ScratchDir::new("db-template").map_err(|error| error.to_string())?;
    let path = dir.path().join("app.db");
    crate::db::migrate_file(&path, &crate::schema::CHAIN)
        .map_err(|error| format!("template migrations: {error}"))?;
    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    if std::fs::metadata(&wal).is_ok_and(|meta| meta.len() > 0) {
        return Err("template still has a write-ahead log".to_owned());
    }
    std::fs::read(&path).map_err(|error| format!("template read: {error}"))
}
