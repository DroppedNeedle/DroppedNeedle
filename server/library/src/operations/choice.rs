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
use crate::corrections::continuation::{Scope, continue_replaced, continue_tracks};
use crate::corrections::release_join::join_replaced;
use crate::identify::models::{EvidenceClass, PriorAlbumIdentity, PriorTrackIdentity};
use crate::identify::sqlite::{album_identity_row, track_identity_rows};

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

/// An album's "leave unmanaged" row, as the undo keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExclusionRow {
    reason: String,
    excluded_by_user_id: String,
    excluded_at: f64,
    row_revision: i64,
}

/// The same row. The time is compared to the microsecond: read back from
/// the undo's JSON it can differ from the stored value in the last bit.
impl PartialEq for ExclusionRow {
    fn eq(&self, other: &Self) -> bool {
        self.reason == other.reason
            && self.excluded_by_user_id == other.excluded_by_user_id
            && self.row_revision == other.row_revision
            && (self.excluded_at - other.excluded_at).abs() < 1e-6
    }
}

/// What holds the album apart beside its edition: a person's "leave
/// unmanaged" and the active custom edition.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Hold {
    exclusion: Option<ExclusionRow>,
    custom_manifest_id: Option<String>,
}

/// The album's hold before a change and as the change left it.
#[derive(Debug, Serialize, Deserialize)]
struct HoldChange {
    before: Hold,
    after: Hold,
}

fn hold(conn: &Connection, album_id: &str) -> rusqlite::Result<Hold> {
    let exclusion = conn
        .query_row(
            "SELECT reason, excluded_by_user_id, excluded_at, row_revision \
             FROM library_management_exclusions WHERE local_album_id = ?1",
            params![album_id],
            |row| {
                Ok(ExclusionRow {
                    reason: row.get(0)?,
                    excluded_by_user_id: row.get(1)?,
                    excluded_at: row.get(2)?,
                    row_revision: row.get(3)?,
                })
            },
        )
        .optional()?;
    let custom_manifest_id = conn
        .query_row(
            "SELECT manifest_id FROM library_custom_edition_active WHERE local_album_id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Hold {
        exclusion,
        custom_manifest_id,
    })
}

/// Put back each part of the hold `change` altered, while that part is
/// still as the change left it.
fn restore_hold(
    tx: &Transaction<'_>,
    album_id: &str,
    change: &HoldChange,
    actor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let current = hold(tx, album_id)?;
    let (before, after) = (&change.before, &change.after);
    if before.exclusion != after.exclusion && current.exclusion == after.exclusion {
        match &before.exclusion {
            None => {
                tx.execute(
                    "DELETE FROM library_management_exclusions WHERE local_album_id = ?1",
                    params![album_id],
                )?;
            }
            Some(row) => {
                // The person who left it unmanaged may have been removed
                // since; the one undoing stands in for them.
                tx.execute(
                    "INSERT INTO library_management_exclusions (local_album_id, reason, \
                     excluded_by_user_id, excluded_at, row_revision) \
                     SELECT ?1, ?2, chooser, ?4, 1 FROM (SELECT COALESCE( \
                     (SELECT id FROM auth_users WHERE id = ?3), \
                     (SELECT id FROM auth_users WHERE id = ?5)) AS chooser) \
                     WHERE chooser IS NOT NULL \
                     ON CONFLICT (local_album_id) DO UPDATE SET reason = excluded.reason, \
                     excluded_by_user_id = excluded.excluded_by_user_id, \
                     excluded_at = excluded.excluded_at, row_revision = row_revision + 1",
                    params![
                        album_id,
                        row.reason,
                        row.excluded_by_user_id,
                        row.excluded_at,
                        actor
                    ],
                )?;
            }
        }
    }
    if before.custom_manifest_id != after.custom_manifest_id
        && current.custom_manifest_id == after.custom_manifest_id
    {
        match &before.custom_manifest_id {
            None => {
                tx.execute(
                    "DELETE FROM library_custom_edition_active WHERE local_album_id = ?1",
                    params![album_id],
                )?;
            }
            Some(manifest) => {
                // A manifest that is gone since is not put back.
                tx.execute(
                    "INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
                     activated_at, row_revision) SELECT ?1, id, ?3, 1 \
                     FROM library_custom_edition_manifests WHERE id = ?2 AND local_album_id = ?1 \
                     ON CONFLICT (local_album_id) DO UPDATE SET manifest_id = excluded.manifest_id, \
                     activated_at = excluded.activated_at, row_revision = row_revision + 1",
                    params![album_id, manifest, now],
                )?;
            }
        }
    }
    Ok(())
}

