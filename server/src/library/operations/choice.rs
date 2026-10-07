//! Choosing an album's edition: the one operation behind every way a
//! person picks, confirms, undoes, or hands back an album's edition.
//!
//! The album identity row (`local_album_external_identities`) is the
//! album's edition. A person's choice writes it as `manual`, which no
//! automatic pass replaces. Any release can be chosen, from the album's own
//! release group or another one, and the files need not fit it one to one:
//! files the edition has no track for keep their recording without a
//! release placement, and edition tracks with no file are simply missing.
//!
//! Every change keeps what it replaced in `library_edition_choice_undo`, so
//! the last choice can be taken back, and writes an audit row to
//! `library_catalog_actions`. Each function runs inside the caller's
//! transaction.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};

use super::decisions::{
    CatalogAction, TrackRow, bump_catalog, indexed_track_ids, record_action, restore_track_row,
    seal_album, settle_reviews, upsert_track_identity,
};
use super::models::{EditionChoice, OperationError, PlacedFile, ReidentificationCandidate};
use super::reasons;
use super::store::PROVIDER;
use crate::library::identify::models::{EvidenceClass, PriorAlbumIdentity, PriorTrackIdentity};
use crate::library::identify::sqlite::{album_identity_row, track_identity_rows};

/// An album's match state row, as the undo snapshot keeps it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct MatchStateRow {
    state: String,
    reason_code: String,
    release_mbid: Option<String>,
    candidates_json: String,
    updated_at: f64,
}

fn match_state_row(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<MatchStateRow>> {
    conn.query_row(
        "SELECT state, reason_code, release_mbid, candidates_json, updated_at \
         FROM library_album_match_state WHERE local_album_id = ?1",
        params![album_id],
        |row| {
            Ok(MatchStateRow {
                state: row.get(0)?,
                reason_code: row.get(1)?,
                release_mbid: row.get(2)?,
                candidates_json: row.get(3)?,
                updated_at: row.get(4)?,
            })
        },
    )
    .optional()
}

fn to_json<T: Serialize>(value: &T) -> Result<String, OperationError> {
    serde_json::to_string(value).map_err(|error| OperationError::Store(error.to_string()))
}

/// What a change replaced, kept until the next change or an undo.
struct Snapshot {
    identity: Option<PriorAlbumIdentity>,
    tracks: Vec<PriorTrackIdentity>,
    state: Option<MatchStateRow>,
}

fn snapshot(conn: &Connection, album_id: &str) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        identity: album_identity_row(conn, album_id)?,
        tracks: track_identity_rows(conn, album_id)?,
        state: match_state_row(conn, album_id)?,
    })
}

