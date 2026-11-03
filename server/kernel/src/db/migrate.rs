//! The schema runner: one rusqlite connection, keyed on `PRAGMA user_version`.
//!
//! A binary carries one [`Chain`]: the baseline first, then any later files
//! in number order. Every file stamps its own number, and the stamp is the
//! only record of what ran: a file at or below it never runs again. There
//! are no down migrations; rollback is restoring a pre-upgrade backup.
//!
//! [`Chain::plan`] works out what a database needs from its stamp and its
//! table count, before anything is written:
//! - stamp 0 and no tables: the baseline, in one transaction with its
//!   stamp, so a crash leaves stamp 0 and no tables; then every later file.
//!   A `_sqlx_migrations` table left by a pre-release build that stopped
//!   before its first migration is no table, and the baseline's
//!   transaction drops it;
//! - stamp exactly the chain's bridge: one transaction that drops the old
//!   bookkeeping table `_sqlx_migrations` and stamps the baseline, then
//!   every later file. It moves a database made by the migration chain the
//!   baseline replaced, at that chain's last number, onto the baseline;
//! - stamp from the baseline to the latest: each later file above it;
//! - anything else is refused with nothing written: a stamp-0 file with
//!   tables is not a v3 database, a lower stamp is a pre-release database
//!   older than the bridge, and a higher one is newer than the binary.
//!
//! Boot works out the plan before it takes the pre-upgrade backup, so a
//! refused stamp never takes one: backups rotate, and a refusal repeated at
//! every restart would rotate out the backups it tells the user to restore.
//!
//! Each file runs in one transaction, and the runner checks that it stamped
//! its own number before the commit. A file whose first line is
//! `-- no-transaction` runs as written, because the recipe below holds its
//! own `BEGIN IMMEDIATE`, `COMMIT` and foreign-key switch; the runner then
//! checks that foreign keys are back on and the stamp is the file's. The
//! connection goes with any failure, which rolls back an open transaction
//! and its `foreign_keys=OFF`.
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
//!    after the file's `COMMIT` must never run it again). Name each CHECK
//!    (`CONSTRAINT guard_... CHECK (...)`), so a refusal at boot says
//!    which guard fired.
//! 3. Save the rows of `pragma_foreign_key_check` (table, rowid, parent)
//!    into a temp table, duplicates kept, so an old orphan elsewhere does not
//!    block the upgrade, and index the three columns: step 5 looks each group
//!    up, and a database can hold many orphans. Leave `fkid` out: a rebuild
//!    that adds a key renumbers the table's others, and an old orphan would
//!    then read as new. A child copied into a table of a new name is saved
//!    under that new name, the one its orphans report under after the copy.
//! 4. Per table: `CREATE TABLE x_new (...) STRICT`;
//!    `INSERT INTO x_new (rowid, ...) SELECT rowid, <explicit cast or
//!    conversion per column> FROM x` (the rowid only when x has one);
//!    `DROP TABLE x`;
//!    `ALTER TABLE x_new RENAME TO x`. Then recreate x's indexes and
//!    triggers, and any view or other table's trigger that names x (dropped
//!    before the `DROP`). Never rename the old table first: SQLite rewrites
//!    every child's `REFERENCES` to follow it. An AUTOINCREMENT table carries
//!    its `sqlite_sequence` value.
//! 5. Group `pragma_foreign_key_check` by (table, rowid, parent) and
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
//!
//! An upgrade test runs such a file through [`migrate`] and compares every
//! `sqlite_schema` row other than the rebuilt tables' own `CREATE TABLE`,
//! before against after.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use thiserror::Error;

use super::{error::DbError, sql, writer::open_connection};

/// The first line of a file that runs as written, outside the runner's
/// transaction.
pub const NO_TRANSACTION: &str = "-- no-transaction";

/// One migration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration<'a> {
    /// The number the file stamps.
    pub version: i64,
    /// The file's name in `server/migrations/`.
    pub name: &'a str,
    /// The file's text.
    pub sql: &'a str,
}

