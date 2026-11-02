//! The rows of `library_catalog_actions`: the audit row a catalog decision
//! writes, and the history read from them.

pub mod history;
pub(crate) mod sql;

use rusqlite::{Transaction, params};

/// One audit row for a catalog decision.
pub(crate) struct CatalogAction<'a> {
    pub actor: &'a str,
    pub kind: &'a str,
    pub album_id: &'a str,
    pub job_id: Option<&'a str>,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: &'a str,
}

pub(crate) fn record_action(
    tx: &Transaction<'_>,
    action: CatalogAction<'_>,
    now: f64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        crate::catalog_actions::sql::RECORD_ACTION.text(),
        params![
            id,
            action.actor,
            action.kind,
            action.album_id,
            action.job_id,
            action.before.to_string(),
            action.after.to_string(),
            action.reason,
            crate::clock::ms(now),
        ],
    )?;
    Ok(id)
}

/// One audit row for a catalog decision about an artist.
pub(crate) struct ArtistAction<'a> {
    pub actor: &'a str,
    pub kind: &'a str,
    pub artist_id: &'a str,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: &'a str,
}

pub(crate) fn record_artist_action(
    tx: &Transaction<'_>,
    action: ArtistAction<'_>,
    now: f64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        crate::catalog_actions::sql::RECORD_ARTIST_ACTION.text(),
        params![
            id,
            action.actor,
            action.kind,
            action.artist_id,
            action.before.to_string(),
            action.after.to_string(),
            action.reason,
            crate::clock::ms(now),
        ],
    )?;
    Ok(id)
}