/// What a change replaced, kept until the next change or an undo.
struct Snapshot {
    identity: Option<PriorAlbumIdentity>,
    tracks: Vec<PriorTrackIdentity>,
    state: Option<MatchStateRow>,
    hold: Hold,
}

fn snapshot(conn: &Connection, album_id: &str) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        identity: album_identity_row(conn, album_id)?,
        tracks: track_identity_rows(conn, album_id)?,
        state: match_state_row(conn, album_id)?,
        hold: hold(conn, album_id)?,
    })
}

/// The album identity's row revision, 0 when the album has none.
pub(super) fn identity_revision(conn: &Connection, album_id: &str) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row(
            "SELECT row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Keep `before` as the album's one undo, keyed to the identity revision
/// the change left behind, with the album's hold before and after it.
fn keep_undo(
    tx: &Transaction<'_>,
    album_id: &str,
    before: &Snapshot,
    action_id: &str,
    now: f64,
) -> Result<(), OperationError> {
    let revision = identity_revision(tx, album_id)?;
    let identity = before.identity.as_ref().map(to_json).transpose()?;
    let state = before.state.as_ref().map(to_json).transpose()?;
    let held = to_json(&HoldChange {
        before: before.hold.clone(),
        after: hold(tx, album_id)?,
    })?;
    tx.execute(
        "INSERT INTO library_edition_choice_undo (id, local_album_id, action_id, \
         prior_identity_json, prior_tracks_json, prior_match_state_json, \
         expected_identity_revision, created_at, prior_hold_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT (local_album_id) DO UPDATE SET id = excluded.id, \
         action_id = excluded.action_id, prior_identity_json = excluded.prior_identity_json, \
         prior_tracks_json = excluded.prior_tracks_json, \
         prior_match_state_json = excluded.prior_match_state_json, \
         expected_identity_revision = excluded.expected_identity_revision, \
         created_at = excluded.created_at, prior_hold_json = excluded.prior_hold_json",
        params![
            uuid::Uuid::new_v4().to_string(),
            album_id,
            action_id,
            identity,
            to_json(&before.tracks)?,
            state,
            revision,
            now,
            held,
        ],
    )?;
    Ok(())
}

/// What a change is about to replace, taken before it writes.
pub(crate) struct UndoPoint(Snapshot);

/// Take the album's current match as the undo point for a change.
pub(crate) fn undo_point(conn: &Connection, album_id: &str) -> rusqlite::Result<UndoPoint> {
    Ok(UndoPoint(snapshot(conn, album_id)?))
}

/// After a person's change: write its audit row and keep `point` as the
/// album's one undo, as every edition choice does.
pub(crate) fn keep_undo_point(
    tx: &Transaction<'_>,
    album_id: &str,
    point: &UndoPoint,
    actor: &str,
    kind: &str,
    reason: &str,
    now: f64,
) -> Result<String, OperationError> {
    let after = album_identity_row(tx, album_id)?;
    let action = record_action(
        tx,
        CatalogAction {
            actor,
            kind,
            album_id,
            job_id: None,
            before: serde_json::to_value(&point.0.identity).unwrap_or_default(),
            after: serde_json::to_value(&after).unwrap_or_default(),
            reason,
        },
        now,
    )?;
    keep_undo(tx, album_id, &point.0, &action, now)?;
    bump_catalog(tx)?;
    Ok(action)
}

/// One file placed on a track of the chosen edition.
pub(super) struct Placement {
    pub(super) local_track_id: String,
    pub(super) recording_mbid: String,
    pub(super) release_track_mbid: String,
    pub(super) disc: u32,
    pub(super) position: u32,
}

/// Place each indexed file on its own track of the candidate's release
/// where the evidence supports it. Returns the placements and the files
/// left over. Two files never share one release track. The edition plan
/// uses the same placement, so its preview is what a choice would write.
pub(super) fn place(
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
/// carried from an older pin). `undo_for` names the audit row or job the
/// undo belongs to; `None` (placing files for a choice already made) keeps
/// the album's existing undo record as it is.
pub fn apply_choice(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    actor: Option<&str>,
    undo_for: Option<&str>,
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
    // A person's "leave unmanaged" stays: "Manage it again" is the one way
    // back to management.
    for table in [
        "library_custom_edition_active",
        "library_album_match_state",
        "library_edition_remap_queue",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE local_album_id = ?1"),
            params![album_id],
        )?;
    }
    if let Some(action_id) = undo_for {
        keep_undo(tx, album_id, &before, action_id, now)?;
    }
    // A new edition means new tags and paths: automatic management looks
    // at the album again with the edition just chosen.
    crate::management::automatic::queue(tx, album_id, now)?;
    join_replaced(tx, Scope::Album(album_id), now)?;
    continue_tracks(tx, Scope::Album(album_id), now)?;
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
    let choice = apply_choice(tx, album_id, candidate, actor, Some(&action), now)?;
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
    // A chosen or confirmed edition is what automatic management waits
    // for: the album's files follow that exact edition.
    crate::management::automatic::queue(tx, album_id, now)?;
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

/// Accept an edition for an album without a person: the release becomes
/// the album's automatic identity with its files placed on it. The gate
/// that leads here is stricter than identification's own sealing, so the
/// match is applied as confirmed. Both undos are kept: the album page's
/// edition undo and the automatic-edition undo, and either one puts the
/// earlier match state back. A person's choice is never replaced: `None`
/// when the album holds one, or the edition fits no file.
pub fn accept_automatic(
    tx: &Transaction<'_>,
    album_id: &str,
    candidate: &ReidentificationCandidate,
    reason_code: &str,
    job_id: Option<&str>,
    now: f64,
) -> Result<Option<String>, OperationError> {
    let Some(release) = candidate.evidence.release_mbid.clone() else {
        return Ok(None);
    };
    let before = snapshot(tx, album_id)?;
    if before
        .identity
        .as_ref()
        .is_some_and(|identity| identity.decision_source.as_deref() == Some("manual"))
    {
        return Ok(None);
    }
    let indexed = indexed_track_ids(tx, album_id)?;
    let (placed, extra) = place(&indexed, candidate);
    if placed.is_empty() {
        return Ok(None);
    }
    let group = candidate.evidence.release_group_mbid.clone();
    tx.execute(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         VALUES (?1, ?2, ?3, ?4, 'automatic', ?5, 1) \
         ON CONFLICT (local_album_id, provider) DO UPDATE SET \
         release_group_mbid = excluded.release_group_mbid, \
         release_mbid = excluded.release_mbid, decision_source = 'automatic', \
         selected_by_user_id = NULL, selected_at = excluded.selected_at, \
         row_revision = row_revision + 1",
        params![album_id, PROVIDER, group, release, now],
    )?;
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
                decision_source: "automatic",
            },
            now,
        )?;
    }
    for track_id in &extra {
        tx.execute(
            "UPDATE local_track_external_identities SET release_mbid = NULL, \
             release_track_mbid = NULL, medium_position = NULL, \
             release_track_position = NULL, selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 AND provider = ?3",
            params![track_id, now, PROVIDER],
        )?;
    }
    tx.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![album_id],
    )?;
    // A confident match: applied fully, so no unconfirmed flag remains.
    tx.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1",
        params![album_id],
    )?;
    // The automatic-edition undo, as an automatic seal keeps it.
    tx.execute(
        "INSERT INTO library_automatic_edition_undo (id, local_album_id, job_id, \
         prior_identity_json, prior_track_identities_json, expected_post_album_revision, \
         expected_post_identity_revision, reason_code, created_at) \
         SELECT ?1, a.id, ?2, ?3, ?4, a.row_revision, i.row_revision, ?5, ?6 \
         FROM local_albums a JOIN local_album_external_identities i \
         ON i.local_album_id = a.id AND i.provider = ?8 WHERE a.id = ?7 \
         ON CONFLICT (local_album_id) DO UPDATE SET id = excluded.id, \
         job_id = excluded.job_id, evidence_id = NULL, \
         prior_identity_json = excluded.prior_identity_json, \
         prior_track_identities_json = excluded.prior_track_identities_json, \
         expected_post_album_revision = excluded.expected_post_album_revision, \
         expected_post_identity_revision = excluded.expected_post_identity_revision, \
         reason_code = excluded.reason_code, created_at = excluded.created_at, \
         consumed_at = NULL, consumed_action_id = NULL",
        params![
            uuid::Uuid::new_v4().to_string(),
            job_id,
            before.identity.as_ref().map(to_json).transpose()?,
            to_json(&before.tracks)?,
            reason_code,
            now,
            album_id,
            PROVIDER,
        ],
    )?;
    let action = record_action(
        tx,
        CatalogAction {
            actor: "system",
            kind: "automatic_exact_edition",
            album_id,
            job_id,
            before: serde_json::to_value(&before.identity).unwrap_or_default(),
            after: serde_json::json!({
                "release_group_mbid": group,
                "release_mbid": release,
                "placed": placed.len(),
                "extra": extra.len(),
            }),
            reason: reason_code,
        },
        now,
    )?;
    keep_undo(tx, album_id, &before, &action, now)?;
    bump_catalog(tx)?;
    Ok(Some(action))
}

