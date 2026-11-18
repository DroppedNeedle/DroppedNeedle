//! The catalog revision: a change counter read caches and open tabs key
//! on. Every transaction that changes catalog-visible rows moves it here,
//! in that same transaction, so the move commits with the rows or not at
//! all.
//!
//! The counter is also the high-water mark album identity revisions are
//! taken above: a new or moved album identity row starts one past it, and
//! the counter is raised to a row's revision before that row leaves its
//! album, so an album never sees one of its revisions again. It is never
//! lowered, and nothing compares it with an identity revision.

pub(crate) mod sql;

use rusqlite::Connection;

/// Move the catalog revision in the caller's transaction; returns the new
/// value.
pub(crate) fn bump_catalog(conn: &Connection) -> rusqlite::Result<i64> {
    conn.prepare_cached(sql::BUMP.text())?
        .query_row([], |row| row.get(0))
}

/// Raise the catalog revision to at least `floor` in the caller's
/// transaction; a counter already there stays as it is.
pub(crate) fn raise_catalog(conn: &Connection, floor: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(sql::RAISE.text())?.execute([floor])?;
    Ok(())
}
