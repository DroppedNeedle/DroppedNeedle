//! Durable source operations.
//!
//! Undo replays the exact sealed bundle a past operation published:
//! pinned identity, override revisions, and per-file fingerprints.
//! The publisher records each bundle here after its pre-gates pass,
//! before the first byte stages; rows for failed operations linger
//! harmlessly (undo finds no snapshots for them and stays put).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use super::PublishError;
use super::planner::{GeneratedOutput, OutputKind, PlanBundle};

/// Record one sealed bundle, made at `now_ms` (unix milliseconds) on
/// `today_day`, with the account that applied it when a person did.
/// Overwriting an existing row is refused: bundle ids are unique per
/// operation.
pub fn record_operation(
    conn: &Connection,
    bundle: &PlanBundle,
    today_day: i64,
    now_ms: i64,
    applied_by: Option<&str>,
) -> Result<(), PublishError> {
    let json = serde_json::to_string(bundle).map_err(PublishError::from)?;
    let changed = conn
        .prepare_cached(super::sql::OPERATION_INSERT.text())?
        .execute(rusqlite::params![
            bundle.id, json, today_day, now_ms, applied_by
        ])?;
    if changed == 0 {
        return Err(PublishError::Journal(
            format!("operation {} already recorded", bundle.id).into(),
        ));
    }
    Ok(())
}

/// Every file a committed operation generated for `track_id` (a cover,
/// an `.lrc`), oldest first, one entry per path: the latest write to a
/// path wins. Whether each still holds those bytes is the caller's check.
pub fn generated_for_track(
    conn: &Connection,
    track_id: &str,
) -> Result<Vec<GeneratedOutput>, PublishError> {
    let rows = conn
        .prepare_cached(super::sql::OUTPUTS_FOR_TRACK.text())?
        .query_map(rusqlite::params![track_id], |row| read_output(row, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out: Vec<GeneratedOutput> = Vec::new();
    for output in rows {
        out.retain(|known| {
            known.dest_root != output.dest_root || known.dest_rel != output.dest_rel
        });
        out.push(output);
    }
    Ok(out)
}

/// The files one committed operation generated, by track, in plan order.
/// Empty for an operation that never committed.
pub fn outputs_of(
    conn: &Connection,
    bundle_id: &str,
) -> Result<HashMap<String, Vec<GeneratedOutput>>, PublishError> {
    let mut out: HashMap<String, Vec<GeneratedOutput>> = HashMap::new();
    let rows = conn
        .prepare_cached(super::sql::OUTPUTS_OF_BUNDLE.text())?
        .query_map(rusqlite::params![bundle_id], |row| {
            Ok((row.get::<_, String>(0)?, read_output(row, 1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (track_id, output) in rows {
        out.entry(track_id).or_default().push(output);
    }
    Ok(out)
}

/// One output row from column `at` on: root, path, hash, size and kind.
fn read_output(row: &rusqlite::Row<'_>, at: usize) -> Result<GeneratedOutput, rusqlite::Error> {
    let kind: String = row.get(at + 4)?;
    let kind = OutputKind::parse(&kind).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            at + 4,
            rusqlite::types::Type::Text,
            format!("unknown output kind {kind}").into(),
        )
    })?;
    Ok(GeneratedOutput {
        dest_root: row.get(at)?,
        dest_rel: row.get(at + 1)?,
        sha256: row.get(at + 2)?,
        size: row.get(at + 3)?,
        kind,
    })
}

/// One recorded bundle as stored, undecoded, or `None` when the id is
/// unknown. A read failure is an error; whether the text decodes is the
/// caller's question.
pub fn operation_json(conn: &Connection, bundle_id: &str) -> Result<Option<String>, PublishError> {
    conn.prepare_cached(super::sql::OPERATION_JSON.text())?
        .query_row(rusqlite::params![bundle_id], |row| row.get(0))
        .optional()
        .map_err(PublishError::from)
}

/// Load one recorded bundle, or `None` when the id is unknown.
pub fn load_operation(
    conn: &Connection,
    bundle_id: &str,
) -> Result<Option<PlanBundle>, PublishError> {
    let row: Option<String> = conn
        .query_row(
            "SELECT bundle_json FROM library_publish_operations WHERE bundle_id = ?1",
            rusqlite::params![bundle_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(PublishError::from)?;
    row.map(|json: String| serde_json::from_str(&json).map_err(PublishError::from))
        .transpose()
}

/// The account that applied one recorded bundle: `None` when the id is
/// unknown, `Some(None)` when no account is on record for it.
pub fn applied_by(
    conn: &Connection,
    bundle_id: &str,
) -> Result<Option<Option<String>>, PublishError> {
    conn.query_row(
        "SELECT applied_by_user_id FROM library_publish_operations WHERE bundle_id = ?1",
        rusqlite::params![bundle_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(PublishError::from)
}
