//! The commands a person's word on an album runs: choosing, confirming,
//! handing back and leaving unmanaged an edition, taking the match off,
//! approving a review, and matching one file by hand.
//!
//! Each runs inside the caller's transaction, keeps what it replaced as the
//! album's one choice undo, writes an audit row, and moves the catalog
//! revision once when it changed a row. A track row is written only for a
//! file the album holds and has indexed.

use rusqlite::{Connection, Transaction, params};

use super::reads::{
    album_identity_row, hand_matches_standing, identity_revision, indexed_track_ids,
    stored_release, track_row,
};
use super::remap::queue_remap;
use super::sql;
use super::undo::{Snapshot, keep_choice_undo, record_and_keep, restore_track_row, snapshot};
use super::{DecisionSource, IdentityError, PROVIDER, Placed, Refusal, Rows, delete_album_row};
use crate::catalog_revision::bump_catalog;
use crate::corrections::continuation::{Scope, continue_tracks};
use crate::corrections::release_join::join_replaced;
use crate::identify::models::PriorTrackIdentity;
use crate::operations::decisions::{CatalogAction, record_action, settle_reviews};

/// Reason code on an album whose chosen edition fits none of its files.
pub(crate) const CHOSEN_FITS_NO_FILE: &str = "CHOSEN_EDITION_FITS_NO_FILE";

/// An edition offered for the album's files: its release, and the release
/// track each supported file could take.
#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub release_group_mbid: String,
    pub release_mbid: String,
    /// One spot per file the evidence supports, with its disc and position.
    pub spots: Vec<Placed>,
    /// Every file the offer weighed, supported or not.
    pub weighed: Vec<String>,
}

/// Place each of `indexed` on its spot of the offer. Returns the
/// placements and the files left over. Two files never share one release
/// track. The edition plan uses the same placement, so its preview is what
/// a choice would write.
pub fn place(indexed: &[String], spots: &[Placed]) -> (Vec<Placed>, Vec<String>) {
    let mut placed: Vec<Placed> = Vec::new();
    let mut extra = Vec::new();
    for track_id in indexed {
        match spots.iter().find(|spot| spot.local_track_id == *track_id) {
            Some(spot)
                if !placed
                    .iter()
                    .any(|seen| seen.release_track_mbid == spot.release_track_mbid) =>
            {
                placed.push(spot.clone())
            }
            _ => extra.push(track_id.clone()),
        }
    }
    (placed, extra)
}

/// How an offer falls on the album's files.
pub(super) struct Fit {
    pub(super) placed: Vec<Placed>,
    pub(super) extra: Vec<String>,
    /// Files a match by hand keeps on their track.
    pub(super) by_hand: Vec<String>,
}

/// Whether `release` is the album's current edition: the same id, or a
/// release whose stored document answers to it (a merged id).
fn answers_to_current(conn: &Connection, album_id: &str, release: &str) -> rusqlite::Result<bool> {
    let Some(current) = album_identity_row(conn, album_id)?
        .and_then(|row| row.release_mbid)
        .filter(|current| !current.trim().is_empty())
    else {
        return Ok(false);
    };
    if current.trim().eq_ignore_ascii_case(release.trim()) {
        return Ok(true);
    }
    Ok(stored_release(conn, release, 0.0)?.is_some_and(|doc| doc.answers_to(current.trim())))
}

/// Fit the offer to the album's files. Choosing the edition the album
/// already has keeps the files a person matched by hand on their tracks,
/// and a placement on a track such a match holds makes that file extra.
pub(super) fn fit(
    tx: &Transaction<'_>,
    album_id: &str,
    offer: &Offer,
) -> Result<Fit, IdentityError> {
    let indexed = indexed_track_ids(tx, album_id)?;
    if indexed.is_empty() {
        return Err(Refusal::AlbumNotFound.into());
    }
    let by_hand = if answers_to_current(tx, album_id, &offer.release_mbid)? {
        hand_matches_standing(tx, album_id)?
    } else {
        Vec::new()
    };
    let free: Vec<String> = indexed
        .into_iter()
        .filter(|file| !by_hand.iter().any(|(kept, _)| kept == file))
        .collect();
    let (offered, mut extra) = place(&free, &offer.spots);
    let mut placed = Vec::with_capacity(offered.len());
    for placement in offered {
        if by_hand
            .iter()
            .any(|(_, track)| track.eq_ignore_ascii_case(&placement.release_track_mbid))
        {
            extra.push(placement.local_track_id);
        } else {
            placed.push(placement);
        }
    }
    if placed.is_empty() && by_hand.is_empty() {
        return Err(Refusal::EditionFitsNoFile.into());
    }
    Ok(Fit {
        placed,
        extra,
        by_hand: by_hand.into_iter().map(|(file, _)| file).collect(),
    })
}

