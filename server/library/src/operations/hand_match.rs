//! A person matches one file to a track of the album's edition by hand.
//!
//! On the album's current edition, a curator puts a file the edition did
//! not place on an edition track no file holds. The match is the file's
//! track identity, written as the person's (`manual`) on the album's
//! release, so it counts as placed everywhere: the edition plan, automatic
//! management and the carry of a missing track's history. What the undo
//! needs is kept in `library_track_hand_matches`, one row per file.
//!
//! A match stands while the file's identity is `manual` and names the
//! row's release and release track. Choosing another edition, "Let
//! DroppedNeedle choose" and "Leave unmanaged" change that identity, so
//! the row then stands for nothing and is no longer offered for undo.
//!
//! A match on an edition the app picked makes the edition the person's, as
//! "Looks right" does, with the edition's undo kept; one carried from v2
//! already is a person's. A match never writes a file, never marks the
//! album taken over and never stamps or hands it back. The checks run
//! here, the writes in [`crate::identity::hand_match`].

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::models::{OperationError, ReidentificationCandidate};
use super::plan::{StoredPlacement, seat};
use super::reasons;
use super::store::PROVIDER;
use crate::identify::models::PriorTrackIdentity;
use crate::identity::reads::{album_identity_row, identity_revision, indexed_track_ids, track_row};
use crate::identity::{self, Matched};
use crate::matching::model::Release;

/// Whether the album is in the library and not merged into another.
fn album_live(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
         AND retired_into_album_id IS NULL)",
        params![album_id],
        |row| row.get(0),
    )
}

/// Whether a custom edition is active on the album.
fn custom_active(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)",
        params![album_id],
        |row| row.get(0),
    )
}

/// Whether `track_id` is an indexed file of the album.
fn indexed_on(conn: &Connection, album_id: &str, track_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE id = ?1 AND local_album_id = ?2 \
         AND availability = 'indexed')",
        params![track_id, album_id],
        |row| row.get(0),
    )
}

/// The album's release, as its identity row spells it: `None` when it has
/// no identity or no release (a release group only, "Leave unmanaged", a
/// custom edition).
fn album_release(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    Ok(album_identity_row(conn, album_id)?
        .and_then(|row| row.release_mbid)
        .filter(|release| !release.trim().is_empty()))
}

/// What a match is checked against before MusicBrainz is asked: the album
/// is there, is not a custom edition, has not changed since the person read
/// it and has a release. Returns that release.
pub(super) fn guard(
    conn: &Connection,
    album_id: &str,
    expected_identity_revision: i64,
) -> Result<String, OperationError> {
    if !album_live(conn, album_id)? {
        return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
    }
    if custom_active(conn, album_id)? {
        return Err(OperationError::Conflict(
            reasons::CUSTOM_EDITION_KEEPS_FILES,
        ));
    }
    if identity_revision(conn, album_id)? != expected_identity_revision {
        return Err(OperationError::Conflict(reasons::EDITION_CHANGED));
    }
    album_release(conn, album_id)?.ok_or(OperationError::Invalid(reasons::NO_EDITION_TO_PLAN))
}

