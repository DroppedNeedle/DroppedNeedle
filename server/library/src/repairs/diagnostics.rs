//! Scan diagnostics: a bounded, redacted JSON report of one scan run to
//! attach to a bug report (v2's `library_diagnostics_service`).
//!
//! It holds the run's counters and timings, its scopes, the files it
//! inventoried and the files that failed, with every path replaced by its
//! SHA-256 and the requesting user and worker left out. No credentials,
//! full paths, provider responses or error text. At most 5,000 rows and
//! 2 MB.

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension as _, Statement, params};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::reasons;
use crate::operations::models::OperationError;

pub const MAX_ROWS: usize = 5_000;
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cell(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => json!(number),
        ValueRef::Real(number) => json!(number),
        ValueRef::Text(text) => json!(String::from_utf8_lossy(text)),
        ValueRef::Blob(bytes) => json!(bytes.len()),
    }
}

/// Up to `limit` rows as JSON objects, with `relative_path` swapped for
/// its hash and `drop` columns left out. True when more rows exist.
fn rows(
    stmt: &mut Statement<'_>,
    params: impl rusqlite::Params,
    limit: usize,
    drop: &[&str],
) -> rusqlite::Result<(Vec<Value>, bool)> {
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let mut out = Vec::new();
    let mut more = false;
    let mut cursor = stmt.query(params)?;
    while let Some(row) = cursor.next()? {
        if out.len() == limit {
            more = true;
            break;
        }
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            if drop.contains(&name.as_str()) {
                continue;
            }
            let value = cell(row.get_ref(index)?);
            if name == "relative_path" {
                let path = value.as_str().unwrap_or_default().to_owned();
                object.insert("relative_path_hash".to_owned(), json!(sha256(&path)));
            } else {
                object.insert(name.clone(), value);
            }
        }
        out.push(Value::Object(object));
    }
    Ok((out, more))
}

fn valid_run_id(run_id: &str) -> bool {
    !run_id.is_empty() && run_id.len() <= 128 && !run_id.contains(['/', '\\', '\r', '\n'])
}

/// The report for one run: its file name and JSON bytes. Blocking.
pub fn export(conn: &Connection, run_id: &str) -> Result<(String, Vec<u8>), OperationError> {
    if !valid_run_id(run_id) {
        return Err(OperationError::Invalid(reasons::RUN_ID_INVALID));
    }
    let mut stmt = conn.prepare("SELECT * FROM library_scan_runs WHERE id = ?1")?;
    let (run, _) = rows(
        &mut stmt,
        params![run_id],
        1,
        &["requested_by_user_id", "lease_owner"],
    )?;
    let Some(run) = run.into_iter().next() else {
        return Err(OperationError::NotFound(reasons::RUN_NOT_FOUND));
    };
    let mut stmt = conn.prepare(
        "SELECT root_id, relative_path, effective_policy, policy_revision, estimated_count, \
         discovered_count, discovery_state, reconciliation_state, phase_timings_json, error_code \
         FROM library_scan_run_scopes WHERE run_id = ?1 ORDER BY scope_sequence",
    )?;
    let (scopes, scopes_truncated) = rows(&mut stmt, params![run_id], MAX_ROWS, &[])?;
    let mut stmt = conn.prepare(
        "SELECT root_id, relative_path, comparison_result, processing_state, failure_code, \
         file_size_bytes FROM library_scan_inventory WHERE run_id = ?1 \
         ORDER BY root_id, relative_path",
    )?;
    let (inventory, inventory_truncated) = rows(
        &mut stmt,
        params![run_id],
        MAX_ROWS.saturating_sub(scopes.len()),
        &[],
    )?;
    // The failure detail is error text, which the report leaves out.
    let mut stmt = conn.prepare(
        "SELECT root_id, relative_path, failure_code, phase, recorded_at \
         FROM library_scan_failures WHERE run_id = ?1 ORDER BY recorded_at, root_id, relative_path",
    )?;
    let (failures, failures_truncated) = rows(&mut stmt, params![run_id], MAX_ROWS, &[])?;
    let identification: Option<Value> = conn
        .query_row(
            "SELECT json_group_object(state, n) FROM \
             (SELECT state, COUNT(*) AS n FROM library_identify_jobs GROUP BY state)",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let document = json!({
        "format": "droppedneedle-library-diagnostic-v1",
        "run": run,
        "scopes": scopes,
        "scopes_truncated": scopes_truncated,
        "inventory": inventory,
        "inventory_truncated": inventory_truncated,
        "failures": failures,
        "failures_truncated": failures_truncated,
        "exported_row_count": scopes.len() + inventory.len() + failures.len(),
        "identification_jobs_by_state": identification.unwrap_or_else(|| json!({})),
        "limits": { "maximum_rows": MAX_ROWS, "maximum_bytes": MAX_BYTES },
        "excluded": [
            "credentials",
            "full_filesystem_paths",
            "raw_provider_responses",
            "exception_text",
        ],
    });
    let bytes =
        serde_json::to_vec(&document).map_err(|error| OperationError::Store(error.to_string()))?;
    if bytes.len() > MAX_BYTES {
        return Err(OperationError::Invalid(reasons::DIAGNOSTICS_TOO_LARGE));
    }
    let opaque = &sha256(run_id)[..16];
    Ok((format!("droppedneedle-library-run-{opaque}.json"), bytes))
}