/// Take an automatic match off an album that failed its recheck: the
/// album identity and its files' placements go, the album shows as
/// unmatched with `reason_code`, and the change can be undone like an
/// edition choice. A person's choice is never detached: `None` when the
/// album has no identity or a chosen one. Returns the audit row id.
pub fn detach(
    tx: &Transaction<'_>,
    album_id: &str,
    reason_code: &str,
    actor: Option<&str>,
    job_id: Option<&str>,
    now: f64,
) -> Result<Option<String>, OperationError> {
    detach_identity(tx, album_id, reason_code, actor, job_id, now, false)
}

/// A person takes the match off an album from its review, whoever made
/// it: the same change as [`detach`], undoable the same way. `None` when
/// the album has no identity.
pub fn detach_by_person(
    tx: &Transaction<'_>,
    album_id: &str,
    reason_code: &str,
    actor: &str,
    now: f64,
) -> Result<Option<String>, OperationError> {
    detach_identity(tx, album_id, reason_code, Some(actor), None, now, true)
}

fn detach_identity(
    tx: &Transaction<'_>,
    album_id: &str,
    reason_code: &str,
    actor: Option<&str>,
    job_id: Option<&str>,
    now: f64,
    include_manual: bool,
) -> Result<Option<String>, OperationError> {
    let before = snapshot(tx, album_id)?;
    let Some(identity) = before.identity.clone() else {
        return Ok(None);
    };
    if !include_manual && identity.decision_source.as_deref() == Some("manual") {
        return Ok(None);
    }
    tx.execute(
        "DELETE FROM local_track_external_identities WHERE provider = ?2 AND local_track_id IN \
         (SELECT id FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed')",
        params![album_id, PROVIDER],
    )?;
    tx.execute(
        "DELETE FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
    )?;
    tx.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![album_id],
    )?;
    tx.execute(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         release_mbid, candidates_json, updated_at) VALUES (?1, 'unmatched', ?2, NULL, '[]', ?3) \
         ON CONFLICT (local_album_id) DO UPDATE SET state = 'unmatched', \
         reason_code = excluded.reason_code, release_mbid = NULL, candidates_json = '[]', \
         updated_at = excluded.updated_at",
        params![album_id, reason_code, now],
    )?;
    let action = record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind: if include_manual {
                "review_detach"
            } else {
                "repair_detach"
            },
            album_id,
            job_id,
            before: serde_json::to_value(&identity).unwrap_or_default(),
            after: serde_json::json!({ "match_state": "unmatched" }),
            reason: reason_code,
        },
        now,
    )?;
    keep_undo(tx, album_id, &before, &action, now)?;
    bump_catalog(tx)?;
    Ok(Some(action))
}