/// The album's indexed files whose identity names a release track, each
/// with when a person matched it there by hand, if that match stands. The
/// row a match by hand writes is `manual`, so only such a row stands for
/// one.
pub(super) fn stored_placements(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Vec<StoredPlacement>> {
    conn.prepare(
        "SELECT t.id, i.release_mbid, i.release_track_mbid, \
         CASE WHEN i.decision_source = 'manual' \
         AND lower(h.release_mbid) = lower(i.release_mbid) \
         AND lower(h.release_track_mbid) = lower(i.release_track_mbid) \
         THEN h.matched_at END \
         FROM local_tracks t JOIN local_track_external_identities i \
         ON i.local_track_id = t.id AND i.provider = ?2 \
         LEFT JOIN library_track_hand_matches h ON h.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
         AND trim(COALESCE(i.release_track_mbid, '')) <> '' ORDER BY t.id",
    )?
    .query_map(params![album_id, PROVIDER], |row| {
        let matched_at: Option<i64> = row.get(3)?;
        Ok(StoredPlacement {
            local_track_id: row.get(0)?,
            release_mbid: row.get(1)?,
            release_track_mbid: row.get(2)?,
            by_hand: matched_at.is_some(),
            matched_at,
        })
    })?
    .collect()
}

/// Whether the app has written the file since a match made at `matched_at`
/// (milliseconds): a run that writes files (a profile run, a tag edit, a
/// duplicate resolution), planned after the match with something to write
/// to this file, not failed or discarded, and whose album that run's undo
/// has not put back. Its plan exists before the file is written, so a
/// write still under way counts too. A scan would then put the file back
/// on the track, so "Undo match" waits until that write is undone. The
/// undo and the plan both ask this.
pub(super) fn written_since(
    conn: &Connection,
    track_id: &str,
    matched_at: i64,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_management_plan_items p \
         JOIN library_management_job_snapshots s ON s.job_id = p.job_id \
         JOIN library_operation_jobs j ON j.id = p.job_id \
         WHERE p.local_track_id = ?1 \
         AND s.mode IN ('apply', 'automatic_apply', 'duplicate_resolution') \
         AND j.state NOT IN ('failed', 'cancelled', 'stopped') \
         AND p.eligibility IN ('eligible', 'warning') \
         AND json_extract(p.desired_document_json, '$.plan') IS NOT NULL \
         AND p.created_at * 1000 >= ?2 \
         AND NOT EXISTS (SELECT 1 FROM library_management_job_snapshots u \
         JOIN library_operation_work w ON w.job_id = u.job_id \
         WHERE u.linked_operation_job_id = p.job_id AND u.mode = 'undo' \
         AND w.ordinal = p.bundle_ordinal AND w.state = 'succeeded'))",
        params![track_id, matched_at],
        |row| row.get(0),
    )
}

/// The stored placements that name `release`: its own id, a merged id of
/// it, or no release at all.
pub(super) fn on_release(stored: Vec<StoredPlacement>, release: &Release) -> Vec<StoredPlacement> {
    stored
        .into_iter()
        .filter(|row| {
            row.release_mbid
                .as_deref()
                .map(str::trim)
                .is_none_or(|named| named.is_empty() || release.answers_to(named))
        })
        .collect()
}

/// One match a person asked for.
pub(super) struct HandMatch<'a> {
    pub album_id: &'a str,
    pub track_id: &'a str,
    pub release_track_mbid: &'a str,
    pub expected_identity_revision: i64,
    pub actor: &'a str,
}

