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

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::store::{self, OpenFinding};
use crate::identify::sqlite::offer_album;
use crate::operations::choice;
use crate::operations::decisions::{
    CatalogAction, TrackRow, bump_catalog, record_action, upsert_track_identity,
};
use crate::operations::models::OperationError;

/// Reason an album's match state carries after a recheck detached it.
pub const DETACH_REASON: &str = "IDENTITY_FAILED_RECHECK";

/// Apply the finding for one claimed album and close its work item.
#[allow(clippy::too_many_arguments)]
pub fn apply_album(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    ordinal: i64,
    album_id: &str,
    actor: Option<&str>,
    now: f64,
) -> Result<(), OperationError> {
    let finding = store::open_finding(tx, job_id, album_id)?;
    let outcome = match &finding {
        Some(finding) if store::guard_holds(tx, album_id, finding.payload.guard.as_ref())? => {
            apply_finding(tx, job_id, album_id, finding, actor, now)?
        }
        _ => None,
    };
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
    Ok(())
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
            let detached = choice::detach(tx, album_id, DETACH_REASON, actor, Some(job_id), now)?;
            if detached.is_some() {
                offer_album(tx, album_id, crate::clock::now_ms() as i64)?;
            }
            Ok(detached.map(|_| "DETACHED"))
        }
        "mapping_ready" => place(tx, job_id, album_id, finding, actor, now),
        "exact_release_suggested" => {
            let Some(candidate) = finding.payload.candidate.as_ref() else {
                return Ok(None);
            };
            match choice::choose(tx, album_id, candidate, actor, now) {
                Ok(_) => Ok(Some("EDITION_ACCEPTED")),
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
    let source: Option<String> = tx
        .query_row(
            "SELECT decision_source FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = 'musicbrainz' AND release_mbid = ?2",
            params![album_id, release],
            |row| row.get(0),
        )
        .optional()?;
    let Some(source) = source else {
        return Ok(None);
    };
    for placed in &payload.mapping {
        upsert_track_identity(
            tx,
            &TrackRow {
                local_track_id: &placed.local_track_id,
                recording_mbid: &placed.recording_mbid,
                release_mbid: Some(release),
                release_track_mbid: Some(&placed.release_track_mbid),
                medium_position: Some(placed.medium_position),
                release_track_position: Some(placed.release_track_position),
                decision_source: &source,
            },
            now,
        )?;
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