/// Put back the match state an automatic acceptance replaced, when the
/// acceptance that left identity revision `post_identity_revision` kept
/// one, and use up that edition undo (the change it covers is gone).
pub(crate) fn restore_match_state(
    tx: &Transaction<'_>,
    album_id: &str,
    post_identity_revision: i64,
) -> Result<(), OperationError> {
    let kept: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT id, prior_match_state_json FROM library_edition_choice_undo \
             WHERE local_album_id = ?1 AND expected_identity_revision = ?2",
            params![album_id, post_identity_revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((undo_id, state_json)) = kept else {
        return Ok(());
    };
    let prior: Option<MatchStateRow> = state_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| OperationError::Store(error.to_string()))?;
    tx.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1",
        params![album_id],
    )?;
    if let Some(state) = prior {
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
    Ok(())
}

/// An undo row: id, prior identity, prior tracks, prior match state, the
/// identity revision the change left, and the hold before and after it.
type UndoRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    i64,
    Option<String>,
);

/// Whether the album has an edition change that can still be undone.
pub fn undo_available(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_edition_choice_undo u \
         LEFT JOIN local_album_external_identities i \
         ON i.local_album_id = u.local_album_id AND i.provider = ?2 \
         WHERE u.local_album_id = ?1 \
         AND COALESCE(i.row_revision, 0) = u.expected_identity_revision)",
        params![album_id, PROVIDER],
        |row| row.get(0),
    )
}