/// Write a chosen edition's track rows: placed files as the person's on
/// their release tracks, the rest keeping their recording, unplaced.
pub(super) fn write_choice_tracks(
    tx: &Transaction<'_>,
    album_id: &str,
    release: &str,
    fit: &Fit,
    now: f64,
) -> rusqlite::Result<()> {
    for placement in &fit.placed {
        tx.prepare_cached(sql::PLACE_CHOSEN_TRACK.text())?
            .execute(params![
                placement.local_track_id,
                PROVIDER,
                placement.recording_mbid,
                release,
                placement.release_track_mbid,
                placement.disc,
                placement.position,
                now,
                album_id,
            ])?;
    }
    for track_id in &fit.extra {
        tx.prepare_cached(sql::UNPLACE_CHOSEN_TRACK.text())?
            .execute(params![track_id, now, PROVIDER, album_id])?;
    }
    Ok(())
}

/// Write the album's identity as a person's decision. Row revisions only
/// move forward.
pub(super) fn seal_persons_album(
    tx: &Transaction<'_>,
    album_id: &str,
    release_group: &str,
    release: Option<&str>,
    actor: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::SEAL_PERSONS_ALBUM.text())?
        .execute(params![
            album_id,
            PROVIDER,
            release_group,
            release,
            actor,
            now
        ])?;
    Ok(())
}

pub(super) fn drop_satellite(
    tx: &Transaction<'_>,
    album_id: &str,
    statement: droppedneedle_kernel::db::statement::Statement,
) -> rusqlite::Result<()> {
    tx.prepare_cached(statement.text())?
        .execute(params![album_id])?;
    Ok(())
}

/// What a person's choice of an edition did.
#[derive(Debug, Clone)]
pub struct Chosen {
    pub release_group_mbid: String,
    pub release_mbid: String,
    /// Files on a track of the edition: those placed now, then those a
    /// match by hand keeps.
    pub placed: Vec<String>,
    /// Files the edition has no track for.
    pub extra: Vec<String>,
}

/// A person chooses the album's edition: the album row becomes theirs on
/// the release, its files are placed where the offer supports them and the
/// rest keep their recording, unplaced. The match state, a queued placement
/// and an active custom edition go; a person's "leave unmanaged" stays
/// ("Manage it again" is the one way back to management). With a person
/// (`actor`), the album's open reviews settle as approved. Automatic
/// management looks at the album again.
pub fn choose(
    tx: &Transaction<'_>,
    album_id: &str,
    offer: &Offer,
    candidate_key: &str,
    actor: Option<&str>,
    missing: usize,
    now: f64,
) -> Result<Chosen, IdentityError> {
    let fit = fit(tx, album_id, offer)?;
    let before = snapshot(tx, album_id)?;
    let release = offer.release_mbid.as_str();
    seal_persons_album(
        tx,
        album_id,
        &offer.release_group_mbid,
        Some(release),
        actor,
        now,
    )?;
    write_choice_tracks(tx, album_id, release, &fit, now)?;
    for statement in [sql::DROP_CUSTOM, sql::CLEAR_FLAG, sql::DROP_REMAP] {
        drop_satellite(tx, album_id, statement)?;
    }
    let undo_action = uuid::Uuid::new_v4().to_string();
    keep_choice_undo(tx, album_id, &before, &undo_action, false, now)?;
    crate::management::automatic::queue(tx, album_id, now)?;
    join_replaced(tx, Scope::Album(album_id), now)?;
    continue_tracks(tx, Scope::Album(album_id), now)?;
    if let Some(actor) = actor {
        settle_reviews(tx, album_id, "approved", Some(candidate_key), actor, now)?;
    }
    record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind: "edition_choice",
            album_id,
            job_id: None,
            before: serde_json::to_value(&before.identity).unwrap_or_default(),
            after: serde_json::json!({
                "release_group_mbid": offer.release_group_mbid,
                "release_mbid": release,
                "placed": fit.placed.len() + fit.by_hand.len(),
                "extra": fit.extra.len(),
                "missing": missing,
                "undo_action_id": undo_action,
            }),
            reason: "EDITION_CHOSEN",
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(Chosen {
        release_group_mbid: offer.release_group_mbid.clone(),
        release_mbid: release.to_owned(),
        placed: fit
            .placed
            .iter()
            .map(|placement| placement.local_track_id.clone())
            .chain(fit.by_hand.iter().cloned())
            .collect(),
        extra: fit.extra,
    })
}

