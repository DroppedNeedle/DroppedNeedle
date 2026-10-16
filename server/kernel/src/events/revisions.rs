//! The library revision poller behind `activity.changed`.
//!
//! Port of v2's `library_revision_poller.py`: one process-wide loop reads
//! the durable library revisions (scan, identification and operation
//! streams, plus the catalog revision) every [`POLL_INTERVAL`] and
//! publishes `activity.changed` only when they moved. Every stream shares
//! that one read instead of polling per connection. A scan changing state
//! pokes the loop so it reads at once instead of at its next tick.
//!
//! The loop survives database errors (logged, retried next tick) and stops
//! when the shutdown signal flips. `serve` closes the hub on that same
//! signal.

use std::collections::BTreeMap;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::sync::watch;

use super::hub::EventHub;
use super::model::{ActivityChanged, Event};

/// How often the revisions are read (v2 `ACTIVITY_POLL_INTERVAL_SECONDS`).
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

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
pub async fn read_revisions(pool: &SqlitePool) -> Result<BTreeMap<String, u64>, sqlx::Error> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT stream_kind, value FROM library_event_stream_revisions")
            .fetch_all(pool)
            .await?;
    let mut revisions: BTreeMap<String, u64> = STREAMS
        .iter()
        .map(|stream| ((*stream).to_owned(), 0))
        .collect();
    for (stream, value) in rows {
        revisions.insert(stream, u64::try_from(value).unwrap_or(0));
    }
    let catalog: Option<i64> =
        sqlx::query_scalar("SELECT value FROM library_catalog_revision WHERE singleton = 1")
            .fetch_optional(pool)
            .await?;
    revisions.insert(
        "catalog".to_owned(),
        catalog
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0),
    );
    Ok(revisions)
}

/// Run the poller until `stop` flips.
pub async fn run(hub: EventHub, pool: SqlitePool, mut stop: watch::Receiver<bool>) {
    let mut previous: Option<BTreeMap<String, u64>> = None;
    loop {
        if *stop.borrow() {
            break;
        }
        match read_revisions(&pool).await {
            Ok(revisions) if previous.as_ref() != Some(&revisions) => {
                hub.publish(Event::ActivityChanged(ActivityChanged {
                    id: revision_event_id(&revisions),
                    revisions: revisions.clone(),
                }));
                previous = Some(revisions);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "library revision read failed; retrying"),
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
            _ = hub.activity_poked() => {}
            changed = stop.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
}
