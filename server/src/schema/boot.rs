//! Boot-time schema checks: migrate, then prove the stamp matches.
//!
//! The runtime calls [`apply_migrations`] before binding its socket; a
//! version mismatch is fatal and names the fix (restore the matching
//! backup), never a silent partial serve.

use sqlx::SqlitePool;
use thiserror::Error;

use super::MIGRATOR;

/// Schema failures: a stale database or a migration that could not apply.
#[derive(Debug, Error)]
pub enum SchemaError {
    /// The database stamp differs from the binary. Fatal at boot.
    #[error(
        "database schema version {found} does not match binary version {expected}; refusing to serve"
    )]
    VersionMismatch {
        /// `PRAGMA user_version` read from the database file.
        found: i64,
        /// Newest embedded migration version.
        expected: i64,
    },
    /// A migration failed to apply.
    #[error("schema migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A schema check query failed.
    #[error("schema check failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Newest embedded migration version.
pub fn latest_version() -> i64 {
    MIGRATOR
        .migrations
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

/// Apply pending migrations, then assert the stamp matches the binary.
pub async fn apply_migrations(pool: &SqlitePool) -> Result<(), SchemaError> {
    MIGRATOR.run(pool).await?;
    assert_migrated(pool).await
}

/// Apply every embedded migration over one rusqlite connection, for
/// scratch test databases that never meet the async pool. Production
/// boots through [`apply_migrations`].
#[cfg(any(test, feature = "test-support"))]
pub fn apply_migrations_blocking(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    for migration in MIGRATOR.migrations.iter() {
        connection.execute_batch(&migration.sql)?;
    }
    Ok(())
}

/// Refuse to serve when `user_version` differs from the binary.
pub async fn assert_migrated(pool: &SqlitePool) -> Result<(), SchemaError> {
    let found: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await?;
    let expected = latest_version();
    if found == expected {
        Ok(())
    } else {
        Err(SchemaError::VersionMismatch { found, expected })
    }
}