/// Take back the album's last edition change: the identity, track rows and
/// match state go back to what they were, and so does each part of the
/// album's hold ("leave unmanaged", the active custom edition) the change
/// altered, while that part is still as the change left it. A record kept
/// before the hold was recorded restores none. Refused once anything else
/// has changed the album's identity since.
pub fn undo(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let undo: Option<UndoRow> = tx
        .query_row(
            "SELECT id, prior_identity_json, prior_tracks_json, prior_match_state_json, \
             expected_identity_revision, prior_hold_json FROM library_edition_choice_undo \
             WHERE local_album_id = ?1",
            params![album_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((undo_id, identity_json, tracks_json, state_json, expected, hold_json)) = undo else {
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
    let held: Option<HoldChange> = hold_json
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
    if let Some(change) = &held {
        restore_hold(tx, album_id, change, actor, now)?;
    }
    tx.execute(
        "DELETE FROM library_edition_choice_undo WHERE id = ?1",
        params![undo_id],
    )?;
    crate::management::automatic::queue(tx, album_id, now)?;
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
// Editions waiting for their files to be placed.
//
// A queue row names the release the album's files go on. `album_pin` and
// `group_pin` rows seal the release as the album's choice when they run,
// as a choice made then would. A `place_only` row is for an album that
// already has its edition, one a person chose or v2 carried, and holds
// files that are not on it yet (a merge, a move, a retag the scan
// followed): it puts the files on the release and changes nothing else.
// Whatever its origin, a row runs only while the album's match is still a
// person's and names that release; otherwise it is dropped unrun.
// ---------------------------------------------------------------------------

/// Why an album waits in the remap queue (the `origin` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapOrigin {
    /// A person chose this album's edition.
    AlbumPin,
    /// The choice came from a release-group pin fanned out over every
    /// copy, not from a person pinning this album.
    GroupPin,
    /// The album has its edition; only its files are placed on it.
    PlaceOnly,
}

impl RemapOrigin {
    pub const ALL: [Self; 3] = [Self::AlbumPin, Self::GroupPin, Self::PlaceOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlbumPin => "album_pin",
            Self::GroupPin => "group_pin",
            Self::PlaceOnly => "place_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|origin| origin.as_str() == value)
    }
}

/// A stored origin this version does not know is an error, never a
/// default.
impl rusqlite::types::FromSql for RemapOrigin {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::parse(text).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(format!("unknown remap origin {text:?}").into())
        })
    }
}

/// One album whose edition still needs its files placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRemap {
    pub local_album_id: String,
    pub release_mbid: String,
    pub chosen_by_user_id: Option<String>,
    pub attempts: i64,
    pub origin: RemapOrigin,
}

/// First wait, in seconds, after MusicBrainz did not answer; it doubles
/// with each try up to [`REMAP_RETRY_MAX_SECS`].
pub const REMAP_RETRY_SECS: f64 = 300.0;
/// Longest wait between tries while MusicBrainz is down (six hours).
pub const REMAP_RETRY_MAX_SECS: f64 = 21_600.0;

/// The next album due for a remap, if any.
pub fn next_remap(conn: &Connection, now: f64) -> rusqlite::Result<Option<PendingRemap>> {
    conn.query_row(
        "SELECT local_album_id, release_mbid, chosen_by_user_id, attempts, origin \
         FROM library_edition_remap_queue WHERE not_before <= ?1 \
         ORDER BY queued_at, local_album_id LIMIT 1",
        params![now],
        |row| {
            Ok(PendingRemap {
                local_album_id: row.get(0)?,
                release_mbid: row.get(1)?,
                chosen_by_user_id: row.get(2)?,
                attempts: row.get(3)?,
                origin: row.get(4)?,
            })
        },
    )
    .optional()
}

