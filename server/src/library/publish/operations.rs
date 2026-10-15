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

/// Record one sealed bundle, with the account that applied it when a
/// person did. Overwriting an existing row is refused: bundle ids are
/// unique per operation.
pub fn record_operation(
    conn: &Connection,
    bundle: &PlanBundle,
    today_day: i64,
    applied_by: Option<&str>,
) -> Result<(), PublishError> {
    let json = serde_json::to_string(bundle).map_err(PublishError::from)?;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO library_publish_operations
         (bundle_id, bundle_json, created_day, applied_by_user_id)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![bundle.id, json, today_day, applied_by],
    )?;
    if changed == 0 {
        return Err(PublishError::Journal(format!(
            "operation {} already recorded",
            bundle.id
        )));
    }
    Ok(())
}

/// Every file a committed operation generated for `track_id` (a cover,
/// an `.lrc`), oldest first, one entry per path. Whether each still
/// holds those bytes is the caller's check.
pub fn generated_for_track(
    conn: &Connection,
    track_id: &str,
) -> Result<Vec<super::planner::GeneratedOutput>, PublishError> {
    let mut statement = conn.prepare(
        "SELECT bundle_id, bundle_json FROM library_publish_operations
         WHERE instr(bundle_json, '\"outputs\":[{') > 0 AND instr(bundle_json, ?1) > 0
         ORDER BY created_day, rowid",
    )?;
    let rows: Vec<(String, String)> = statement
        .query_map(rusqlite::params![track_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut out: Vec<super::planner::GeneratedOutput> = Vec::new();
    for (bundle_id, json) in rows {
        let bundle: PlanBundle = serde_json::from_str(&json).map_err(PublishError::from)?;
        for item in bundle.items.iter().filter(|item| item.track_id == track_id) {
            for output in &item.outputs {
                // Only a write the operation finished counts: a failed
                // one may never have put its file there.
                let landed: bool = conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM library_publish_journal
                     WHERE bundle_id = ?1 AND kind = 'output' AND dest_root = ?2
                       AND dest_rel = ?3 AND staged_sha256 = ?4
                       AND state IN ('committed', 'cleaned'))",
                    rusqlite::params![bundle_id, output.dest_root, output.dest_rel, output.sha256],
                    |row| row.get(0),
                )?;
                if !landed {
                    continue;
                }
                out.retain(|known| {
                    known.dest_root != output.dest_root || known.dest_rel != output.dest_rel
                });
                out.push(output.clone());
            }
        }
    }
    Ok(out)
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
