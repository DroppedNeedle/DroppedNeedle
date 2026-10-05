//! Durable source operations.
//!
//! Undo replays the exact sealed bundle a past operation published:
//! pinned identity, override revisions, and per-file fingerprints.
//! The publisher records each bundle here after its pre-gates pass,
//! before the first byte stages; rows for failed operations linger
//! harmlessly (undo finds no snapshots for them and stays put).

use rusqlite::{Connection, OptionalExtension};

use super::PublishError;
use super::planner::PlanBundle;

/// Record one sealed bundle. Overwriting an existing row is refused:
/// bundle ids are unique per operation.
pub fn record_operation(
    conn: &Connection,
    bundle: &PlanBundle,
    today_day: i64,
) -> Result<(), PublishError> {
    let json = serde_json::to_string(bundle).map_err(PublishError::from)?;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO publish_operations (bundle_id, bundle_json, created_day)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![bundle.id, json, today_day],
    )?;
    if changed == 0 {
        return Err(PublishError::Journal(format!(
            "operation {} already recorded",
            bundle.id
        )));
    }
    Ok(())
}

/// Load one recorded bundle, or `None` when the id is unknown.
pub fn load_operation(
    conn: &Connection,
    bundle_id: &str,
) -> Result<Option<PlanBundle>, PublishError> {
    let row: Option<String> = conn
        .query_row(
            "SELECT bundle_json FROM publish_operations WHERE bundle_id = ?1",
            rusqlite::params![bundle_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(PublishError::from)?;
    row.map(|json: String| serde_json::from_str(&json).map_err(PublishError::from))
        .transpose()
}