/// Match one file to a track of the album's edition, in the caller's
/// transaction. `release` and `candidate` are the album's release as
/// MusicBrainz lists it, scored against the files as the plan scores it.
/// Everything is checked again here and refused with nothing written.
/// `now` is in seconds, `now_ms` in milliseconds.
pub(super) fn match_file(
    tx: &Transaction<'_>,
    asked: &HandMatch<'_>,
    release: &Release,
    candidate: &ReidentificationCandidate,
    now: f64,
    now_ms: i64,
) -> Result<(), OperationError> {
    let album_id = asked.album_id;
    if !album_live(tx, album_id)? {
        return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
    }
    if custom_active(tx, album_id)? {
        return Err(OperationError::Conflict(
            reasons::CUSTOM_EDITION_KEEPS_FILES,
        ));
    }
    let album_row = album_identity_row(tx, album_id)?;
    let Some(album_release) = album_row
        .as_ref()
        .and_then(|row| row.release_mbid.clone())
        .filter(|release| !release.trim().is_empty())
    else {
        return Err(OperationError::Invalid(reasons::NO_EDITION_TO_PLAN));
    };
    if identity_revision(tx, album_id)? != asked.expected_identity_revision
        || !release.answers_to(album_release.trim())
    {
        return Err(OperationError::Conflict(reasons::EDITION_CHANGED));
    }
    if !indexed_on(tx, album_id, asked.track_id)? {
        return Err(OperationError::NotFound(reasons::FILE_NOT_ON_ALBUM));
    }
    let wanted = asked.release_track_mbid.trim();
    let Some(track_index) = release
        .tracks
        .iter()
        .position(|track| track.id.eq_ignore_ascii_case(wanted))
    else {
        return Err(OperationError::Invalid(reasons::TRACK_NOT_ON_EDITION));
    };
    let indexed = indexed_track_ids(tx, album_id)?;
    let stored = on_release(stored_placements(tx, album_id)?, release);
    let seats = seat(release, candidate, &indexed, &stored);
    if seats
        .iter()
        .any(|seat| seat.local_track_id == asked.track_id)
    {
        return Err(OperationError::Conflict(reasons::FILE_ALREADY_PLACED));
    }
    if seats.iter().any(|seat| seat.track == track_index) {
        return Err(OperationError::Conflict(reasons::EDITION_TRACK_TAKEN));
    }
    let track = &release.tracks[track_index];
    identity::hand_match(
        tx,
        &Matched {
            album_id,
            track_id: asked.track_id,
            release_mbid: &album_release,
            release_track_mbid: &track.id,
            recording_mbid: &track.recording_id,
            disc: track.disc,
            position: track.position,
            actor: asked.actor,
            now,
            now_ms,
        },
    )?;
    Ok(())
}

/// A person's match of one file, as kept for its undo.
struct Kept {
    release_mbid: String,
    release_track_mbid: String,
    prior_identity_json: Option<String>,
    matched_at: i64,
}

/// "Undo match": the file's identity goes back to what it was before the
/// match, and the match is forgotten. Refused while the match no longer
/// stands, or once the app has written the file since the match (a scan
/// would put it back on the track). What a carry moved stays where it
/// went, and the album's edition stays the person's.
pub(super) fn unmatch_file(
    tx: &Transaction<'_>,
    album_id: &str,
    track_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    if !album_live(tx, album_id)? {
        return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
    }
    let kept = tx
        .query_row(
            "SELECT release_mbid, release_track_mbid, prior_identity_json, matched_at \
             FROM library_track_hand_matches WHERE local_track_id = ?1",
            params![track_id],
            |row| {
                Ok(Kept {
                    release_mbid: row.get(0)?,
                    release_track_mbid: row.get(1)?,
                    prior_identity_json: row.get(2)?,
                    matched_at: row.get(3)?,
                })
            },
        )
        .optional()?
        .ok_or(OperationError::NotFound(reasons::NO_HAND_MATCH))?;
    if !indexed_on(tx, album_id, track_id)? {
        return Err(OperationError::NotFound(reasons::FILE_NOT_ON_ALBUM));
    }
    let current = track_row(tx, track_id)?;
    let stands = current.as_ref().is_some_and(|row| {
        row.decision_source.as_deref() == Some("manual")
            && row
                .release_mbid
                .as_deref()
                .is_some_and(|release| release.eq_ignore_ascii_case(&kept.release_mbid))
            && row
                .release_track_mbid
                .as_deref()
                .is_some_and(|track| track.eq_ignore_ascii_case(&kept.release_track_mbid))
    });
    if !stands {
        return Err(OperationError::Conflict(reasons::HAND_MATCH_STALE));
    }
    if written_since(tx, track_id, kept.matched_at)? {
        return Err(OperationError::Conflict(reasons::HAND_MATCH_WRITTEN));
    }
    let prior: Option<PriorTrackIdentity> = kept
        .prior_identity_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| OperationError::Store(error.to_string()))?;
    identity::unmatch(
        tx,
        album_id,
        track_id,
        prior.as_ref(),
        current.as_ref(),
        actor,
        now,
    )?;
    Ok(())
}
