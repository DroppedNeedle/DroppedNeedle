//! The link step: references to v2 library items that carried rows hold
//! while the catalog was not there yet.
//!
//! User data is carried before the catalog, so its library references are
//! first recorded in `import_pending_links` (or held by an earlier import
//! that had no catalog at all). Once the catalog has landed, every
//! reference whose item exists is settled:
//!
//! - `as_written` rows already hold the id, and `link_only` ones have no
//!   v3 column: their pending entry just goes;
//! - `until_resolved` columns (which have a foreign key) get the id
//!   written now, then the entry goes.
//!
//! References whose v2 item is gone stay pending and are listed. The
//! report notes the numbers rather than counting rows (no row is added),
//! so a dry run and a real run report the same counts. The step runs on
//! every import and is never marked, so a later import that
//! brings the catalog settles links an earlier one left waiting.

use droppedneedle_kernel::db::statement::Choice as _;
use rusqlite::Connection;

use super::{CarryError, SectionResult};
use crate::import::sql::{self, WaitingColumn};

/// Report entity name.
pub(crate) const ENTITY: &str = "library_link";

/// Settle every pending link whose item now exists. On a dry run, count.
pub(crate) fn apply(conn: &Connection, dry_run: bool) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    let resolvable: i64 = conn.query_row(sql::LINKS_RESOLVABLE.text(), [], |row| row.get(0))?;
    if dry_run || resolvable == 0 {
        // A dry run writes no pending links to settle; it only says how
        // many earlier imports left that the catalog now answers.
        if resolvable > 0 {
            result.note(
                String::new(),
                "would_settle",
                format!("{resolvable} reference(s) left by an earlier import"),
            );
        }
        return Ok(result);
    }
    for column in WaitingColumn::all() {
        conn.execute(&sql::link_settle(column), [])?;
    }
    let settled = conn.execute(sql::LINKS_SETTLED.text(), [])? as u64;
    result.written = settled;
    result.note(
        String::new(),
        "settled",
        format!("{settled} reference(s) to v2 library items now point at the carried catalog"),
    );
    let waiting: i64 = conn.query_row(sql::LINKS_WAITING.text(), [], |row| row.get(0))?;
    if waiting > 0 {
        result.note(
            String::new(),
            "pending",
            format!(
                "{waiting} reference(s) name v2 library items that were already gone in v2; \
                 the rows keep their names. Nothing to do"
            ),
        );
    }
    Ok(result)
}
