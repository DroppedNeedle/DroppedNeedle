//! The request step: requests v2 had not finished, once the catalog and
//! the download history have landed.
//!
//! The request section carries them waiting for approval, or approved with
//! no download (`pending`), which acquire's startup recovery starts by
//! itself; the download waits until a download source is ready. For the
//! carried ones still as they landed, this step:
//!
//! - settles an album request whose album the library already holds as
//!   imported, as acquire's request sweep would, so it never downloads
//!   again;
//! - moves a request that starts again past the downloads its owner
//!   started for it in v2. Recovery reads a download no older than its
//!   request as the one made for it and follows it, and a download v2
//!   finished would leave the request finished instead of searching again.
//!
//! The summary says how many carried in each state. The step is marked
//! carried like a section, so a repeat import changes nothing.

use rusqlite::{Connection, params};

use super::sql;
use super::{CarryError, SectionResult, bundle};
use crate::export::sections::requests::REQUESTS;
use crate::import::report::now_ms;
use droppedneedle_acquire::requests::ledger::RequestStatus;
use droppedneedle_kernel::db::statement::Statement;
use droppedneedle_kernel::db::time::Millis;

/// Report entity and section marker name.
pub(crate) const ENTITY: &str = "unfinished_request";

/// Settle and start the carried unfinished requests. On a dry run, count.
pub(crate) fn apply(conn: &Connection, dry_run: bool) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    // A bundle that does not hold the request section has nothing here.
    let present = bundle::present_columns(conn, &REQUESTS)?;
    if !present.iter().any(|column| column.name == "status") {
        return Ok(result);
    }
    let waiting = count(
        conn,
        sql::UNFINISHED_BUNDLED,
        RequestStatus::AwaitingApproval,
    )?;
    let starting = count(conn, sql::UNFINISHED_BUNDLED, RequestStatus::Pending)?;
    result.rows = waiting + starting;
    if result.rows == 0 {
        return Ok(result);
    }
    if dry_run {
        result.note(
            String::new(),
            "would_carry",
            format!(
                "{waiting} unfinished request(s) would wait for approval and {starting} would \
                 search again, less any whose album the library holds, which would be marked \
                 imported"
            ),
        );
        return Ok(result);
    }
    let owned = conn.execute(
        sql::SETTLE_UNFINISHED_OWNED.text(),
        params![Millis(now_ms())],
    )? as u64;
    let waiting = count(
        conn,
        sql::UNFINISHED_LANDED,
        RequestStatus::AwaitingApproval,
    )?;
    let starting = count(conn, sql::UNFINISHED_LANDED, RequestStatus::Pending)?;
    let moved = conn.execute(sql::START_UNFINISHED_AFRESH.text(), [])? as u64;
    result.written = owned + moved;
    let one = |count: u64, single: &'static str, plural: &'static str| {
        if count == 1 { single } else { plural }
    };
    if waiting > 0 {
        result.summary.push(format!(
            "{waiting} unfinished {} for an admin's approval.",
            one(waiting, "request waits", "requests wait")
        ));
    }
    if starting > 0 {
        result.summary.push(format!(
            "{starting} {} once a download source is ready.",
            one(
                starting,
                "request still downloading or queued in v2 searches again by itself",
                "requests still downloading or queued in v2 search again by themselves"
            )
        ));
    }
    if owned > 0 {
        result.summary.push(format!(
            "{owned} unfinished {} marked imported.",
            one(
                owned,
                "request for an album already in your library is",
                "requests for an album already in your library are"
            )
        ));
    }
    Ok(result)
}

/// How many requests in `status` one counting statement finds.
fn count(conn: &Connection, statement: Statement, status: RequestStatus) -> rusqlite::Result<u64> {
    let count: i64 = conn.query_row(statement.text(), params![status], |row| row.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}
