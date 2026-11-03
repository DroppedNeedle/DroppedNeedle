//! Per-user service links: v2's JSON reshaped into each service's v3
//! record and sealed under the v3 key.
//!
//! - Last.fm becomes the plain document v3's Last.fm store reads, with the
//!   session key sealed on its own. v2 users signed with the instance app
//!   key, so `configured` stays false and v3 signs with the instance pair
//!   the settings carry.
//! - Every other service (ListenBrainz, Spotify, Navidrome, Jellyfin,
//!   Plex) is sealed whole with v2's field names kept, which is how v3
//!   stores those links.
//!
//! A repeat import compares plaintexts, never ciphertext (every seal draws
//! a fresh nonce), so an unchanged link is skipped with nothing written.
//! v2's ISO times are stored as integer milliseconds, and one that does not
//! read as a time as 0.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::sql;
use super::{CarryError, SectionResult};
use crate::export::envelope::ConnectionRecord;
use droppedneedle_auth::times::parse_iso_ms;
use droppedneedle_kernel::runtime_config::Crypto;

/// Report entity for this section.
pub(crate) const ENTITY: &str = "connection";

const LASTFM: &str = "lastfm";

/// One stored v3 row.
struct Stored {
    data: String,
    enabled: bool,
    created_at: i64,
    updated_at: i64,
}

/// The v3 stored form of one v2 link.
fn v3_data(service: &str, plaintext: &str, crypto: &Crypto) -> Result<Option<String>, CarryError> {
    if service != LASTFM {
        return crypto
            .encrypt(plaintext)
            .map(Some)
            .map_err(|_| CarryError::Rekey);
    }
    let Ok(doc) = serde_json::from_str::<Value>(plaintext) else {
        return Ok(None);
    };
    let session_key = doc
        .get("session_key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(|key| crypto.encrypt(key).map_err(|_| CarryError::Rekey))
        .transpose()?;
    Ok(Some(
        json!({
            "configured": false,
            "api_key": null,
            "shared_secret": null,
            "username": doc.get("username").and_then(Value::as_str),
            "session_key": session_key,
        })
        .to_string(),
    ))
}

/// Plaintext view of an incoming v2 link, shaped like [`stored_view`].
fn incoming_view(service: &str, plaintext: &str) -> Option<Value> {
    let doc: Value = serde_json::from_str(plaintext).ok()?;
    if service != LASTFM {
        return Some(doc);
    }
    Some(json!({
        "configured": false,
        "api_key": null,
        "shared_secret": null,
        "username": doc.get("username").and_then(Value::as_str),
        "session_key": doc.get("session_key").and_then(Value::as_str).filter(|key| !key.is_empty()),
    }))
}

/// Plaintext view of a stored v3 row; `None` when it does not open.
fn stored_view(service: &str, data: &str, crypto: &Crypto) -> Option<Value> {
    if service != LASTFM {
        let plaintext = crypto.decrypt(data).ok()?;
        return serde_json::from_str(&plaintext).ok();
    }
    let doc: Value = serde_json::from_str(data).ok()?;
    let open = |field: &str| -> Option<Option<String>> {
        match doc.get(field).and_then(Value::as_str) {
            None => Some(None),
            Some(cipher) => crypto.decrypt(cipher).ok().map(Some),
        }
    };
    Some(json!({
        "configured": doc.get("configured").and_then(Value::as_bool).unwrap_or(false),
        "api_key": open("api_key")?,
        "shared_secret": open("shared_secret")?,
        "username": doc.get("username").and_then(Value::as_str),
        "session_key": open("session_key")?,
    }))
}

/// Decide and (on a real run) write every link.
pub(crate) fn apply(
    conn: &Connection,
    records: &[ConnectionRecord],
    unsealed: &HashMap<String, String>,
    crypto: &Crypto,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let stored: HashMap<(String, String), Stored> =
        sql::all_rows(conn, sql::CONNECTIONS, sql::ConnectionRow::read)?
            .into_iter()
            .map(|row| {
                (
                    (row.user_id, row.service),
                    Stored {
                        data: row.connection_data,
                        enabled: row.enabled != 0,
                        created_at: row.created_at,
                        updated_at: row.updated_at,
                    },
                )
            })
            .collect();
    let mut result = SectionResult {
        rows: records.len() as u64,
        ..SectionResult::default()
    };
    for (index, record) in records.iter().enumerate() {
        let key = format!("{}|{}", record.user_id, record.service);
        let plaintext = unsealed
            .get(&format!("user_connections[{index}].data"))
            .ok_or(CarryError::Rekey)?;
        let created_at = parse_iso_ms(&record.created_at).unwrap_or(0);
        let updated_at = parse_iso_ms(&record.updated_at).unwrap_or(0);
        let existing = stored.get(&(record.user_id.clone(), record.service.clone()));
        if let Some(existing) = existing {
            let same = existing.enabled == record.enabled
                && existing.created_at == created_at
                && existing.updated_at == updated_at
                && incoming_view(&record.service, plaintext).is_some()
                && stored_view(&record.service, &existing.data, crypto)
                    == incoming_view(&record.service, plaintext);
            if same {
                result.counts.skipped_identical += 1;
            } else {
                result.counts.conflict_kept_existing += 1;
                result.note(
                    key,
                    "conflict_kept_existing",
                    "already linked in v3; the v3 link is kept",
                );
            }
            continue;
        }
        let Some(data) = v3_data(&record.service, plaintext, crypto)? else {
            result.counts.dropped_invalid += 1;
            result.note(
                key,
                "dropped_invalid",
                "the v2 link is not readable JSON; link again in v3",
            );
            continue;
        };
        result.counts.imported += 1;
        result.secrets += 1;
        if dry_run {
            continue;
        }
        conn.execute(
            sql::INSERT_CONNECTION.text(),
            params![
                record.user_id,
                record.service,
                data,
                i64::from(record.enabled),
                created_at,
                updated_at,
            ],
        )?;
        result.written += 1;
    }
    Ok(result)
}
