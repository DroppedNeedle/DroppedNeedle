//! The library revisions behind `activity.changed`.
//!
//! Port of v2's `library_revision_poller.py`, woken by commits instead of a
//! timer: one process-wide loop reads the durable library revisions (scan,
//! identification and operation streams, plus the catalog revision) after
//! the database changes and publishes `activity.changed` only when they
//! moved. Every stream shares that one read. Pacing, retries and the end of
//! the loop are in `super::follow`.

use std::collections::BTreeMap;

use super::hub::EventHub;
use super::model::{ActivityChanged, Event};
use crate::db::{Db, DbError};
use crate::jobs::{AlreadyRunning, Supervisor};
use rusqlite::{Connection, OptionalExtension as _};
use sha2::{Digest, Sha256};

/// The loop's name on the supervisor.
pub const JOB_NAME: &str = "event-revisions";

/// The streams every revision map carries, even before their first bump.
const STREAMS: [&str; 3] = ["scan", "identification", "operation"];

/// Stable id for one revision map (v2 `_revision_event_id`): `activity:`
/// plus the first 16 hex digits of the sha256 of the sorted map.
pub fn revision_event_id(revisions: &BTreeMap<String, u64>) -> String {
    let sorted: Vec<(&String, &u64)> = revisions.iter().collect();
    let encoded = serde_json::to_string(&sorted).unwrap_or_default();
    let digest = Sha256::digest(encoded.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("activity:{}", &hex[..16])
}

/// Read every library revision in one go.
fn read_revisions(conn: &Connection) -> Result<BTreeMap<String, u64>, DbError> {
    let mut revisions: BTreeMap<String, u64> = STREAMS
        .iter()
        .map(|stream| ((*stream).to_owned(), 0))
        .collect();
    let mut statement =
        conn.prepare("SELECT stream_kind, value FROM library_event_stream_revisions")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (stream, value) = row?;
        revisions.insert(stream, u64::try_from(value).unwrap_or(0));
    }
    let catalog: Option<i64> = conn
        .query_row(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    revisions.insert(
        "catalog".to_owned(),
        catalog
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0),
    );
    Ok(revisions)
}

/// Follow the database on the supervisor until it stops or the database
/// shuts down.
pub fn start(supervisor: &Supervisor, hub: EventHub, db: Db) -> Result<(), AlreadyRunning> {
    super::follow::start(
        supervisor,
        JOB_NAME,
        db,
        |db: Db| async move { db.read("events.revisions", read_revisions).await },
        move |revisions: &BTreeMap<String, u64>| {
            hub.publish(Event::ActivityChanged(ActivityChanged {
                id: revision_event_id(revisions),
                revisions: revisions.clone(),
            }));
        },
    )
}