/// "Looks right": a person confirms the album's unconfirmed best guess.
/// The guess becomes their choice, so it is protected like one, and file
/// management treats it as confirmed.
pub fn confirm(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), IdentityError> {
    let before = snapshot(tx, album_id)?;
    if before
        .identity
        .as_ref()
        .is_none_or(|identity| identity.release_mbid.is_none())
    {
        return Err(Refusal::NoMatchToConfirm.into());
    }
    if before.flag_state() != Some("unconfirmed") {
        return Err(Refusal::NothingToConfirm.into());
    }
    make_persons(tx, album_id, &before, actor, now)?;
    bump_catalog(tx)?;
    Ok(())
}

/// The writes of a confirmation: the album row and its indexed track rows
/// become the person's (a v2 choice already is one and keeps its source),
/// the match state goes, open reviews are settled, automatic management
/// looks again, and `before` is kept as the undo.
fn make_persons(
    tx: &Transaction<'_>,
    album_id: &str,
    before: &Snapshot,
    actor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::CONFIRM_ALBUM.text())?
        .execute(params![album_id, actor, now, PROVIDER])?;
    tx.prepare_cached(sql::CONFIRM_TRACKS.text())?
        .execute(params![album_id, now, PROVIDER])?;
    drop_satellite(tx, album_id, sql::CLEAR_FLAG)?;
    // A chosen or confirmed edition is what automatic management waits
    // for: the album's files follow that exact edition.
    crate::management::automatic::queue(tx, album_id, now)?;
    settle_reviews(tx, album_id, "approved", None, actor, now)?;
    let action = record_action(
        tx,
        CatalogAction {
            actor,
            kind: "edition_confirmed",
            album_id,
            job_id: None,
            before: serde_json::to_value(before.identity.clone().unwrap_or_default())
                .unwrap_or_default(),
            after: serde_json::json!({ "decision_source": "manual" }),
            reason: "MATCH_CONFIRMED",
        },
        now,
    )?;
    keep_choice_undo(tx, album_id, before, &action, false, now)
}

/// "Let DroppedNeedle choose": the album's edition goes back to automatic
/// best fit. The album row and the person's track rows become automatic
/// (rows the tags carried keep their source), a queued placement and an
/// active custom edition go; the match state, "leave unmanaged" and files
/// matched by hand stay. The undo is carried through the identification
/// the caller queues in the same transaction, so it still brings the
/// person's edition back after DroppedNeedle has picked again. Returns
/// false when the album has no identity at all.
pub fn hand_back(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let before = snapshot(tx, album_id)?;
    let Some(identity) = before.identity.clone() else {
        return Ok(false);
    };
    tx.prepare_cached(sql::HAND_BACK_ALBUM.text())?
        .execute(params![album_id, now, PROVIDER])?;
    tx.prepare_cached(sql::HAND_BACK_TRACKS.text())?
        .execute(params![album_id, now, PROVIDER])?;
    for statement in [sql::DROP_REMAP, sql::DROP_CUSTOM] {
        drop_satellite(tx, album_id, statement)?;
    }
    let action = record_action(
        tx,
        CatalogAction {
            actor,
            kind: "edition_choice_cleared",
            album_id,
            job_id: None,
            before: serde_json::to_value(&identity).unwrap_or_default(),
            after: serde_json::json!({ "decision_source": "automatic" }),
            reason: "EDITION_HANDED_BACK",
        },
        now,
    )?;
    keep_choice_undo(tx, album_id, &before, &action, true, now)?;
    bump_catalog(tx)?;
    Ok(true)
}

