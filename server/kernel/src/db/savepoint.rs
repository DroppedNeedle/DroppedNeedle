//! One part of a write that may fail on its own.
//!
//! Inside a write closure, [`savepoint`] runs a part of the transaction so
//! that, when the part fails, only what it wrote is undone and the rest of
//! the transaction goes on. Raw savepoint text stays in this file.

use rusqlite::Connection;

/// Run `part` inside the savepoint `name` on `conn`, which must be inside a
/// transaction. On `Ok` the savepoint is released and its work joins the
/// transaction; on `Err` everything `part` wrote is rolled back and the
/// savepoint released. The outer `Result` is the savepoint's own statements;
/// the inner one is what `part` answered, so a caller can log the failure
/// and go on. Savepoints nest, and a nested one may reuse a name.
///
/// `name` is a plain SQL identifier written by the caller, never input.
/// After the lane aborts a write past its budget, the rollback statement
/// fails too and the outer error ends the closure; nothing is committed.
pub fn savepoint<T, E>(
    conn: &Connection,
    name: &'static str,
    part: impl FnOnce() -> Result<T, E>,
) -> rusqlite::Result<Result<T, E>> {
    debug_assert!(
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "a savepoint name is a plain identifier"
    );
    conn.execute_batch(&format!("SAVEPOINT {name}"))?;
    match part() {
        Ok(value) => {
            conn.execute_batch(&format!("RELEASE {name}"))?;
            Ok(Ok(value))
        }
        Err(error) => {
            conn.execute_batch(&format!("ROLLBACK TO {name}; RELEASE {name}"))?;
            Ok(Err(error))
        }
    }
}
