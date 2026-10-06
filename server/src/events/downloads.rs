//! The download queue poller behind `downloads.changed`.
//!
//! SQLite triggers bump one global download activity revision whenever a
//! task is added, changes status, switches source or is deleted, and when
//! a held file comes or goes. One loop reads that revision every
//! [`POLL_INTERVAL`] and tells every stream when it moved, so the web UI
//! refetches its queue summary instead of polling it. The event carries an
//! opaque id only: what changed, and for whom, comes from the
//! permission-scoped summary each tab fetches.
//!
//! Byte progress does not move the revision; the worker sends it to the
//! task's owner as `download_progress`.

use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::watch;

use super::hub::EventHub;
use super::model::{DownloadsChanged, Event};

/// How often the revision is read.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

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
async fn read_revision(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let revision: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM download_activity_global_revision WHERE singleton = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(revision.unwrap_or(0))
}

/// Run the poller until `stop` flips.
pub async fn run(hub: EventHub, pool: SqlitePool, mut stop: watch::Receiver<bool>) {
    let mut previous: Option<i64> = None;
    loop {
        if *stop.borrow() {
            break;
        }
        match read_revision(&pool).await {
            Ok(revision) if previous != Some(revision) => {
                hub.publish(Event::DownloadsChanged(DownloadsChanged {
                    id: event_id(revision),
                }));
                previous = Some(revision);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "download revision read failed; retrying"),
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
            changed = stop.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
}