/// A person leaves the album unmanaged: the exact release goes, the release
/// group stays as their choice, the files leave their release tracks, and
/// Library Management keeps out until "Manage it again". A queued placement
/// and an active custom edition go, and so does the match state while the
/// album has a match. Refused as stale, with nothing changed, unless the
/// identity revision is still `expected_identity_revision` (0 for an album
/// with none). The album's open reviews settle as rejected.
pub fn leave_unmanaged(
    tx: &Transaction<'_>,
    album_id: &str,
    expected_identity_revision: i64,
    actor: &str,
    now: f64,
) -> Result<(), IdentityError> {
    if identity_revision(tx, album_id)? != expected_identity_revision {
        return Err(Refusal::EditionChanged.into());
    }
    let before = snapshot(tx, album_id)?;
    let matched = before.identity.is_some();
    if matched {
        tx.prepare_cached(sql::LEAVE_ALBUM_UNMANAGED.text())?
            .execute(params![album_id, actor, now, PROVIDER])?;
    }
    tx.prepare_cached(sql::UNPLACE_ALBUM_TRACKS.text())?
        .execute(params![album_id, now, PROVIDER])?;
    tx.prepare_cached(sql::EXCLUDE_ALBUM.text())?
        .execute(params![album_id, actor, now])?;
    for statement in [sql::DROP_CUSTOM, sql::DROP_REMAP] {
        drop_satellite(tx, album_id, statement)?;
    }
    // An album left with no identity keeps its `unmatched` row.
    if matched {
        drop_satellite(tx, album_id, sql::CLEAR_FLAG)?;
    }
    settle_reviews(tx, album_id, "rejected", None, actor, now)?;
    record_and_keep(
        tx,
        album_id,
        &before,
        actor,
        "leave_unmanaged",
        "MANAGEMENT_EXCLUDED",
        false,
        now,
    )?;
    bump_catalog(tx)?;
    Ok(())
}

/// Who takes an album's edition off, and so what stays to take it back.
#[derive(Debug, Clone, Copy)]
pub enum Removal<'a> {
    /// A repair found the automatic match failed its recheck. A person's
    /// choice, made now or carried over from v2, is never removed. The
    /// undo is carried through the identification the caller queues.
    Repair {
        reason: &'a str,
        actor: Option<&'a str>,
        job_id: Option<&'a str>,
    },
    /// A person takes the match off from a review, whoever made it.
    Person { reason: &'a str, actor: &'a str },
    /// Two albums became one: the edition of the one that goes is dropped
    /// with no undo. The review the caller leaves is the way back.
    Merge,
}