/// Keep `before` as the album's one undo, keyed to the identity revision
/// the change left behind.
fn keep_undo(
    tx: &Transaction<'_>,
    album_id: &str,
    before: &Snapshot,
    action_id: &str,
    now: f64,
) -> Result<(), OperationError> {
    let revision: i64 = tx
        .query_row(
            "SELECT row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let identity = before.identity.as_ref().map(to_json).transpose()?;
    let state = before.state.as_ref().map(to_json).transpose()?;
    tx.execute(
        "INSERT INTO library_edition_choice_undo (id, local_album_id, action_id, \
         prior_identity_json, prior_tracks_json, prior_match_state_json, \
         expected_identity_revision, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (local_album_id) DO UPDATE SET id = excluded.id, \
         action_id = excluded.action_id, prior_identity_json = excluded.prior_identity_json, \
         prior_tracks_json = excluded.prior_tracks_json, \
         prior_match_state_json = excluded.prior_match_state_json, \
         expected_identity_revision = excluded.expected_identity_revision, \
         created_at = excluded.created_at",
        params![
            uuid::Uuid::new_v4().to_string(),
            album_id,
            action_id,
            identity,
            to_json(&before.tracks)?,
            state,
            revision,
            now,
        ],
    )?;
    Ok(())
}

/// One file placed on a track of the chosen edition.
struct Placement {
    local_track_id: String,
    recording_mbid: String,
    release_track_mbid: String,
    disc: u32,
    position: u32,
}

/// Place each indexed file on its own track of the candidate's release
/// where the evidence supports it. Returns the placements and the files
/// left over. Two files never share one release track.
fn place(
    indexed: &[String],
    candidate: &ReidentificationCandidate,
) -> (Vec<Placement>, Vec<String>) {
    let mut placed: Vec<Placement> = Vec::new();
    let mut extra = Vec::new();
    for track_id in indexed {
        let evidence = candidate
            .evidence
            .track_evidence
            .iter()
            .find(|evidence| evidence.local_track_id == *track_id)
            .filter(|evidence| evidence.classification == EvidenceClass::Supported);
        let spot = candidate
            .tracks
            .iter()
            .find(|track| track.local_track_id == *track_id);
        let placement = evidence.zip(spot).and_then(|(evidence, spot)| {
            Some(Placement {
                local_track_id: track_id.clone(),
                recording_mbid: evidence.recording_mbid.clone()?,
                release_track_mbid: evidence.release_track_mbid.clone()?,
                disc: spot.disc_number.filter(|disc| *disc > 0)?,
                position: spot.position.filter(|position| *position > 0)?,
            })
        });
        match placement {
            Some(placement)
                if !placed
                    .iter()
                    .any(|seen| seen.release_track_mbid == placement.release_track_mbid) =>
            {
                placed.push(placement)
            }
            _ => extra.push(track_id.clone()),
        }
    }
    (placed, extra)
}

/// Seal the candidate's release as the album's chosen edition and keep an
/// undo. Used by every edition choice, including a re-identification's
/// exact release. `actor` is the person choosing (`None` for a choice
/// carried from an older pin). `action_id` names the audit row or job the
/// undo belongs to.
pub fn apply_choice(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    actor: Option<&str>,
    action_id: &str,
    now: f64,
) -> Result<EditionChoice, OperationError> {
    let release = candidate
        .evidence
        .release_mbid
        .clone()
        .ok_or(OperationError::NotFound(reasons::EDITION_NOT_FOUND))?;
    let indexed = indexed_track_ids(tx, album_id)?;
    if indexed.is_empty() {
        return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
    }
    let (placed, extra) = place(&indexed, candidate);
    if placed.is_empty() {
        return Err(OperationError::Invalid(reasons::EDITION_FITS_NO_FILE));
    }
    let before = snapshot(tx, album_id)?;
    let group = candidate.evidence.release_group_mbid.clone();
    seal_album(tx, album_id, &group, Some(&release), actor, now)?;
    for placement in &placed {
        upsert_track_identity(
            tx,
            &TrackRow {
                local_track_id: &placement.local_track_id,
                recording_mbid: &placement.recording_mbid,
                release_mbid: Some(&release),
                release_track_mbid: Some(&placement.release_track_mbid),
                medium_position: Some(placement.disc),
                release_track_position: Some(placement.position),
                decision_source: "manual",
            },
            now,
        )?;
    }
    // Files the edition does not hold keep their recording, unplaced.
    for track_id in &extra {
        tx.execute(
            "UPDATE local_track_external_identities SET release_mbid = NULL, \
             release_track_mbid = NULL, medium_position = NULL, \
             release_track_position = NULL, decision_source = 'manual', selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 AND provider = ?3",
            params![track_id, now, PROVIDER],
        )?;
    }
    for table in [
        "library_custom_edition_active",
        "library_management_exclusions",
        "library_album_match_state",
        "library_edition_remap_queue",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE local_album_id = ?1"),
            params![album_id],
        )?;
    }
    keep_undo(tx, album_id, &before, action_id, now)?;
    Ok(EditionChoice {
        local_album_id: album_id.to_owned(),
        release_group_mbid: group,
        release_mbid: release,
        placed: placed_files(tx, &placed)?,
        extra_track_ids: extra,
        missing_titles: candidate.unmatched_expected_tracks.clone(),
    })
}

