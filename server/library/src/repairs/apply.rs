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

use super::models::{ApplyResult, FindingCode, FindingState};
use super::store::{self, OpenFinding};
use crate::catalog_revision::bump_catalog;
use crate::identify::queue::queue_retry;
use crate::jobs::WorkState;
use crate::operations::choice;
use crate::operations::decisions::{CatalogAction, record_action};
use crate::operations::models::OperationError;

/// Reason an album's match state carries after a recheck detached it.
pub const DETACH_REASON: &str = "IDENTITY_FAILED_RECHECK";

/// Why Apply left an album whose finding does not decode.
const FINDING_UNREADABLE: &str = "FINDING_UNREADABLE";

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
    let finding = match store::open_finding(tx, job_id, album_id)? {
        Some(Ok(finding)) => Some(finding),
        // Logged where it was read; the finding stays open.
        Some(Err(_)) => {
            store::finish_work(
                tx,
                job_id,
                worker,
                ordinal,
                WorkState::Failed,
                Some(FINDING_UNREADABLE),
                None,
                now,
            )?;
            return Ok(false);
        }
        None => None,
    };
    let outcome = match &finding {
        Some(finding) if store::guard_holds(tx, album_id, finding.payload.guard.as_ref())? => {
            apply_finding(tx, job_id, album_id, finding, actor, now)?
        }
        _ => None,
    };
    let accepted = outcome == Some(ApplyResult::EditionAccepted);
    match (finding, outcome) {
        (Some(finding), Some(result)) => {
            store::settle_finding(tx, &finding.id, FindingState::Applied, result, now)?;
            store::finish_work(
                tx,
                job_id,
                worker,
                ordinal,
                WorkState::Succeeded,
                None,
                None,
                now,
            )?;
        }
        (finding, _) => {
            let stale = ApplyResult::StaleSubject;
            if let Some(finding) = finding {
                store::settle_finding(tx, &finding.id, FindingState::Stale, stale, now)?;
            }
            store::finish_work(
                tx,
                job_id,
                worker,
                ordinal,
                WorkState::Skipped,
                Some(stale.as_str()),
                None,
                now,
            )?;
        }
    }
    Ok(accepted)
}

/// The change itself. `None` when the album no longer takes it, or the
/// finding is not one Apply acts on.
fn apply_finding(
    tx: &Transaction<'_>,
    job_id: &str,
    album_id: &str,
    finding: &OpenFinding,
    actor: Option<&str>,
    now: f64,
) -> Result<Option<ApplyResult>, OperationError> {
    match finding.finding_code {
        FindingCode::SafeDetach => {
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
            Ok(detached.map(|_| ApplyResult::Detached))
        }
        FindingCode::MappingReady => place(tx, job_id, album_id, finding, actor, now),
        FindingCode::ExactReleaseSuggested => {
            let Some(candidate) = finding.payload.candidate.as_ref() else {
                return Ok(None);
            };
            match choice::choose(tx, album_id, candidate, actor, now) {
                Ok(_) => Ok(Some(ApplyResult::EditionAccepted)),
                // The edition no longer fits the files: leave the album.
                Err(OperationError::Invalid(_) | OperationError::NotFound(_)) => Ok(None),
                Err(other) => Err(other),
            }
        }
        // Only the three above are ever marked eligible.
        FindingCode::Valid
        | FindingCode::ManualIdentity
        | FindingCode::NeedsReview
        | FindingCode::Unverifiable
        | FindingCode::Stale
        | FindingCode::Ready
        | FindingCode::ExactReleaseRequired
        | FindingCode::ExactReleaseAutoAccepted => Ok(None),
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
) -> Result<Option<ApplyResult>, OperationError> {
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
    Ok(Some(ApplyResult::MappingsAccepted))
}