/// Take the album's edition off, with everything that only means something
/// with it: the album row, a queued placement and the active custom
/// edition, always; custom edition manifests, files matched by hand and
/// "leave unmanaged" stay. A repair or a person also takes the files'
/// matches off and marks the album unmatched with the reason, and the
/// change can be undone like an edition choice (the automatic-edition undo
/// is not brought back). A merge drops the match state and both undos.
/// Returns the audit row id; `None` when there was nothing to remove (or a
/// repair met a person's choice), and always for a merge.
pub fn remove(
    tx: &Transaction<'_>,
    album_id: &str,
    removal: Removal<'_>,
    now: f64,
) -> rusqlite::Result<Option<String>> {
    if let Removal::Merge = removal {
        delete_album_row(tx, album_id, Rows::Any)?;
        for statement in [sql::CLEAR_FLAG, sql::DROP_REMAP, sql::DROP_CUSTOM] {
            drop_satellite(tx, album_id, statement)?;
        }
        return Ok(None);
    }
    let before = snapshot(tx, album_id)?;
    let Some(identity) = before.identity.clone() else {
        return Ok(None);
    };
    let persons = identity
        .decision_source
        .as_deref()
        .and_then(DecisionSource::parse)
        .is_some_and(DecisionSource::is_persons_choice);
    let (reason, actor, job_id, kind, carried) = match removal {
        Removal::Repair { .. } if persons => return Ok(None),
        Removal::Repair {
            reason,
            actor,
            job_id,
        } => (reason, actor, job_id, "repair_detach", true),
        Removal::Person { reason, actor } => (reason, Some(actor), None, "review_detach", false),
        Removal::Merge => return Ok(None),
    };
    tx.prepare_cached(sql::DELETE_ALBUM_TRACK_ROWS.text())?
        .execute(params![album_id, PROVIDER])?;
    delete_album_row(tx, album_id, Rows::Any)?;
    for statement in [sql::DROP_REMAP, sql::DROP_CUSTOM] {
        drop_satellite(tx, album_id, statement)?;
    }
    tx.prepare_cached(sql::MARK_UNMATCHED.text())?
        .execute(params![album_id, reason, now])?;
    let action = record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind,
            album_id,
            job_id,
            before: serde_json::to_value(&identity).unwrap_or_default(),
            after: serde_json::json!({ "match_state": "unmatched" }),
            reason,
        },
        now,
    )?;
    keep_choice_undo(tx, album_id, &before, &action, carried, now)?;
    bump_catalog(tx)?;
    Ok(Some(action))
}

/// A curator's approval of a review candidate.
#[derive(Debug, Clone, Copy)]
pub struct Approved<'a> {
    pub release_group_mbid: &'a str,
    /// `None` for a candidate naming only the release group.
    pub release_mbid: Option<&'a str>,
    pub by_user_id: &'a str,
    pub now: f64,
}

/// A curator approves a review candidate: the album row becomes theirs on
/// the candidate's group and release. The match state, an active custom
/// edition and a queued placement go; "leave unmanaged" stays and the
/// track rows are left as they are. With a release, the files are queued
/// to be placed on it as the curator's choice, which then queues automatic
/// management; approval queues nothing else. The caller settles the review.
pub fn approve(
    tx: &Transaction<'_>,
    album_id: &str,
    approved: &Approved<'_>,
) -> rusqlite::Result<()> {
    let before = snapshot(tx, album_id)?;
    seal_persons_album(
        tx,
        album_id,
        approved.release_group_mbid,
        approved.release_mbid,
        Some(approved.by_user_id),
        approved.now,
    )?;
    for statement in [sql::CLEAR_FLAG, sql::DROP_CUSTOM, sql::DROP_REMAP] {
        drop_satellite(tx, album_id, statement)?;
    }
    if let Some(release) = approved.release_mbid {
        queue_remap(
            tx,
            album_id,
            release,
            Some(approved.by_user_id),
            approved.now,
        )?;
    }
    record_and_keep(
        tx,
        album_id,
        &before,
        approved.by_user_id,
        "review_approve",
        "REVIEW_APPROVED",
        false,
        approved.now,
    )?;
    // The album now holds a person's match: an album its files replaced
    // joins it now, as after any choice.
    join_replaced(tx, Scope::Album(album_id), approved.now)?;
    continue_tracks(tx, Scope::Album(album_id), approved.now)?;
    bump_catalog(tx)?;
    Ok(())
}

/// One file a person matched by hand to a track of the album's edition.
#[derive(Debug, Clone, Copy)]
pub struct Matched<'a> {
    pub album_id: &'a str,
    pub track_id: &'a str,
    /// The album's release, as its identity row spells it.
    pub release_mbid: &'a str,
    pub release_track_mbid: &'a str,
    pub recording_mbid: &'a str,
    pub disc: u32,
    pub position: u32,
    pub actor: &'a str,
    /// Seconds.
    pub now: f64,
    /// The same moment in milliseconds, as the match record keeps it.
    pub now_ms: i64,
}