/// Whether the queued remap is still the album's latest ask. A person's
/// newer choice or hand-back removes or replaces the queue row, and the
/// worker must then leave the album alone.
pub fn remap_still_wanted(conn: &Connection, pending: &PendingRemap) -> rusqlite::Result<bool> {
    let queued: Option<(String, RemapOrigin)> = conn
        .query_row(
            "SELECT release_mbid, origin FROM library_edition_remap_queue \
             WHERE local_album_id = ?1",
            params![pending.local_album_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(queued.is_some_and(|(release, origin)| {
        release.eq_ignore_ascii_case(&pending.release_mbid) && origin == pending.origin
    }))
}

/// Whether the album's match is still a person's (chosen by hand or
/// carried from v2) and names the queued release. When it is not, a
/// person changed the album since the row was queued ("leave unmanaged",
/// a custom edition, "Let DroppedNeedle choose"), and that later word
/// stands: the row has nothing left to do.
pub fn remap_still_holds(conn: &Connection, pending: &PendingRemap) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = ?2 \
         AND decision_source IN ('manual', 'legacy_import') \
         AND lower(release_mbid) = lower(?3))",
        params![pending.local_album_id, PROVIDER, pending.release_mbid],
        |row| row.get(0),
    )
}

/// Drop a queued remap that no longer holds ([`remap_still_holds`]),
/// writing nothing else.
pub fn drop_remap(tx: &Transaction<'_>, pending: &PendingRemap) -> rusqlite::Result<()> {
    tracing::info!(
        album = pending.local_album_id,
        release = pending.release_mbid,
        origin = pending.origin.as_str(),
        "the album's edition changed since its files were queued to be placed; dropped"
    );
    tx.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![pending.local_album_id],
    )?;
    Ok(())
}

/// MusicBrainz did not answer, or the album's files are briefly out of
/// reach (a scan moving them): try again later, waiting longer each time
/// (capped). This never gives the remap up.
pub fn retry_remap(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let steps = pending.attempts.clamp(0, 16) as i32;
    let wait = (REMAP_RETRY_SECS * 2f64.powi(steps)).min(REMAP_RETRY_MAX_SECS);
    tx.execute(
        "UPDATE library_edition_remap_queue SET attempts = attempts + 1, \
         not_before = ?2, last_code = ?3 WHERE local_album_id = ?1",
        params![pending.local_album_id, now + wait, code],
    )?;
    Ok(())
}

/// The carried choice cannot be honoured: MusicBrainz does not know the
/// release, or a release-group pin's release fits none of this copy's
/// files. So
/// the album goes back to automatic best fit, its unplaced tracks with it,
/// and identification is queued. Nothing a person changed since is
/// touched: only the carried choice and its own unplaced track rows.
pub fn give_up_remap(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tracing::warn!(
        album = pending.local_album_id,
        release = pending.release_mbid,
        code,
        "the carried edition choice cannot hold this album's files; back to automatic choice"
    );
    let handed_back = tx.execute(
        "UPDATE local_album_external_identities SET decision_source = 'automatic', \
         selected_by_user_id = NULL, selected_at = ?2, row_revision = row_revision + 1 \
         WHERE local_album_id = ?1 AND provider = ?3 AND decision_source = 'manual' \
         AND lower(release_mbid) = lower(?4)",
        params![pending.local_album_id, now, PROVIDER, pending.release_mbid],
    )?;
    if handed_back == 1 {
        tx.execute(
            "UPDATE local_track_external_identities SET decision_source = 'automatic', \
             selected_at = ?2, row_revision = row_revision + 1 \
             WHERE provider = ?3 AND decision_source = 'manual' AND release_mbid IS NULL \
             AND local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            params![pending.local_album_id, now, PROVIDER],
        )?;
        crate::identify::sqlite::offer_album(tx, &pending.local_album_id, (now * 1000.0) as i64)?;
    }
    tx.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![pending.local_album_id],
    )?;
    Ok(())
}

