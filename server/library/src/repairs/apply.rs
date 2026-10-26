//! Apply: carry out one album's finding from a ready report, inside the
//! caller's transaction. Nothing happens to an album that changed since it
//! was checked; its finding turns stale with `STALE_SUBJECT`.
//!
//! - `safe_detach` removes the failed automatic match through the edition
//!   module's detach, which keeps an undo, and queues the album to be
//!   matched again.
//! - `mapping_ready` places each file on its track of the album's release.
//! - `exact_release_suggested` chooses the suggested edition, as a person
//!   choosing it on the album page would (with the same undo).

use rusqlite::Transaction;

use super::store::{self, OpenFinding};
use crate::catalog_revision::bump_catalog;
use crate::identify::queue::queue_retry;
use crate::operations::choice;
use crate::operations::decisions::{CatalogAction, record_action};
use crate::operations::models::OperationError;

/// Reason an album's match state carries after a recheck detached it.
pub const DETACH_REASON: &str = "IDENTITY_FAILED_RECHECK";

/// What an applied finding that accepted an edition settles as.
const EDITION_ACCEPTED: &str = "EDITION_ACCEPTED";

/// Apply the finding for one claimed album and close its work item. True
/// when that accepted an edition for the album.
#[allow(clippy::too_many_arguments)]
pub fn apply_album(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    ordinal: i64,
    album_id: &str,
    actor: Option<&str>,
    now: f64,
) -> Result<bool, OperationError> {
    let finding = store::open_finding(tx, job_id, album_id)?;
    let outcome = match &finding {
        Some(finding) if store::guard_holds(tx, album_id, finding.payload.guard.as_ref())? => {
            apply_finding(tx, job_id, album_id, finding, actor, now)?
        }
        _ => None,
    };
    let accepted = outcome == Some(EDITION_ACCEPTED);
    match (finding, outcome) {
        (Some(finding), Some(result)) => {
            store::settle_finding(tx, &finding.id, "applied", result, now)?;
            store::finish_work(tx, job_id, worker, ordinal, "succeeded", None, None, now)?;
        }
        (finding, _) => {
            if let Some(finding) = finding {
                store::settle_finding(tx, &finding.id, "stale", "STALE_SUBJECT", now)?;
            }
            store::finish_work(
                tx,
                job_id,
                worker,
                ordinal,
                "skipped",
                Some("STALE_SUBJECT"),
                None,
                now,
            )?;
        }
    }
    Ok(accepted)
}

/// The change itself. `None` when the album no longer takes it.
fn apply_finding(
    tx: &Transaction<'_>,
    job_id: &str,
    album_id: &str,
    finding: &OpenFinding,
    actor: Option<&str>,
    now: f64,
) -> Result<Option<&'static str>, OperationError> {
    match finding.finding_code.as_str() {
        "safe_detach" => {
            let detached = crate::identity::remove(
                tx,
                album_id,
                crate::identity::Removal::Repair {
                    reason: DETACH_REASON,
                    actor,
                    job_id: Some(job_id),
                },
                now,
            )?;
            // Matched again even when its files were identified before:
            // the detach's undo is carried through this identification.
            if detached.is_some() {
                queue_retry(tx, album_id, actor, crate::clock::ms(now))?;
            }
            Ok(detached.map(|_| "DETACHED"))
        }
        "mapping_ready" => place(tx, job_id, album_id, finding, actor, now),
        "exact_release_suggested" => {
            let Some(candidate) = finding.payload.candidate.as_ref() else {
                return Ok(None);
            };
            match choice::choose(tx, album_id, candidate, actor, now) {
                Ok(_) => Ok(Some(EDITION_ACCEPTED)),
                // The edition no longer fits the files: leave the album.
                Err(OperationError::Invalid(_) | OperationError::NotFound(_)) => Ok(None),
                Err(other) => Err(other),
            }
        }
        _ => Ok(None),
    }
}

/// Place each file on its track of the album's release. The track rows
/// keep the album's decision source, so an automatic album stays
/// automatic and a chosen one stays chosen.
fn place(
    tx: &Transaction<'_>,
    job_id: &str,
    album_id: &str,
    finding: &OpenFinding,
    actor: Option<&str>,
    now: f64,
) -> Result<Option<&'static str>, OperationError> {
    let payload = &finding.payload;
    let (Some(release), false) = (payload.release_mbid.as_deref(), payload.mapping.is_empty())
    else {
        return Ok(None);
    };
    let spots: Vec<crate::identity::Spot<'_>> = payload
        .mapping
        .iter()
        .map(|placed| crate::identity::Spot {
            local_track_id: &placed.local_track_id,
            recording_mbid: &placed.recording_mbid,
            release_track_mbid: &placed.release_track_mbid,
            disc: Some(placed.medium_position),
            position: Some(placed.release_track_position),
        })
        .collect();
    if !crate::identity::place_mapping(tx, album_id, release, &spots, now)? {
        return Ok(None);
    }
    record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind: "accept_management_track_mappings",
            album_id,
            job_id: Some(job_id),
            before: serde_json::json!({ "release_mbid": release }),
            after: serde_json::to_value(&payload.mapping).unwrap_or_default(),
            reason: "EXACT_RELEASE_MAPPINGS_ACCEPTED",
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(Some("MAPPINGS_ACCEPTED"))
}
