//! Which chart rows the library already holds.
//!
//! Charts and the queue mark rows as owned (v2 `_mark_*_ownership`): an
//! album when an owned library album is identified as its release group,
//! an artist when an owned library artist carries its MBID. The local id
//! comes back too, so the frontend can link straight to the library page.

use std::collections::HashMap;

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::db::statement::ListLen;

use crate::reads::discover::ports::ProviderFailure;
use crate::reads::discover::{read_all, sql};

/// Most ids one lookup binds; chart pages are at most 100 rows.
const MAX_IDS: usize = 200;

/// Lowercased release-group MBID to the oldest owned album identified as
/// it.
pub async fn owned_albums(
    db: &Db,
    mbids: &[&str],
) -> Result<HashMap<String, String>, ProviderFailure> {
    lookup(db, mbids, sql::owned_albums).await
}

/// Lowercased artist MBID to the owned local artist carrying it.
pub async fn owned_artists(
    db: &Db,
    mbids: &[&str],
) -> Result<HashMap<String, String>, ProviderFailure> {
    lookup(db, mbids, sql::owned_artists).await
}

async fn lookup(
    db: &Db,
    mbids: &[&str],
    statement: fn(ListLen) -> String,
) -> Result<HashMap<String, String>, ProviderFailure> {
    let mut wanted: Vec<String> = mbids
        .iter()
        .map(|mbid| mbid.trim().to_ascii_lowercase())
        .filter(|mbid| !mbid.is_empty())
        .collect();
    wanted.sort();
    wanted.dedup();
    wanted.truncate(MAX_IDS);
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = read_all(
        db,
        "discover.ownership",
        statement(ListLen::new(wanted.len())),
        rusqlite::params_from_iter(wanted),
        sql::OwnedRow::read,
    )
    .await
    .map_err(|error| ProviderFailure::failed(format!("library ownership: {error}")))?;
    Ok(rows
        .into_iter()
        .map(|row| (row.mbid.to_ascii_lowercase(), row.local_id))
        .collect())
}
