//! Choosing an album's edition: what a person asks for on the album page
//! (pick, confirm, undo, hand back) and what a repair or a review does to
//! the match, turned into the identity module's commands.
//!
//! The commands themselves, what each writes and what its undo brings
//! back, live in [`crate::identity`]. Here a MusicBrainz candidate becomes
//! the identity module's [`Offer`], refusals become the API's reasons, and
//! a hand back queues the identification it promises. Each function runs
//! inside the caller's transaction.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::models::{EditionChoice, OperationError, PlacedFile, ReidentificationCandidate};
use super::reasons;
use super::store::PROVIDER;
use crate::corrections::continuation::continue_replaced;
use crate::identify::models::EvidenceClass;
use crate::identify::queue::queue_retry;
use crate::identity::{
    self, IdentityError, Offer, Placed, Refusal, Removal, TrackRow, upsert_track_identity,
};

pub(crate) use crate::identity::reads::mapped_track_ids;
pub use crate::identity::reads::{identity_revision, undo_available};
pub use crate::identity::remap::{
    CHOSEN_FITS_NO_FILE, CHOSEN_NOT_LISTED, PendingRemap, REMAP_RETRY_MAX_SECS, REMAP_RETRY_SECS,
    RemapOrigin, drop_remap, give_up_remap, keep_unplaceable_choice, next_remap, queue_placement,
    queue_remap, remap_still_holds, remap_still_wanted, retry_remap,
};

impl From<IdentityError> for OperationError {
    fn from(error: IdentityError) -> Self {
        match error {
            IdentityError::Db(error) => Self::from(error),
            IdentityError::Refused(refusal) => match refusal {
                Refusal::AlbumNotFound => Self::NotFound(reasons::ALBUM_NOT_FOUND),
                Refusal::EditionFitsNoFile => Self::Invalid(reasons::EDITION_FITS_NO_FILE),
                Refusal::NoMatchToConfirm => Self::Conflict(reasons::NO_MATCH_TO_CONFIRM),
                Refusal::NothingToConfirm => Self::Conflict(reasons::NOTHING_TO_CONFIRM),
                Refusal::NoEditionChoice => Self::NotFound(reasons::NO_EDITION_CHOICE),
                Refusal::EditionChoiceStale => Self::Conflict(reasons::EDITION_CHOICE_STALE),
                Refusal::NoAutomaticEdition => Self::NotFound(reasons::NO_AUTOMATIC_EDITION),
                Refusal::AutomaticUndoStale => Self::Conflict(reasons::UNDO_STALE),
                Refusal::EditionChanged => Self::Conflict(reasons::EDITION_CHANGED),
                Refusal::CustomEditionGroupConflict => {
                    Self::Invalid(reasons::CUSTOM_EDITION_GROUP_CONFLICT)
                }
                Refusal::CustomEditionNamesMissing => {
                    Self::Invalid(reasons::CUSTOM_EDITION_NAMES_MISSING)
                }
                Refusal::CustomEditionArtistConflict => {
                    Self::Invalid(reasons::CUSTOM_EDITION_ARTIST_CONFLICT)
                }
                Refusal::CustomEditionPositions => Self::Invalid(reasons::CUSTOM_EDITION_POSITIONS),
            },
        }
    }
}

/// The spot on the candidate's release each file the evidence supports
/// could take: its recording, release track, disc and position.
fn spots(candidate: &ReidentificationCandidate) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::new();
    for evidence in &candidate.evidence.track_evidence {
        let file = &evidence.local_track_id;
        if out.iter().any(|spot| spot.local_track_id == *file) {
            continue;
        }
        // The first evidence for a file decides it.
        let first = candidate
            .evidence
            .track_evidence
            .iter()
            .find(|seen| seen.local_track_id == *file)
            .filter(|seen| seen.classification == EvidenceClass::Supported);
        let spot = candidate
            .tracks
            .iter()
            .find(|track| track.local_track_id == *file);
        let placed = first.zip(spot).and_then(|(evidence, spot)| {
            Some(Placed {
                local_track_id: file.clone(),
                recording_mbid: evidence.recording_mbid.clone()?,
                release_track_mbid: evidence.release_track_mbid.clone()?,
                disc: spot.disc_number.filter(|disc| *disc > 0)?,
                position: spot.position.filter(|position| *position > 0)?,
            })
        });
        out.extend(placed);
    }
    out
}