impl Migration<'_> {
    /// Whether the file runs as written: its first line is
    /// [`NO_TRANSACTION`].
    #[must_use]
    pub fn no_transaction(&self) -> bool {
        self.sql.lines().next().map(str::trim_end) == Some(NO_TRANSACTION)
    }
}

/// The migrations a binary carries, and the one stamp its bridge accepts.
#[derive(Debug, Clone, Copy)]
pub struct Chain<'a> {
    migrations: &'a [Migration<'a>],
    bridge_from: Option<i64>,
}

/// What a database needs, worked out before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The database is empty and takes the baseline.
    pub baseline: bool,
    /// The database is at the bridge's stamp and moves onto the baseline.
    pub bridge: bool,
    /// The later files to run, by number, in order.
    pub later: Vec<i64>,
}

impl Plan {
    /// Whether the plan changes a database that holds data, so the
    /// pre-upgrade backup must come first. An empty database taking the
    /// baseline has nothing to lose.
    #[must_use]
    pub fn needs_backup(&self) -> bool {
        !self.baseline && (self.bridge || !self.later.is_empty())
    }

    /// Whether the database is already at the latest stamp.
    #[must_use]
    pub fn is_current(&self) -> bool {
        !self.baseline && !self.bridge && self.later.is_empty()
    }
}

/// A database file's stamp and its table count, as the runner reads them.
/// SQLite's own tables and sqlx's `_sqlx_migrations` do not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// `PRAGMA user_version`.
    pub version: i64,
    /// Tables other than SQLite's own and `_sqlx_migrations`.
    pub tables: i64,
}

impl Stamp {
    /// Read the stamp over an open connection.
    pub fn read(connection: &Connection) -> rusqlite::Result<Self> {
        Ok(Self {
            version: user_version(connection)?,
            tables: connection.query_row(sql::USER_TABLE_COUNT.text(), [], |row| row.get(0))?,
        })
    }

    /// Read a file's stamp on a read-only connection, writing nothing.
    pub fn read_file(path: &Path) -> Result<Self, DbError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self::read(&connection)?)
    }
}

/// A stamp the runner refuses, or a migration that failed. A refusal
/// changes nothing on disk.
#[derive(Debug, Error)]
pub enum SchemaError {
    /// A stamp below the bridge: a v3 pre-release database the binary cannot
    /// move on.
    #[error(
        "database schema version {found} is a v3 pre-release database older than this version; \
         restore a newer backup or start with an empty data folder"
    )]
    TooOld {
        /// `PRAGMA user_version` read from the database file.
        found: i64,
    },
    /// A stamp past the newest migration this binary carries.
    #[error(
        "database schema version {found} is newer than this binary's {latest}; restore the \
         backup taken before the upgrade"
    )]
    TooNew {
        /// `PRAGMA user_version` read from the database file.
        found: i64,
        /// The newest migration this binary carries.
        latest: i64,
    },
    /// Tables with no stamp: not a database this server made.
    #[error(
        "the database file holds tables but no schema version, so it is not a DroppedNeedle v3 \
         database; point the server at an empty data folder"
    )]
    NotV3 {
        /// The tables found.
        tables: i64,
    },
    /// A migration file failed to run.
    #[error("schema migration {version} failed: {source}")]
    Failed {
        /// The file's number.
        version: i64,
        /// SQLite's failure.
        source: rusqlite::Error,
    },
    /// A migration ran but stamped another number, or a no-transaction
    /// file left foreign keys off.
    #[error(
        "schema migration {version} left foreign_keys = {foreign_keys} and user_version = \
         {stamp}; refusing to serve"
    )]
    LeftOpen {
        /// The file's number.
        version: i64,
        /// `PRAGMA foreign_keys` read after it ran.
        foreign_keys: i64,
        /// `PRAGMA user_version` read after it ran.
        stamp: i64,
    },
    /// Reading the stamp or committing failed.
    #[error("schema check failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl<'a> Chain<'a> {
    /// The chain of `migrations`, the baseline first and the rest in
    /// ascending order, whose bridge accepts a database stamped
    /// `bridge_from`.
    #[must_use]
    pub const fn new(migrations: &'a [Migration<'a>], bridge_from: Option<i64>) -> Self {
        Self {
            migrations,
            bridge_from,
        }
    }

    /// Every migration, the baseline first.
    #[must_use]
    pub fn migrations(&self) -> &'a [Migration<'a>] {
        self.migrations
    }

    /// The baseline's number.
    #[must_use]
    pub fn baseline(&self) -> i64 {
        self.migrations
            .first()
            .map_or(0, |migration| migration.version)
    }

    /// The newest migration's number: the stamp of a migrated database.
    #[must_use]
    pub fn latest(&self) -> i64 {
        self.migrations
            .last()
            .map_or(0, |migration| migration.version)
    }

    /// The stamp the bridge accepts, if the chain has one.
    #[must_use]
    pub fn bridge_from(&self) -> Option<i64> {
        self.bridge_from
    }

    /// What a database at `stamp` needs, or why it is refused.
    pub fn plan(&self, stamp: Stamp) -> Result<Plan, SchemaError> {
        let later_than = |floor: i64| -> Vec<i64> {
            self.migrations
                .iter()
                .skip(1)
                .map(|migration| migration.version)
                .filter(|version| *version > floor)
                .collect()
        };
        let found = stamp.version;
        if found == 0 {
            if stamp.tables > 0 {
                return Err(SchemaError::NotV3 {
                    tables: stamp.tables,
                });
            }
            return Ok(Plan {
                baseline: !self.migrations.is_empty(),
                bridge: false,
                later: later_than(self.baseline()),
            });
        }
        if found > self.latest() {
            return Err(SchemaError::TooNew {
                found,
                latest: self.latest(),
            });
        }
        if Some(found) == self.bridge_from {
            return Ok(Plan {
                baseline: false,
                bridge: true,
                later: later_than(self.baseline()),
            });
        }
        if found >= self.baseline() {
            return Ok(Plan {
                baseline: false,
                bridge: false,
                later: later_than(found),
            });
        }
        Err(SchemaError::TooOld { found })
    }
}

