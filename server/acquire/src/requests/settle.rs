//! The one place a request follows its download.
//!
//! A request's status moves inside the transaction that settles what it
//! waits for: the journal write that finishes its task, or the write that
//! records its album reaching the library. Requests that reach the
//! library come back as [`Fulfilled`] and are announced once, after that
//! transaction commits ([`announce_fulfilled`]). A repeated settle finds
//! the rows already moved and announces nothing.

use rusqlite::{Connection, OptionalExtension, params};

use super::ledger::RequestStatus;
use super::models::RequestKind;
use super::sql;
use crate::downloads::state::TaskStatus;
use crate::flows::seams::{FlowEvent, TickSink};

/// A request that reached the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fulfilled {
    /// The request's MBID (the release group for albums).
    pub key: String,
    /// Who asked, when the row has an owner.
    pub user_id: Option<String>,
    /// The album that arrived (empty for a track whose album is unknown).
    pub release_group_mbid: String,
}

/// The status a request takes when its task settles as `outcome`, and
/// whether failed and incomplete rows follow too. `None` for a task that
/// is still live.
fn settled_status(outcome: TaskStatus) -> Option<(RequestStatus, bool)> {
    match outcome {
        TaskStatus::Completed => Some((RequestStatus::Imported, true)),
        TaskStatus::Partial => Some((RequestStatus::Incomplete, false)),
        TaskStatus::Failed => Some((RequestStatus::Failed, false)),
        TaskStatus::Cancelled => Some((RequestStatus::Cancelled, false)),
        TaskStatus::Queued | TaskStatus::Downloading | TaskStatus::Processing => None,
    }
}

/// Decode the rows a settle moved.
fn fulfilled_rows(
    mut statement: rusqlite::CachedStatement<'_>,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<Fulfilled>> {
    let rows = statement.query_map(params, |row| {
        let stored: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let user_id: Option<String> = row.get(2)?;
        let track_group: Option<String> = row.get(3)?;
        let key = stored.strip_prefix("track:").unwrap_or(&stored).to_owned();
        let release_group_mbid = match RequestKind::parse(&kind) {
            Some(RequestKind::Album) => key.clone(),
            _ => track_group.unwrap_or_default(),
        };
        Ok(Fulfilled {
            key,
            user_id,
            release_group_mbid,
        })
    })?;
    rows.collect()
}

/// Settle every request linked to `task_id`, which just settled as
/// `outcome`: live rows take the task's result, and a completed task also
/// imports failed and incomplete rows (a retry that served them). Rows
/// mid-cancel, waiting for approval or already final stay as they are. A
/// completed album also fulfils its wanted watch. Answers the rows that
/// reached the library.
pub(crate) fn on_task_settled(
    conn: &Connection,
    task_id: &str,
    outcome: TaskStatus,
    at_ms: i64,
) -> rusqlite::Result<Vec<Fulfilled>> {
    let Some((status, finished_too)) = settled_status(outcome) else {
        return Ok(Vec::new());
    };
    let stamp = (status != RequestStatus::Incomplete).then_some(at_ms);
    let rows = fulfilled_rows(
        conn.prepare_cached(sql::SETTLE_TASK_ROWS.text())?,
        params![task_id, status, stamp, finished_too],
    )?;
    if outcome != TaskStatus::Completed {
        return Ok(Vec::new());
    }
    let album: Option<(String, String)> = conn
        .prepare_cached(sql::TASK_ALBUM.text())?
        .query_row(params![task_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    if let Some((kind, group)) = album
        && kind == "album"
        && !group.is_empty()
    {
        conn.prepare_cached(sql::FULFIL_WATCH.text())?
            .execute(params![group, "imported", at_ms])?;
    }
    Ok(rows)
}

/// The library now holds album `release_group_mbid`: its open, waiting or
/// failed request is imported and its wanted watch fulfilled with
/// `watch_outcome`, whether or not a request moved. A cancelled, rejected
/// or mid-cancel request stays as it is. Answers the rows that moved.
pub(crate) fn on_album_in_library(
    conn: &Connection,
    release_group_mbid: &str,
    watch_outcome: &str,
    at_ms: i64,
) -> rusqlite::Result<Vec<Fulfilled>> {
    let key = release_group_mbid.trim().to_lowercase();
    let rows = fulfilled_rows(
        conn.prepare_cached(sql::SETTLE_ALBUM_ROW.text())?,
        params![key, at_ms],
    )?;
    conn.prepare_cached(sql::FULFIL_WATCH.text())?
        .execute(params![key, watch_outcome, at_ms])?;
    Ok(rows)
}

/// Point every request that followed `from` at its retry `to`: the row
/// reads downloading again, and the retry records which request it serves.
/// Rows a person decided, waiting for approval or mid-cancel stay put.
pub(crate) fn relink(conn: &Connection, from: &str, to: &str) -> rusqlite::Result<()> {
    let keys: Vec<String> = conn
        .prepare_cached(sql::RELINK_ROWS.text())?
        .query_map(params![from, to], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if let Some(key) = keys.last() {
        conn.prepare_cached(sql::MARK_TASK_REQUEST.text())?
            .execute(params![to, key])?;
    }
    Ok(())
}

/// Tell plugins and the flow log once per request that reached the
/// library. Called only after the settling transaction committed.
pub fn announce_fulfilled(ticks: &dyn TickSink, fulfilled: Vec<Fulfilled>, now: i64) {
    for row in fulfilled {
        ticks.emit(
            "request_fulfilled",
            &format!("request {} imported", row.key),
            now,
        );
        ticks.announce(FlowEvent::RequestFulfilled {
            request_id: row.key,
            user_id: row.user_id.unwrap_or_default(),
            release_group_mbid: row.release_group_mbid,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::ledger::can_request_transition;
    use super::*;

    /// Statuses a row may hold when a settle or link touches it, as the
    /// statements list them.
    const LIVE: [RequestStatus; 3] = [
        RequestStatus::Pending,
        RequestStatus::Queued,
        RequestStatus::Downloading,
    ];

    // Every move the statements make is one the transition table allows.
    #[test]
    fn every_settle_and_link_is_a_legal_move() {
        for outcome in TaskStatus::ALL {
            let Some((to, finished_too)) = settled_status(outcome) else {
                continue;
            };
            let mut from = LIVE.to_vec();
            if finished_too {
                from.extend([RequestStatus::Failed, RequestStatus::Incomplete]);
            }
            for from in from {
                assert!(
                    from == to || can_request_transition(from, to),
                    "{from} -> {to}"
                );
            }
        }
        for from in [
            RequestStatus::Pending,
            RequestStatus::Queued,
            RequestStatus::Downloading,
            RequestStatus::AwaitingApproval,
            RequestStatus::Failed,
            RequestStatus::Incomplete,
        ] {
            assert!(
                can_request_transition(from, RequestStatus::Imported),
                "{from} -> imported"
            );
        }
        for from in [
            RequestStatus::Pending,
            RequestStatus::Queued,
            RequestStatus::Failed,
            RequestStatus::Incomplete,
            RequestStatus::Cancelled,
        ] {
            assert!(
                can_request_transition(from, RequestStatus::Downloading),
                "{from} -> downloading"
            );
        }
    }
}
