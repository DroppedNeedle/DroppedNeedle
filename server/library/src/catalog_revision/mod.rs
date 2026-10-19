//! The catalog revision: a change counter read caches and open tabs key
//! on. Every transaction that changes catalog-visible rows moves it here,
//! in that same transaction, so the move commits with the rows or not at
//! all.

pub(crate) mod sql;

use rusqlite::Connection;

/// Move the catalog revision in the caller's transaction; returns the new
/// value.
pub(crate) fn bump_catalog(conn: &Connection) -> rusqlite::Result<i64> {
    conn.prepare_cached(sql::BUMP.text())?
        .query_row([], |row| row.get(0))
}