/// Match one file to a track of the album's edition: the file's row
/// becomes the person's on that track, the record its undo needs is kept,
/// and a "fits no file" flag goes. On an edition the app picked, the
/// edition becomes the person's first, as "Looks right" does, with the
/// edition's undo kept; a v2 choice already is one.
pub fn hand_match(tx: &Transaction<'_>, matched: &Matched<'_>) -> rusqlite::Result<()> {
    let album_id = matched.album_id;
    let now = matched.now;
    let before = snapshot(tx, album_id)?;
    let persons = before
        .identity
        .as_ref()
        .and_then(|row| row.decision_source.as_deref())
        .and_then(DecisionSource::parse)
        .is_some_and(DecisionSource::is_persons_choice);
    if !persons {
        make_persons(tx, album_id, &before, matched.actor, now)?;
    }
    let prior = track_row(tx, matched.track_id)?;
    tx.prepare_cached(sql::KEEP_HAND_MATCH.text())?
        .execute(params![
            matched.track_id,
            matched.release_mbid,
            matched.release_track_mbid,
            prior.as_ref().map(super::undo::to_json).transpose()?,
            matched.actor,
            matched.now_ms,
        ])?;
    tx.prepare_cached(sql::PLACE_CHOSEN_TRACK.text())?
        .execute(params![
            matched.track_id,
            PROVIDER,
            matched.recording_mbid,
            matched.release_mbid,
            matched.release_track_mbid,
            matched.disc,
            matched.position,
            now,
            album_id,
        ])?;
    // The file now fits the edition the person chose.
    tx.prepare_cached(sql::CLEAR_FLAG_WITH_REASON.text())?
        .execute(params![album_id, CHOSEN_FITS_NO_FILE])?;
    record_action(
        tx,
        CatalogAction {
            actor: matched.actor,
            kind: "track_matched",
            album_id,
            job_id: None,
            before: serde_json::to_value(&prior).unwrap_or_default(),
            after: serde_json::to_value(track_row(tx, matched.track_id)?).unwrap_or_default(),
            reason: "TRACK_MATCHED",
        },
        now,
    )?;
    crate::management::automatic::queue(tx, album_id, now)?;
    join_replaced(tx, Scope::Album(album_id), now)?;
    continue_tracks(tx, Scope::Album(album_id), now)?;
    bump_catalog(tx)?;
    Ok(())
}

/// "Undo match": the file's row goes back to `prior`, what it was before
/// the match (or goes, when it had none), and the match is forgotten.
/// `current` is the row the match left, for the audit. The caller has
/// checked the match still stands and the app has not written the file
/// since. The album's edition stays the person's.
pub fn unmatch(
    tx: &Transaction<'_>,
    album_id: &str,
    track_id: &str,
    prior: Option<&PriorTrackIdentity>,
    current: Option<&PriorTrackIdentity>,
    actor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    match prior.filter(|row| row.recording_mbid.is_some()) {
        Some(row) => restore_track_row(tx, row, now)?,
        None => {
            tx.prepare_cached(sql::DELETE_TRACK_ROW.text())?
                .execute(params![track_id, PROVIDER])?;
        }
    }
    tx.prepare_cached(sql::FORGET_HAND_MATCH.text())?
        .execute(params![track_id])?;
    record_action(
        tx,
        CatalogAction {
            actor,
            kind: "track_match_undone",
            album_id,
            job_id: None,
            before: serde_json::to_value(current).unwrap_or_default(),
            after: serde_json::to_value(prior).unwrap_or_default(),
            reason: "TRACK_MATCH_UNDONE",
        },
        now,
    )?;
    crate::management::automatic::queue(tx, album_id, now)?;
    bump_catalog(tx)?;
    Ok(())
}

/// One track identity row to write.
pub(crate) struct TrackRow<'a> {
    pub local_track_id: &'a str,
    pub recording_mbid: &'a str,
    pub release_mbid: Option<&'a str>,
    pub release_track_mbid: Option<&'a str>,
    pub medium_position: Option<u32>,
    pub release_track_position: Option<u32>,
    pub decision_source: &'a str,
}

/// Write one track identity; an existing row takes the next revision.
pub(crate) fn upsert_track_identity(
    tx: &Transaction<'_>,
    row: &TrackRow<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::UPSERT_TRACK.text())?
        .execute(params![
            row.local_track_id,
            PROVIDER,
            row.recording_mbid,
            row.release_mbid,
            row.release_track_mbid,
            row.medium_position,
            row.release_track_position,
            row.decision_source,
            now,
        ])?;
    Ok(())
}
