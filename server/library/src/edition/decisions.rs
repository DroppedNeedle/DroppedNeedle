//! A person's decisions on the catalog: a custom edition or "leave
//! unmanaged" made directly on an album, and undoing an automatic edition.
//! Each checks what the person was shown and runs the identity module's
//! command in the caller's transaction.

use rusqlite::{Transaction, params};

use crate::edition::models::UndoOutcome;
use crate::identify::models::EvidenceClass;
use crate::identity::{self, AutomaticUndo, AutomaticUndone, Custom};
use crate::jobs::models::OperationError;
use crate::reasons;
use crate::reidentify::models::ReidentificationCandidate;
use crate::reidentify::store::album_revisions;

pub(crate) use crate::identity::reads::indexed_track_ids;
pub use crate::identity::reads::live_automatic_edition_undo;

/// What a person saw of the album before asking for a custom edition: its
/// identity revision (0 with none), and its row and input revisions read
/// before MusicBrainz was asked.
#[derive(Debug, Clone)]
pub struct SealGuard {
    pub identity_revision: i64,
    pub album_revision: i64,
    pub input_revision: String,
}

fn is_mbid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

/// The candidate's release group is a real MBID and its album title and
/// artist do not contradict the files.
fn names_hold(candidate: &ReidentificationCandidate) -> bool {
    is_mbid(&candidate.evidence.release_group_mbid)
        && candidate.album_title_classification != EvidenceClass::Contradictory
        && candidate.album_artist_classification != EvidenceClass::Contradictory
}

/// A person seals the album's files as they are under the release's group
/// (a custom edition, [`identity::custom_edition`]), directly on the album.
/// Refused as stale, with nothing changed, when the album moved since
/// `guard` was read.
pub fn custom_edition(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    guard: &SealGuard,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let now_revisions =
        album_revisions(tx, album_id)?.ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
    if now_revisions.album_revision != guard.album_revision
        || now_revisions.input_revision != guard.input_revision
        || identity::reads::identity_revision(tx, album_id)? != guard.identity_revision
    {
        return Err(OperationError::Conflict(reasons::EDITION_CHANGED));
    }
    if !names_hold(candidate) {
        return Err(OperationError::Invalid(
            reasons::CUSTOM_EDITION_NAMES_CONFLICT,
        ));
    }
    let recordings: Vec<(String, String)> = candidate
        .evidence
        .track_evidence
        .iter()
        .filter(|evidence| evidence.classification == EvidenceClass::Supported)
        .filter_map(|evidence| {
            Some((
                evidence.local_track_id.clone(),
                evidence.recording_mbid.clone()?,
            ))
        })
        .collect();
    Ok(identity::custom_edition(
        tx,
        album_id,
        &Custom {
            release_group_mbid: &candidate.evidence.release_group_mbid,
            candidate_key: &candidate.candidate_key,
            artist_mbid: candidate.artist_mbid.as_deref(),
            recordings: &recordings,
            input_revision: &guard.input_revision,
        },
        actor,
        now,
    )?)
}

/// A person leaves the album unmanaged, directly on the album
/// ([`identity::leave_unmanaged`]).
pub fn leave_album_unmanaged(
    tx: &Transaction<'_>,
    album_id: &str,
    expected_identity_revision: i64,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    if album_revisions(tx, album_id)?.is_none() {
        return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
    }
    Ok(identity::leave_unmanaged(
        tx,
        album_id,
        expected_identity_revision,
        actor,
        now,
    )?)
}

/// Undo the album's last automatic edition ([`identity::undo_automatic`]).
/// With no earlier album identity, the album goes to review instead.
pub fn undo_automatic_edition(
    tx: &Transaction<'_>,
    album_id: &str,
    expected_album_revision: i64,
    expected_identity_revision: i64,
    actor: &str,
    now: f64,
) -> Result<UndoOutcome, OperationError> {
    let review_id = uuid::Uuid::new_v4().to_string();
    let outcome = identity::undo_automatic(
        tx,
        album_id,
        &AutomaticUndo {
            expected_album_revision,
            expected_identity_revision,
            actor,
            review_id: &review_id,
            now,
        },
    )?;
    let review_id = match outcome {
        AutomaticUndone::Restored => None,
        AutomaticUndone::ClearedToReview => {
            tx.execute(
                crate::edition::sql::UNDO_AUTOMATIC_EDITION.text(),
                params![review_id, album_id, crate::clock::ms(now)],
            )?;
            Some(review_id)
        }
    };
    Ok(UndoOutcome {
        local_album_id: album_id.to_owned(),
        outcome,
        review_id,
    })
}