fn placed_files(conn: &Connection, placed: &[Placement]) -> rusqlite::Result<Vec<PlacedFile>> {
    let mut out = Vec::with_capacity(placed.len());
    for placement in placed {
        let file = conn
            .query_row(
                "SELECT root_id, relative_path FROM local_tracks WHERE id = ?1",
                params![placement.local_track_id],
                |row| {
                    Ok(PlacedFile {
                        local_track_id: placement.local_track_id.clone(),
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

/// A person chooses the album's edition: seal it, settle the album's open
/// reviews, and record the decision.
pub fn choose(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    actor: Option<&str>,
    now: f64,
) -> Result<EditionChoice, OperationError> {
    let before = album_identity_row(tx, album_id)?;
    let action = uuid::Uuid::new_v4().to_string();
    let choice = apply_choice(tx, album_id, candidate, actor, &action, now)?;
    if let Some(actor) = actor {
        settle_reviews(
            tx,
            album_id,
            "approved",
            Some(&candidate.candidate_key),
            actor,
        )?;
    }
    audit(
        tx,
        actor,
        "edition_choice",
        album_id,
        serde_json::to_value(&before).unwrap_or_default(),
        serde_json::json!({
            "release_group_mbid": choice.release_group_mbid,
            "release_mbid": choice.release_mbid,
            "placed": choice.placed.len(),
            "extra": choice.extra_track_ids.len(),
            "missing": choice.missing_titles.len(),
            "undo_action_id": action,
        }),
        "EDITION_CHOSEN",
        now,
    )?;
    bump_catalog(tx)?;
    Ok(choice)
}

#[allow(clippy::too_many_arguments)]
fn audit(
    tx: &Transaction<'_>,
    actor: Option<&str>,
    kind: &str,
    album_id: &str,
    before: serde_json::Value,
    after: serde_json::Value,
    reason: &str,
    now: f64,
) -> rusqlite::Result<String> {
    record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind,
            album_id,
            job_id: None,
            before,
            after,
            reason,
        },
        now,
    )
}

/// "Let DroppedNeedle choose": the album's edition goes back to automatic
/// best fit. The identity row and its track rows become automatic again,
/// so the next identification may replace them; the caller queues it.
/// Returns false when the album has no identity at all (nothing to hand
/// back; identification alone decides).
pub fn hand_back(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<bool, OperationError> {
    let before = snapshot(tx, album_id)?;
    let Some(identity) = before.identity.clone() else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE local_album_external_identities SET decision_source = 'automatic', \
         selected_by_user_id = NULL, selected_at = ?2, row_revision = row_revision + 1 \
         WHERE local_album_id = ?1 AND provider = ?3",
        params![album_id, now, PROVIDER],
    )?;
    tx.execute(
        "UPDATE local_track_external_identities SET decision_source = 'automatic', \
         selected_at = ?2, row_revision = row_revision + 1 \
         WHERE provider = ?3 AND decision_source <> 'automatic' AND local_track_id IN \
         (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
        params![album_id, now, PROVIDER],
    )?;
    tx.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![album_id],
    )?;
    let action = audit(
        tx,
        Some(actor),
        "edition_choice_cleared",
        album_id,
        serde_json::to_value(&identity).unwrap_or_default(),
        serde_json::json!({ "decision_source": "automatic" }),
        "EDITION_HANDED_BACK",
        now,
    )?;
    keep_undo(tx, album_id, &before, &action, now)?;
    bump_catalog(tx)?;
    Ok(true)
}

/// "Looks right": a person confirms the album's unconfirmed best guess.
/// The guess becomes their choice, so it is protected like one, and file
/// management treats it as confirmed.
pub fn confirm(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let before = snapshot(tx, album_id)?;
    let identity = before
        .identity
        .clone()
        .filter(|identity| identity.release_mbid.is_some())
        .ok_or(OperationError::Conflict(reasons::NO_MATCH_TO_CONFIRM))?;
    if before
        .state
        .as_ref()
        .is_none_or(|state| state.state != "unconfirmed")
    {
        return Err(OperationError::Conflict(reasons::NOTHING_TO_CONFIRM));
    }
    tx.execute(
        "UPDATE local_album_external_identities SET decision_source = 'manual', \
         selected_by_user_id = (SELECT id FROM auth_users WHERE id = ?2), selected_at = ?3, \
         row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?4",
        params![album_id, actor, now, PROVIDER],
    )?;
    tx.execute(
        "UPDATE local_track_external_identities SET decision_source = 'manual', \
         selected_at = ?2, row_revision = row_revision + 1 \
         WHERE provider = ?3 AND local_track_id IN \
         (SELECT id FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed')",
        params![album_id, now, PROVIDER],
    )?;
    tx.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1",
        params![album_id],
    )?;
    settle_reviews(tx, album_id, "approved", None, actor)?;
    let action = audit(
        tx,
        Some(actor),
        "edition_confirmed",
        album_id,
        serde_json::to_value(&identity).unwrap_or_default(),
        serde_json::json!({ "decision_source": "manual" }),
        "MATCH_CONFIRMED",
        now,
    )?;
    keep_undo(tx, album_id, &before, &action, now)?;
    bump_catalog(tx)?;
    Ok(())
}

/// An undo row: id, prior identity, prior tracks, prior match state, and
/// the identity revision the change left.
type UndoRow = (String, Option<String>, String, Option<String>, i64);

/// Whether the album has an edition change that can still be undone.
pub fn undo_available(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_edition_choice_undo u \
         JOIN local_album_external_identities i \
         ON i.local_album_id = u.local_album_id AND i.provider = ?2 \
         WHERE u.local_album_id = ?1 AND i.row_revision = u.expected_identity_revision)",
        params![album_id, PROVIDER],
        |row| row.get(0),
    )
}

