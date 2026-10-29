//! Boot-time schema checks: migrate, then prove the stamp matches.
//!
//! The runtime calls [`apply_migrations`] before binding its socket; a
//! version mismatch is fatal and names the fix (restore the matching
//! backup), never a silent partial serve.
//!
//! # The runner
//!
//! [`run_migrations`] holds one pool connection for the whole run and keeps
//! sqlx's own checks (a dirty row, a recorded version the binary lacks, a
//! changed checksum). An ordinary migration goes through sqlx's `apply`,
//! which wraps the file and its record in one transaction. sqlx 0.8 reads a
//! file's `-- no-transaction` first line, but its SQLite driver still wraps
//! the file, so the runner runs such a file itself with no transaction
//! around it. It then checks that foreign keys are back on and the stamp is
//! the file's version, and records it exactly as sqlx would. When such a
//! file fails, its connection is closed rather than returned to the pool:
//! closing rolls back its open transaction and its `foreign_keys=OFF`.
//!
//! A no-transaction file commits before it is recorded, so a crash between
//! the two leaves a stamp with no record. At the next boot, for each such
//! file N that `_sqlx_migrations` does not list:
//! - stamp equal to N: the file already committed, so it is recorded
//!   without running again;
//! - stamp below N: it runs;
//! - stamp above N: boot refuses, recording nothing.
//!
//! # Rebuilding a table other tables point at
//!
//! SQLite cannot change a column's type or constraints in place, so a table
//! is rebuilt by copying it. A plain copy inside a transaction deletes or
//! orphans the children of the table it drops. Every step below is
//! load-bearing:
//!
//! 1. The file's first line is `-- no-transaction`. Without it the file runs
//!    inside a transaction, `PRAGMA foreign_keys=OFF` is a silent no-op and
//!    `DROP TABLE` cascades into every child.
//! 2. `PRAGMA foreign_keys=OFF;` then `BEGIN IMMEDIATE;`. Right after, two
//!    guards, each an insert into a temp table whose CHECK fails: unless
//!    `(SELECT foreign_keys FROM pragma_foreign_keys) = 0`, and unless
//!    `pragma_user_version` is still below this file's version (a restart
//!    after the file's `COMMIT` must never run it again).
//! 3. Save the rows of `pragma_foreign_key_check` (table, rowid, parent,
//!    fkid) into a temp table, duplicates kept, so an old orphan elsewhere
//!    does not block the upgrade.
//! 4. Per table: `CREATE TABLE x_new (...) STRICT`;
//!    `INSERT INTO x_new (rowid, ...) SELECT rowid, <explicit cast or
//!    conversion per column> FROM x` (the rowid only when x has one);
//!    `DROP TABLE x`;
//!    `ALTER TABLE x_new RENAME TO x`. Then recreate x's indexes and
//!    triggers, and any view or other table's trigger that names x (dropped
//!    before the `DROP`). Never rename the old table first: SQLite rewrites
//!    every child's `REFERENCES` to follow it. An AUTOINCREMENT table carries
//!    its `sqlite_sequence` value.
//! 5. Group `pragma_foreign_key_check` by (table, rowid, parent, fkid) and
//!    refuse when any group has more rows than step 3 saved for it, rowids
//!    compared with `IS`. A WITHOUT ROWID child reports a NULL rowid, so all
//!    of its orphans share one group: a set difference (`EXCEPT`) would let
//!    new ones through behind one old one, and a total count would let a
//!    fixed orphan mask a new one. On refusal the temp table's CHECK fails,
//!    the transaction never commits and the server does not start, with the
//!    database as it was.
//! 6. Drop the temp tables, `PRAGMA user_version = N;`, `COMMIT;`,
//!    `PRAGMA foreign_keys=ON;`. Only the two foreign-key pragmas sit outside
//!    `BEGIN IMMEDIATE ... COMMIT`; the stamp is inside.
//! 7. The runner records a file whose stamp already shows it, so the guard
//!    in step 2 never leaves a server unable to start.
//!
//! An upgrade test that migrates through a no-transaction file uses
//! [`run_migrations`], never sqlx's `Migrator::run`, which fails on the
//! file's own `BEGIN`. It also compares every `sqlite_schema` row other than
//! the rebuilt tables' own `CREATE TABLE`, before against after.

use std::time::{Duration, Instant};

use sqlx::migrate::{Migrate, MigrateError, Migration, Migrator};
use sqlx::pool::PoolConnection;
use sqlx::{Sqlite, SqliteConnection, SqlitePool};
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
    /// A no-transaction migration is not recorded, but the stamp is past it.
    #[error(
        "migration {version} is not recorded but the database stamp is already {stamp}; refusing to serve"
    )]
    StampAhead {
        /// The unrecorded no-transaction migration.
        version: i64,
        /// `PRAGMA user_version` read from the database file.
        stamp: i64,
    },
    /// A no-transaction migration ran but left foreign keys off or a stamp
    /// other than its own version.
    #[error(
        "migration {version} left foreign_keys = {foreign_keys} and user_version = {stamp}; refusing to serve"
    )]
    LeftOpen {
        /// The no-transaction migration that ran.
        version: i64,
        /// `PRAGMA foreign_keys` read after it ran.
        foreign_keys: i64,
        /// `PRAGMA user_version` read after it ran.
        stamp: i64,
    },
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
    run_migrations(pool, &MIGRATOR).await?;
    assert_migrated(pool).await
}