/// A person chose this exact edition but its files cannot be placed on it
/// (`code`, logged). Their choice stays and the track placements stay as
/// they were; the album shows why (`state_code`), so they can pick another
/// edition or let DroppedNeedle choose.
pub fn keep_unplaceable_choice(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    state_code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tracing::warn!(
        album = pending.local_album_id,
        release = pending.release_mbid,
        code,
        "the album's files cannot be placed on the chosen edition; the choice is kept"
    );
    tx.execute(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         release_mbid, candidates_json, updated_at) VALUES (?1, 'unmatched', ?2, ?3, '[]', ?4) \
         ON CONFLICT (local_album_id) DO UPDATE SET state = excluded.state, \
         reason_code = excluded.reason_code, release_mbid = excluded.release_mbid, \
         updated_at = excluded.updated_at",
        params![
            pending.local_album_id,
            state_code,
            pending.release_mbid,
            now
        ],
    )?;
    tx.execute(
        "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
        params![pending.local_album_id],
    )?;
    Ok(())
}

/// Reason code on an album whose chosen edition fits none of its files.
pub const CHOSEN_FITS_NO_FILE: &str = "CHOSEN_EDITION_FITS_NO_FILE";

/// Reason code on an album whose chosen edition MusicBrainz no longer
/// lists, so its files cannot be placed on it.
pub const CHOSEN_NOT_LISTED: &str = "CHOSEN_EDITION_NOT_LISTED";

/// Queue the album's files to be placed on the release a person just
/// chose for it. It replaces any row queued for the album before, which
/// becomes this person's ask.
pub fn queue_remap(
    tx: &Transaction<'_>,
    album_id: &str,
    release_mbid: &str,
    chosen_by: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
         chosen_by_user_id, queued_at, origin) VALUES (?1, lower(?2), ?3, ?4, ?5) \
         ON CONFLICT (local_album_id) DO UPDATE SET release_mbid = excluded.release_mbid, \
         chosen_by_user_id = excluded.chosen_by_user_id, queued_at = excluded.queued_at, \
         attempts = 0, not_before = 0, last_code = NULL, origin = excluded.origin",
        params![
            album_id,
            release_mbid,
            chosen_by,
            now,
            RemapOrigin::AlbumPin.as_str()
        ],
    )?;
    Ok(())
}

/// Queue the album's files to be placed on the edition it already has
/// (`place_only`), when all of these hold: its match is a person's (chosen
/// by hand or carried from v2) and names a release, no custom edition is
/// active on it, and one of its indexed files is not placed on that
/// release. A row already queued for the album, of any origin, places the
/// files too and is left as it is. Returns true when those three hold,
/// whether the row was written now or was there already.
pub fn queue_placement(tx: &Transaction<'_>, album_id: &str, now: f64) -> rusqlite::Result<bool> {
    let release: Option<String> = tx
        .query_row(
            "SELECT lower(release_mbid) FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2 \
             AND decision_source IN ('manual', 'legacy_import') AND trim(release_mbid) <> '' \
             AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = ?1)",
            params![album_id, PROVIDER],
            |row| row.get(0),
        )
        .optional()?;
    let Some(release) = release else {
        return Ok(false);
    };
    let unplaced: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?1 \
         AND t.availability = 'indexed' AND NOT EXISTS (SELECT 1 \
         FROM local_track_external_identities i WHERE i.local_track_id = t.id \
         AND i.provider = ?2 AND lower(i.release_mbid) = ?3 \
         AND i.release_track_mbid IS NOT NULL))",
        params![album_id, PROVIDER, release],
        |row| row.get(0),
    )?;
    if !unplaced {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO library_edition_remap_queue (local_album_id, release_mbid, \
         chosen_by_user_id, queued_at, origin) VALUES (?1, ?2, NULL, ?3, ?4) \
         ON CONFLICT (local_album_id) DO NOTHING",
        params![album_id, release, now, RemapOrigin::PlaceOnly.as_str()],
    )?;
    Ok(true)
}