/// Bring the database on `connection` to the chain's latest stamp, and hand
/// the connection back. Any failure closes it.
pub fn migrate(mut connection: Connection, chain: &Chain<'_>) -> Result<Connection, SchemaError> {
    let plan = chain.plan(Stamp::read(&connection)?)?;
    let migrations = chain.migrations();
    if plan.baseline
        && let Some(baseline) = migrations.first()
    {
        run_wrapped(&mut connection, baseline, true)?;
        tracing::info!(version = baseline.version, "created the schema");
    }
    if plan.bridge {
        bridge(&mut connection, chain.baseline())?;
        tracing::info!(
            version = chain.baseline(),
            "moved a pre-release database onto the schema baseline"
        );
    }
    for version in plan.later {
        let Some(migration) = migrations
            .iter()
            .find(|migration| migration.version == version)
        else {
            continue;
        };
        connection = if migration.no_transaction() {
            run_unwrapped(connection, migration)?
        } else {
            run_wrapped(&mut connection, migration, false)?;
            connection
        };
        tracing::info!(version, name = migration.name, "applied a schema migration");
    }
    Ok(connection)
}

/// Open `path` with the canonical pragma set and bring it to the chain's
/// latest stamp, then close it.
pub fn migrate_file(path: &Path, chain: &Chain<'_>) -> Result<(), DbError> {
    let connection = open_connection(path)?;
    migrate(connection, chain)?;
    Ok(())
}

/// One file in one transaction that commits only once the file has stamped
/// its own number. With `fresh`, the transaction first drops a
/// `_sqlx_migrations` table an empty pre-release file may hold.
fn run_wrapped(
    connection: &mut Connection,
    migration: &Migration<'_>,
    fresh: bool,
) -> Result<(), SchemaError> {
    let version = migration.version;
    let failed = |source| SchemaError::Failed { version, source };
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(failed)?;
    if fresh {
        transaction
            .execute_batch(sql::DROP_SQLX_MIGRATIONS.text())
            .map_err(failed)?;
    }
    transaction.execute_batch(migration.sql).map_err(failed)?;
    let stamp = user_version(&transaction)?;
    if stamp != version {
        return Err(SchemaError::LeftOpen {
            version,
            foreign_keys: foreign_keys(&transaction)?,
            stamp,
        });
    }
    transaction.commit().map_err(failed)
}