/// Apply `migrator`'s pending migrations on one connection of `pool`,
/// running each `-- no-transaction` file without a wrapping transaction.
/// The module doc gives the rules.
pub async fn run_migrations(pool: &SqlitePool, migrator: &Migrator) -> Result<(), SchemaError> {
    let mut conn = pool.acquire().await?;
    if migrator.locking {
        conn.lock().await?;
    }
    conn.ensure_migrations_table().await?;
    if let Some(version) = conn.dirty_version().await? {
        return Err(MigrateError::Dirty(version).into());
    }
    let applied = conn.list_applied_migrations().await?;
    if !migrator.ignore_missing
        && let Some(missing) = applied
            .iter()
            .find(|applied| !migrator.version_exists(applied.version))
    {
        return Err(MigrateError::VersionMissing(missing.version).into());
    }
    let mut pending = Vec::new();
    for migration in migrator.iter() {
        if migration.migration_type.is_down_migration() {
            continue;
        }
        match applied
            .iter()
            .find(|applied| applied.version == migration.version)
        {
            Some(applied) if applied.checksum != migration.checksum => {
                return Err(MigrateError::VersionMismatch(migration.version).into());
            }
            Some(_) => {}
            None => pending.push(migration),
        }
    }

    // A stamp past an unrecorded no-transaction file refuses before
    // anything is applied, so the refusal records nothing.
    let stamp = user_version(&mut conn).await?;
    if let Some(behind) = pending
        .iter()
        .find(|migration| migration.no_tx && migration.version < stamp)
    {
        return Err(SchemaError::StampAhead {
            version: behind.version,
            stamp,
        });
    }

    for migration in pending {
        if !migration.no_tx {
            conn.apply(migration).await?;
            continue;
        }
        let stamp = user_version(&mut conn).await?;
        if stamp > migration.version {
            return Err(SchemaError::StampAhead {
                version: migration.version,
                stamp,
            });
        }
        let mut guard = CloseUnlessDone {
            conn: &mut conn,
            done: false,
        };
        let done = if stamp == migration.version {
            tracing::info!(
                version = migration.version,
                "recording a migration that committed before a restart"
            );
            record(guard.conn, migration, None).await
        } else {
            run_unwrapped(guard.conn, migration).await
        };
        guard.done = done.is_ok();
        drop(guard);
        if let Err(error) = done {
            close(conn, migration.version).await;
            return Err(error);
        }
    }

    if migrator.locking {
        conn.unlock().await?;
    }
    Ok(())
}

/// Run one no-transaction file as written, check it left foreign keys on
/// and its own stamp, then record it.
async fn run_unwrapped(
    conn: &mut SqliteConnection,
    migration: &Migration,
) -> Result<(), SchemaError> {
    let start = Instant::now();
    sqlx::raw_sql(&migration.sql)
        .execute(&mut *conn)
        .await
        .map_err(|error| MigrateError::ExecuteMigration(error, migration.version))?;
    let elapsed = start.elapsed();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *conn)
        .await?;
    let stamp = user_version(conn).await?;
    if foreign_keys != 1 || stamp != migration.version {
        return Err(SchemaError::LeftOpen {
            version: migration.version,
            foreign_keys,
            stamp,
        });
    }
    record(conn, migration, Some(elapsed)).await
}

/// Record a migration with sqlx's own statements, so the row matches the one
/// `apply` writes. A file recorded without running keeps sqlx's -1 time.
async fn record(
    conn: &mut SqliteConnection,
    migration: &Migration,
    elapsed: Option<Duration>,
) -> Result<(), SchemaError> {
    sqlx::query(
        "INSERT INTO _sqlx_migrations ( version, description, success, checksum, execution_time ) \
         VALUES ( ?1, ?2, TRUE, ?3, -1 )",
    )
    .bind(migration.version)
    .bind(&*migration.description)
    .bind(&*migration.checksum)
    .execute(&mut *conn)
    .await?;
    if let Some(elapsed) = elapsed {
        sqlx::query("UPDATE _sqlx_migrations SET execution_time = ?1 WHERE version = ?2")
            .bind(i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX))
            .bind(migration.version)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Marks the connection to close rather than return to the pool unless the
/// no-transaction file finished, so a panic or a dropped boot future never
/// hands the pool an open transaction or foreign keys off. A file that
/// finished returns its connection, which keeps an in-memory database alive.
struct CloseUnlessDone<'a> {
    conn: &'a mut PoolConnection<Sqlite>,
    done: bool,
}

impl Drop for CloseUnlessDone<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.conn.close_on_drop();
        }
    }
}

/// Close the connection a no-transaction file failed on, so its open
/// transaction and `foreign_keys=OFF` never reach the pool.
async fn close(conn: PoolConnection<Sqlite>, version: i64) {
    if let Err(error) = conn.close().await {
        tracing::warn!(version, %error, "closing the connection a migration failed on");
    }
}

async fn user_version(conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(conn)
        .await
}

/// Apply the pending embedded migrations over one rusqlite connection, for
/// scratch test databases that never meet the async pool. Production
/// boots through [`apply_migrations`].
///
/// Like the sqlx migrator, a migration already applied is skipped: every
/// migration stamps `user_version`, so anything at or below the stamp is
/// done. Running this twice on one database is a no-op, which matters
/// because SQLite cannot make `ALTER TABLE ... ADD COLUMN` or a table
/// rebuild safe to replay in SQL alone. `execute_batch` wraps nothing, so a
/// `-- no-transaction` file runs as written here too.
#[cfg(any(test, feature = "test-support"))]
pub fn apply_migrations_blocking(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    let applied: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for migration in MIGRATOR.migrations.iter() {
        if migration.version > applied {
            connection.execute_batch(&migration.sql)?;
        }
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