/// Place each indexed file on its own track of the candidate's release
/// where the evidence supports it. Returns the placements and the files
/// left over. Two files never share one release track. The edition plan
/// uses the same placement, so its preview is what a choice would write.
pub(super) fn place(
    indexed: &[String],
    candidate: &ReidentificationCandidate,
) -> (Vec<Placed>, Vec<String>) {
    identity::place(indexed, &spots(candidate))
}

/// The candidate as an offer of its release for the album's files.
/// `EDITION_NOT_FOUND` when it names no release.
pub(crate) fn offer(candidate: &ReidentificationCandidate) -> Result<Offer, OperationError> {
    let release_mbid = candidate
        .evidence
        .release_mbid
        .clone()
        .ok_or(OperationError::NotFound(reasons::EDITION_NOT_FOUND))?;
    Ok(offer_on(candidate, &release_mbid))
}

/// The candidate as an offer of `release_mbid`, whatever release the
/// candidate names: placing files on an edition the album already has
/// reads only the candidate's spots.
pub(crate) fn offer_on(candidate: &ReidentificationCandidate, release_mbid: &str) -> Offer {
    Offer {
        release_group_mbid: candidate.evidence.release_group_mbid.clone(),
        release_mbid: release_mbid.to_owned(),
        spots: spots(candidate),
        weighed: candidate
            .evidence
            .track_evidence
            .iter()
            .map(|evidence| evidence.local_track_id.clone())
            .collect(),
    }
}

fn placed_files(tx: &Transaction<'_>, placed: &[String]) -> rusqlite::Result<Vec<PlacedFile>> {
    let mut out = Vec::new();
    for track_id in placed {
        let file = tx
            .query_row(
                "SELECT root_id, relative_path FROM local_tracks WHERE id = ?1",
                params![track_id],
                |row| {
                    Ok(PlacedFile {
                        local_track_id: track_id.clone(),
                        root_id: row.get(0)?,
                        relative_path: row.get(1)?,
                    })
                },
            )
            .optional()?;
        out.extend(file);
    }
    Ok(out)
}

/// A person (or, with no `actor`, a repair) chooses the album's edition
/// ([`identity::choose`]). Any release can be chosen, from the album's own
/// release group or another one, and the files need not fit it one to one.
pub fn choose(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    actor: Option<&str>,
    now: f64,
) -> Result<EditionChoice, OperationError> {
    let offer = offer(candidate)?;
    let chosen = identity::choose(
        tx,
        album_id,
        &offer,
        &candidate.candidate_key,
        actor,
        candidate.unmatched_expected_tracks.len(),
        now,
    )?;
    Ok(EditionChoice {
        local_album_id: album_id.to_owned(),
        release_group_mbid: chosen.release_group_mbid,
        release_mbid: chosen.release_mbid,
        placed: placed_files(tx, &chosen.placed)?,
        extra_track_ids: chosen.extra,
        missing_titles: candidate.unmatched_expected_tracks.clone(),
    })
}

/// "Let DroppedNeedle choose": the edition goes back to automatic best fit
/// ([`identity::hand_back`]) and identification looks at the album again,
/// in the same transaction, whether or not it had an edition to hand back.
pub fn hand_back(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    identity::hand_back(tx, album_id, actor, now)?;
    queue_retry(tx, album_id, Some(actor), (now * 1000.0) as i64)?;
    Ok(())
}

/// "Looks right" ([`identity::confirm`]).
pub fn confirm(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    Ok(identity::confirm(tx, album_id, actor, now)?)
}

/// Take back the album's last edition change ([`identity::undo`]).
pub fn undo(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    Ok(identity::undo(tx, album_id, actor, now)?)
}

/// Take an automatic match off an album that failed its recheck
/// ([`Removal::Repair`]); the caller queues it to be matched again. `None`
/// when the album has no identity or a person's. Returns the audit row id.
pub fn detach(
    tx: &Transaction<'_>,
    album_id: &str,
    reason_code: &str,
    actor: Option<&str>,
    job_id: Option<&str>,
    now: f64,
) -> Result<Option<String>, OperationError> {
    Ok(identity::remove(
        tx,
        album_id,
        Removal::Repair {
            reason: reason_code,
            actor,
            job_id,
        },
        now,
    )?)
}

/// A person takes the match off an album from its review, whoever made it
/// ([`Removal::Person`]). `None` when the album has no identity.
pub fn detach_by_person(
    tx: &Transaction<'_>,
    album_id: &str,
    reason_code: &str,
    actor: &str,
    now: f64,
) -> Result<Option<String>, OperationError> {
    Ok(identity::remove(
        tx,
        album_id,
        Removal::Person {
            reason: reason_code,
            actor,
        },
        now,
    )?)
}

