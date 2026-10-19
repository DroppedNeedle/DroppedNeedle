//! The download queue revision behind `downloads.changed`.
//!
//! SQLite triggers bump one global download activity revision whenever a
//! task is added, changes status, switches source or is deleted, and when
//! a held file comes or goes. One loop reads that revision after the
//! database changes (`super::follow`) and tells every stream when it
//! moved, so the web UI refetches its queue summary instead of polling it.
//! The event carries an opaque id only: what changed, and for whom, comes
//! from the permission-scoped summary each tab fetches.
//!
//! Byte progress does not move the revision; the worker sends it to the
//! task's owner as `download_progress`.

use super::hub::EventHub;
use super::model::{DownloadsChanged, Event};
use crate::db::{Db, DbError};
use crate::jobs::{AlreadyRunning, Supervisor};
use rusqlite::{Connection, OptionalExtension as _};
use sha2::{Digest, Sha256};

/// The loop's name on the supervisor.
pub const JOB_NAME: &str = "event-downloads";

/// Opaque event id for one revision value.
fn event_id(revision: i64) -> String {
    let digest = Sha256::digest(format!("downloads:{revision}").as_bytes());
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("downloads:{hex}")
}

/// Read the global download activity revision.
fn read_revision(conn: &Connection) -> Result<i64, DbError> {
    let revision: Option<i64> = conn
        .query_row(
            "SELECT revision FROM download_activity_global_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(revision.unwrap_or(0))
}

/// Follow the database on the supervisor until it stops or the database
/// shuts down.
pub fn start(supervisor: &Supervisor, hub: EventHub, db: Db) -> Result<(), AlreadyRunning> {
    super::follow::start(
        supervisor,
        JOB_NAME,
        db,
        |db: Db| async move { db.read("events.downloads", read_revision).await },
        move |revision: &i64| {
            hub.publish(Event::DownloadsChanged(DownloadsChanged {
                id: event_id(*revision),
            }));
        },
    )
}
