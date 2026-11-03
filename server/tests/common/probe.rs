//! Plain SQL against a test's database file: seed rows, then check what
//! landed. Each call opens a connection of its own with the production
//! pragma set, so a probe takes no turn on the writer lane and no
//! connection from the read pools, and a test that holds either still
//! reads. A failure panics with the statement, as a test wants.

use std::path::{Path, PathBuf};

use droppedneedle_kernel::db::{Db, DbRuntime, open_connection};
use rusqlite::types::FromSql;
use rusqlite::{Connection, OptionalExtension, Params, Row};

/// A row read as a tuple of its columns.
pub trait FromRow: for<'r> TryFrom<&'r Row<'r>, Error = rusqlite::Error> {}

impl<T> FromRow for T where T: for<'r> TryFrom<&'r Row<'r>, Error = rusqlite::Error> {}

/// Something that names a database file a test can probe.
pub trait Probe {
    /// The database file.
    fn file(&self) -> &Path;

    /// A fresh connection to [`Probe::file`].
    fn connect(&self) -> Connection {
        open_connection(self.file()).expect("probe connection opens")
    }

    /// Run one statement; the rows it changed.
    fn exec(&self, sql: &str, params: impl Params) -> usize {
        self.connect()
            .execute(sql, params)
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// Run several statements with no parameters.
    fn exec_batch(&self, sql: &str) {
        self.connect()
            .execute_batch(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    /// The first column of the one row a query answers.
    fn one<T: FromSql>(&self, sql: &str, params: impl Params) -> T {
        self.connect()
            .query_row(sql, params, |row| row.get(0))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// The first column of the row a query answers, if it answers one.
    fn maybe<T: FromSql>(&self, sql: &str, params: impl Params) -> Option<T> {
        self.connect()
            .query_row(sql, params, |row| row.get(0))
            .optional()
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// The first column of every row a query answers.
    fn column<T: FromSql>(&self, sql: &str, params: impl Params) -> Vec<T> {
        let connection = self.connect();
        let mut statement = connection
            .prepare(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        statement
            .query_map(params, |row| row.get(0))
            .and_then(|rows| rows.collect())
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// The one row a query answers.
    fn row<T: FromRow>(&self, sql: &str, params: impl Params) -> T {
        self.connect()
            .query_row(sql, params, |row| T::try_from(row))
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// The row a query answers, if it answers one.
    fn maybe_row<T: FromRow>(&self, sql: &str, params: impl Params) -> Option<T> {
        self.connect()
            .query_row(sql, params, |row| T::try_from(row))
            .optional()
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// Every row a query answers.
    fn rows<T: FromRow>(&self, sql: &str, params: impl Params) -> Vec<T> {
        let connection = self.connect();
        let mut statement = connection
            .prepare(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        statement
            .query_map(params, |row| T::try_from(row))
            .and_then(|rows| rows.collect())
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }
}

impl Probe for Path {
    fn file(&self) -> &Path {
        self
    }
}

impl Probe for PathBuf {
    fn file(&self) -> &Path {
        self
    }
}

impl Probe for Db {
    fn file(&self) -> &Path {
        self.path()
    }
}

impl Probe for DbRuntime {
    fn file(&self) -> &Path {
        self.path()
    }
}