/// Accept an edition for an album without a person
/// ([`identity::accept_automatic`]). `None` when the album holds a
/// person's choice, the candidate names no release, the edition fits no
/// file, or the album no longer looks as `expected` says.
pub fn accept_automatic(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    reason_code: &str,
    job_id: Option<&str>,
    expected: Option<&identity::Expected>,
    now: f64,
) -> Result<Option<String>, OperationError> {
    let Some(release) = candidate.evidence.release_mbid.clone() else {
        return Ok(None);
    };
    let indexed = crate::identity::reads::indexed_track_ids(tx, album_id)?;
    let (placed, extra) = place(&indexed, candidate);
    if placed.is_empty() {
        return Ok(None);
    }
    Ok(identity::accept_automatic(
        tx,
        album_id,
        &identity::Accepted {
            release_group_mbid: &candidate.evidence.release_group_mbid,
            release_mbid: &release,
            placed: &placed,
            extra: &extra,
            job_id,
            reason_code,
            expected,
            now,
        },
    )?)
}

/// Place files an import just brought onto the album's edition, in the
/// import's catalog transaction, from the import's own match: each file is
/// upserted as a `manual` placement on its release track, as
/// [`place_files`] writes one, naming the release as the album's identity
/// row spells it. Only when the album's match is a person's (chosen by
/// hand or carried from v2), names `release_mbid`, and no custom edition is
/// active; otherwise nothing is placed. A release track another indexed
/// file of the album already holds (naming the release or none) is never
/// given away: that file stays on the album, unplaced. When a file was
/// placed, a match state left by an earlier failed placement
/// (`CHOSEN_EDITION_*`) goes. Missing tracks then continue into the new
/// ones ([`continue_tracks`]), last in the transaction. A file that
/// replaced a track of the album as another version takes what people
/// made on it ([`continue_replaced`]), whatever the album's match source,
/// unless another file holds its release track. No audit row is written.
/// Returns the number of files placed and left unplaced.
pub(crate) fn place_imported(
    tx: &Transaction<'_>,
    album_id: &str,
    release_mbid: &str,
    files: &[(String, crate::publish::planner::JoinedTrack)],
    now: f64,
) -> rusqlite::Result<(usize, usize)> {
    let chosen: Option<String> = tx
        .query_row(
            &format!(
                "SELECT release_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2 \
                 AND decision_source IN {} \
                 AND lower(release_mbid) = lower(?3) \
                 AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
                 WHERE c.local_album_id = ?1)",
                identity::sql::PERSONS
            ),
            params![album_id, PROVIDER, release_mbid],
            |row| row.get(0),
        )
        .optional()?;
    let replacing = files.iter().any(|(_, track)| track.replaced.is_some());
    if chosen.is_none() && !replacing {
        return Ok((0, 0));
    }
    let release = chosen.clone().unwrap_or_else(|| release_mbid.to_owned());
    let mut replaced = Vec::new();
    let (mut placed, mut left) = (0usize, 0usize);
    for (track_id, track) in files {
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_tracks t \
             JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = ?2 \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' AND t.id <> ?3 \
             AND lower(i.release_track_mbid) = lower(?4) \
             AND (i.release_mbid IS NULL OR lower(i.release_mbid) = lower(?5)))",
            params![
                album_id,
                PROVIDER,
                track_id,
                track.release_track_mbid,
                release
            ],
            |row| row.get(0),
        )?;
        if held {
            left += 1;
            continue;
        }
        if let Some(old) = &track.replaced {
            replaced.push((old.clone(), track_id.clone()));
        }
        if chosen.is_none() {
            continue;
        }
        upsert_track_identity(
            tx,
            &TrackRow {
                local_track_id: track_id,
                recording_mbid: &track.recording_mbid,
                release_mbid: Some(&release),
                release_track_mbid: Some(&track.release_track_mbid),
                medium_position: Some(track.disc),
                release_track_position: Some(track.position),
                decision_source: "manual",
            },
            now,
        )?;
        placed += 1;
    }
    if placed > 0 {
        identity::clear_chosen_flag(tx, album_id)?;
    }
    if chosen.is_none() && replaced.is_empty() {
        return Ok((0, left));
    }
    continue_replaced(tx, album_id, &replaced, now)?;
    tracing::info!(
        album = album_id,
        placed,
        left,
        "imported files were placed on the album's edition"
    );
    Ok((placed, left))
}