/// Run a `place_only` row: put the album's files on the edition it
/// already has, and change nothing else. The caller has checked that the
/// album's match is still a person's and names the queued release
/// ([`remap_still_holds`]). An album with an active custom edition has
/// nothing to place: the row is dropped.
///
/// A file already placed on the release keeps its row as it is, and the
/// release track it holds is never given to another file (a second copy
/// of it, merged in, stays unplaced). The other files the candidate
/// weighed are placed as an edition choice places them ([`place`]), each
/// upserted as a `manual` placement naming the release as the album's
/// identity row spells it; the rest keep their recording, unplaced. A
/// match state left by an earlier placement that failed
/// (`CHOSEN_EDITION_*`) goes. A file that joined the album after the
/// candidate was made keeps its row, and the queue row stays so the next
/// try places it. Otherwise the queue row goes and automatic management
/// looks at the album again, keeping a scan's stamp; it still honours
/// "leave unmanaged", the taken-over mark and the stamp before it writes
/// anything.
///
/// The album identity row (source, chooser, time, release and revision),
/// both undo records, "leave unmanaged", the custom edition, the taken-over
/// mark and the stamp are left as they are, and no audit row is written.
/// No credit proof is written either, as for any person's match: the proof
/// for a person's match is the album's accepted release, which reconcile
/// can use once the files are placed on it.
///
/// `EDITION_FITS_NO_FILE` when no file is on the release, before or after,
/// and none waits to be weighed; `ALBUM_NOT_FOUND` when the album has no
/// indexed file. Returns the
/// number of files placed now and of files left unplaced.
pub fn place_files(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    candidate: &ReidentificationCandidate,
    now: f64,
) -> Result<(usize, usize), OperationError> {
    let album_id = pending.local_album_id.as_str();
    let custom: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)",
        params![album_id],
        |row| row.get(0),
    )?;
    if custom {
        // A custom edition lists the album's own tracks; nothing is placed.
        drop_remap(tx, pending)?;
        return Ok((0, 0));
    }
    let release: String = tx.query_row(
        "SELECT release_mbid FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
        |row| row.get(0),
    )?;
    let indexed = indexed_track_ids(tx, album_id)?;
    if indexed.is_empty() {
        return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
    }
    let held: Vec<(String, String)> = tx
        .prepare(
            "SELECT t.id, i.release_track_mbid FROM local_tracks t \
             JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = ?2 \
             WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
             AND lower(i.release_mbid) = lower(?3) AND i.release_track_mbid IS NOT NULL",
        )?
        .query_map(params![album_id, PROVIDER, release], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // A file that joined the album after MusicBrainz was asked was not
    // weighed: it keeps its row, and the queue row stays for the next try.
    let (others, unseen): (Vec<String>, Vec<String>) = indexed
        .into_iter()
        .filter(|track| !held.iter().any(|(placed, _)| placed == track))
        .partition(|track| {
            candidate
                .evidence
                .track_evidence
                .iter()
                .any(|evidence| evidence.local_track_id == *track)
        });
    let (offered, mut extra) = place(&others, candidate);
    let mut placed = Vec::with_capacity(offered.len());
    for placement in offered {
        if held
            .iter()
            .any(|(_, taken)| taken.eq_ignore_ascii_case(&placement.release_track_mbid))
        {
            extra.push(placement.local_track_id);
        } else {
            placed.push(placement);
        }
    }
    if placed.is_empty() && held.is_empty() && unseen.is_empty() {
        return Err(OperationError::Invalid(reasons::EDITION_FITS_NO_FILE));
    }
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
    for track_id in &extra {
        tx.execute(
            "UPDATE local_track_external_identities SET release_mbid = NULL, \
             release_track_mbid = NULL, medium_position = NULL, \
             release_track_position = NULL, decision_source = 'manual', selected_at = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 AND provider = ?3",
            params![track_id, now, PROVIDER],
        )?;
    }
    tx.execute(
        "DELETE FROM library_album_match_state WHERE local_album_id = ?1 \
         AND reason_code LIKE 'CHOSEN\\_EDITION\\_%' ESCAPE '\\'",
        params![album_id],
    )?;
    if unseen.is_empty() {
        tx.execute(
            "DELETE FROM library_edition_remap_queue WHERE local_album_id = ?1",
            params![album_id],
        )?;
        crate::management::automatic::queue(tx, album_id, now)?;
    }
    join_replaced(tx, Scope::Album(album_id), now)?;
    continue_tracks(tx, Scope::Album(album_id), now)?;
    tracing::info!(
        album = album_id,
        placed = placed.len(),
        extra = extra.len(),
        waiting = unseen.len(),
        "the album's files were placed on its edition"
    );
    Ok((placed.len(), extra.len()))
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
            "SELECT release_mbid FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2 \
             AND decision_source IN ('manual', 'legacy_import') \
             AND lower(release_mbid) = lower(?3) \
             AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = ?1)",
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
        tx.execute(
            "DELETE FROM library_album_match_state WHERE local_album_id = ?1 \
             AND reason_code LIKE 'CHOSEN\\_EDITION\\_%' ESCAPE '\\'",
            params![album_id],
        )?;
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