/// Move a database at the bridge's stamp onto the baseline: drop the old
/// bookkeeping table `_sqlx_migrations` and stamp the baseline, in one
/// transaction.
fn bridge(connection: &mut Connection, baseline: i64) -> Result<(), SchemaError> {
    let failed = |source| SchemaError::Failed {
        version: baseline,
        source,
    };
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(failed)?;
    transaction
        .execute_batch(sql::DROP_SQLX_MIGRATIONS.text())
        .map_err(failed)?;
    transaction
        .pragma_update(None, "user_version", baseline)
        .map_err(failed)?;
    transaction.commit().map_err(failed)
}

/// Run one no-transaction file as written, then check it left foreign keys
/// on and its own stamp. The connection is handed back only when it did.
fn run_unwrapped(
    connection: Connection,
    migration: &Migration<'_>,
) -> Result<Connection, SchemaError> {
    let version = migration.version;
    connection
        .execute_batch(migration.sql)
        .map_err(|source| SchemaError::Failed { version, source })?;
    let foreign_keys = foreign_keys(&connection)?;
    let stamp = user_version(&connection)?;
    if foreign_keys != 1 || stamp != version || !connection.is_autocommit() {
        return Err(SchemaError::LeftOpen {
            version,
            foreign_keys,
            stamp,
        });
    }
    Ok(connection)
}

fn user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn foreign_keys(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A baseline at 10, two later files and a bridge from 8.
    const FAKE: &[Migration<'static>] = &[
        Migration {
            version: 10,
            name: "0010_baseline.sql",
            sql: "",
        },
        Migration {
            version: 11,
            name: "0011_a.sql",
            sql: "",
        },
        Migration {
            version: 13,
            name: "0013_b.sql",
            sql: "",
        },
    ];

    /// What the plan is for each stamp, and whether it takes the
    /// pre-upgrade backup first. A refused stamp takes none.
    #[test]
    fn each_stamp_has_one_plan() {
        let chain = Chain::new(FAKE, Some(8));
        let plan = |version, tables| chain.plan(Stamp { version, tables });
        let steps = |baseline, bridge, later: &[i64]| Plan {
            baseline,
            bridge,
            later: later.to_vec(),
        };
        for (version, tables, want, backup) in [
            (0, 0, steps(true, false, &[11, 13]), false),
            (8, 120, steps(false, true, &[11, 13]), true),
            (10, 120, steps(false, false, &[11, 13]), true),
            (11, 120, steps(false, false, &[13]), true),
            (12, 120, steps(false, false, &[13]), true),
            (13, 120, steps(false, false, &[]), false),
        ] {
            let got = plan(version, tables).unwrap();
            assert_eq!(got, want, "stamp {version}");
            assert_eq!(got.needs_backup(), backup, "stamp {version}");
        }
        assert!(plan(13, 120).unwrap().is_current());
        assert!(matches!(plan(0, 3), Err(SchemaError::NotV3 { tables: 3 })));
        assert!(matches!(
            plan(7, 120),
            Err(SchemaError::TooOld { found: 7 })
        ));
        assert!(matches!(
            plan(9, 120),
            Err(SchemaError::TooOld { found: 9 })
        ));
        assert!(matches!(
            plan(14, 120),
            Err(SchemaError::TooNew {
                found: 14,
                latest: 13
            })
        ));
        assert!(matches!(
            Chain::new(FAKE, None).plan(Stamp {
                version: 8,
                tables: 120
            }),
            Err(SchemaError::TooOld { found: 8 })
        ));
    }

    /// Only a first line of exactly `-- no-transaction` runs a file as
    /// written.
    #[test]
    fn the_first_line_marks_a_file_that_runs_as_written() {
        let file = |sql| Migration {
            version: 1,
            name: "x",
            sql,
        };
        assert!(file("-- no-transaction\nBEGIN;").no_transaction());
        assert!(!file("-- 0012_a.sql\n-- no-transaction").no_transaction());
        assert!(!file("-- no-transaction please\n").no_transaction());
    }
}