/// Take back the album's last edition change: the identity, track rows and
/// match state go back to what they were. Refused once anything else has
/// changed the album's identity since.
pub fn undo(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let undo: Option<UndoRow> = tx
        .query_row(
            "SELECT id, prior_identity_json, prior_tracks_json, prior_match_state_json, \
             expected_identity_revision FROM library_edition_choice_undo \
             WHERE local_album_id = ?1",
            params![album_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((undo_id, identity_json, tracks_json, state_json, expected)) = undo else {
        return Err(OperationError::NotFound(reasons::NO_EDITION_CHOICE));
    };
    let current = album_identity_row(tx, album_id)?;
    if current
        .as_ref()
        .and_then(|row| row.row_revision)
        .unwrap_or(0)
        != expected
    {
        return Err(OperationError::Conflict(reasons::EDITION_CHOICE_STALE));
    }
    let unreadable = |error: serde_json::Error| OperationError::Store(error.to_string());
    let prior: Option<PriorAlbumIdentity> = identity_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(unreadable)?;
    let prior_tracks: Vec<PriorTrackIdentity> =
        serde_json::from_str(&tracks_json).map_err(unreadable)?;
    let prior_state: Option<MatchStateRow> = state_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(unreadable)?;

    match prior.filter(|prior| prior.release_group_mbid.is_some()) {
        Some(prior) => {
            tx.execute(
                "INSERT INTO local_album_external_identities (local_album_id, provider, \
                 release_group_mbid, release_mbid, decision_source, matcher_version, \
                 attempt_id, selected_by_user_id, selected_at, row_revision) \
                 VALUES (?1, ?9, ?2, ?3, ?4, ?5, \
                 (SELECT id FROM library_identification_attempts WHERE id = ?6), \
                 (SELECT id FROM auth_users WHERE id = ?7), ?8, 1) \
                 ON CONFLICT (local_album_id, provider) DO UPDATE SET \
                 release_group_mbid = excluded.release_group_mbid, \
                 release_mbid = excluded.release_mbid, \
                 decision_source = excluded.decision_source, \
                 matcher_version = excluded.matcher_version, attempt_id = excluded.attempt_id, \
                 selected_by_user_id = excluded.selected_by_user_id, \
                 selected_at = excluded.selected_at, row_revision = row_revision + 1",
                params![
                    album_id,
                    prior.release_group_mbid,
                    prior.release_mbid,
                    prior.decision_source.as_deref().unwrap_or("automatic"),
                    prior.matcher_version,
                    prior.attempt_id,
                    prior.selected_by_user_id,
                    prior.selected_at.unwrap_or(now),
                    PROVIDER,
                ],
            )?;
        }
        None => {
            tx.execute(
                "DELETE FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2",
                params![album_id, PROVIDER],
            )?;
        }
    }
    for track_id in indexed_track_ids(tx, album_id)? {
        match prior_tracks
            .iter()
            .find(|row| row.local_track_id == track_id && row.recording_mbid.is_some())
        {
            Some(row) => restore_track_row(tx, row, now)?,
            None => {
                tx.execute(
                    "DELETE FROM local_track_external_identities \
                     WHERE local_track_id = ?1 AND provider = ?2",
                    params![track_id, PROVIDER],
                )?;
            }
        }
    }
    tx.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1",
        params![album_id],
    )?;
    if let Some(state) = prior_state {
        tx.execute(
            "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
             release_mbid, candidates_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                album_id,
                state.state,
                state.reason_code,
                state.release_mbid,
                state.candidates_json,
                state.updated_at,
            ],
        )?;
    }
    tx.execute(
        "DELETE FROM library_edition_choice_undo WHERE id = ?1",
        params![undo_id],
    )?;
    audit(
        tx,
        Some(actor),
        "edition_choice_undone",
        album_id,
        serde_json::to_value(&current).unwrap_or_default(),
        serde_json::json!({ "undo_id": undo_id }),
        "EDITION_CHOICE_UNDONE",
        now,
    )?;
    bump_catalog(tx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Choices waiting for their files to be placed.
// ---------------------------------------------------------------------------

/// One album whose chosen edition still needs its files placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRemap {
    pub local_album_id: String,
    pub release_mbid: String,
    pub chosen_by_user_id: Option<String>,
    pub attempts: i64,
}

/// Tries before a remap that keeps failing is given up (the choice stays).
pub const REMAP_ATTEMPTS: i64 = 5;
/// Seconds between tries while MusicBrainz is down.
pub const REMAP_RETRY_SECS: f64 = 300.0;

/// The next album due for a remap, if any.
pub fn next_remap(conn: &Connection, now: f64) -> rusqlite::Result<Option<PendingRemap>> {
    conn.query_row(
        "SELECT local_album_id, release_mbid, chosen_by_user_id, attempts \
         FROM library_edition_remap_queue WHERE not_before <= ?1 \
         ORDER BY queued_at, local_album_id LIMIT 1",
        params![now],
        |row| {
            Ok(PendingRemap {
                local_album_id: row.get(0)?,
                release_mbid: row.get(1)?,
                chosen_by_user_id: row.get(2)?,
                attempts: row.get(3)?,
            })
        },
    )
    .optional()
}

/// Try a remap again later, or give it up after [`REMAP_ATTEMPTS`]. The
/// album keeps its chosen edition either way; only the file placement
/// waits.
pub fn defer_remap(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    retry: bool,
    now: f64,
) -> rusqlite::Result<()> {
    if !retry || pending.attempts + 1 >= REMAP_ATTEMPTS {
        tracing::warn!(
            album = pending.local_album_id,
            release = pending.release_mbid,
            code,
            "gave up placing an album's files on its chosen edition; the choice stays"
        );
        tx.execute(
            "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
            params![pending.local_album_id],
        )?;
        return Ok(());
    }
    tx.execute(
        "UPDATE library_edition_remap_queue SET attempts = attempts + 1, \
         not_before = ?2, last_code = ?3 WHERE local_album_id = ?1",
        params![pending.local_album_id, now + REMAP_RETRY_SECS, code],
    )?;
    Ok(())
}

/// Queue the album's files to be placed on its chosen release.
pub fn queue_remap(
    tx: &Transaction<'_>,
    album_id: &str,
    release_mbid: &str,
    chosen_by: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
         chosen_by_user_id, queued_at) VALUES (?1, lower(?2), ?3, ?4) \
         ON CONFLICT (local_album_id) DO UPDATE SET release_mbid = excluded.release_mbid, \
         chosen_by_user_id = excluded.chosen_by_user_id, queued_at = excluded.queued_at, \
         attempts = 0, not_before = 0, last_code = NULL",
        params![album_id, release_mbid, chosen_by, now],
    )?;
    Ok(())
}
