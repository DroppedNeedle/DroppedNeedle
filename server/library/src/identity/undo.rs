//! What a change to an album's identity replaced, kept so it can be taken
//! back, and the two undos that use it.
//!
//! An album has at most one choice undo (`library_edition_choice_undo`):
//! the snapshot of its match, its files' matches, its match state and its
//! hold ("leave unmanaged", the active custom edition, a queued placement)
//! before the last change, keyed to the identity revision that change left.
//! Any later change to the identity moves the revision, so the undo is then
//! refused as stale.
//!
//! One exception: a hand back and a repair that takes the match off queue
//! an identification, whose seal would move the revision before the person
//! could use the undo. Their undo is marked carried, and the first
//! automatic seal or retraction keys it to the revision it leaves and
//! clears the mark. An automatic acceptance before that keys it to the
//! revision it leaves too, keeps the mark, and keeps no undo of its own.
//! An administrator's undo of that seal or acceptance keys it again, to
//! the revision the administrator's undo leaves.
//!
//! The choice undo also keeps the album's matches by hand: a change that
//! replaces a file's row ends its match, and the undo brings it back.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};

use super::reads::{album_identity_row, identity_revision, indexed_track_ids, track_identity_rows};
use super::sql;
use super::{DecisionSource, IdentityError, PROVIDER, Refusal, Rows, delete_album_row};
use crate::catalog_actions::ActionKind;
use crate::catalog_actions::CatalogAction;
use crate::catalog_actions::record_action;
use crate::identify::models::{PriorAlbumIdentity, PriorTrackIdentity};

/// An album's match state row, as the undo snapshot keeps it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct MatchStateRow {
    state: String,
    reason_code: String,
    release_mbid: Option<String>,
    candidates_json: String,
    updated_at: f64,
}

/// An album's "leave unmanaged" row, as the undo keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExclusionRow {
    reason: String,
    excluded_by_user_id: Option<String>,
    excluded_at: f64,
    row_revision: i64,
}

/// The same row. The time is compared to the millisecond, what the row keeps:
/// an undo's JSON may hold it at the finer resolution it had when written.
impl PartialEq for ExclusionRow {
    fn eq(&self, other: &Self) -> bool {
        self.reason == other.reason
            && self.excluded_by_user_id == other.excluded_by_user_id
            && self.row_revision == other.row_revision
            && crate::clock::ms(self.excluded_at) == crate::clock::ms(other.excluded_at)
    }
}

/// A queued placement of the album's files, as the undo keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RemapRow {
    release_mbid: String,
    chosen_by_user_id: Option<String>,
    origin: String,
    queued_at: f64,
}

/// The same queued ask. When it was queued is compared to the millisecond,
/// what the row keeps: an undo's JSON may hold it at the finer resolution it
/// had when written.
impl PartialEq for RemapRow {
    fn eq(&self, other: &Self) -> bool {
        self.release_mbid == other.release_mbid
            && self.chosen_by_user_id == other.chosen_by_user_id
            && self.origin == other.origin
            && crate::clock::ms(self.queued_at) == crate::clock::ms(other.queued_at)
    }
}

/// What holds the album apart beside its edition: a person's "leave
/// unmanaged", the active custom edition and a queued placement of its
/// files. A record kept before the placement was part of it reads as none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Hold {
    exclusion: Option<ExclusionRow>,
    custom_manifest_id: Option<String>,
    #[serde(default)]
    remap: Option<RemapRow>,
}

/// One file's match by hand, every column its record keeps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HandMatchRow {
    local_track_id: String,
    release_mbid: String,
    release_track_mbid: String,
    prior_identity_json: Option<String>,
    matched_by_user_id: Option<String>,
    matched_at: i64,
}

/// The album's hold before a change and as the change left it, whether
/// the undo is carried through the identification the change queued, and
/// the album's matches by hand before the change. A record without the
/// mark is not carried; one kept before matches by hand were part of it
/// leaves them as they are.
#[derive(Debug, Serialize, Deserialize)]
struct HoldChange {
    #[serde(default)]
    before: Hold,
    #[serde(default)]
    after: Hold,
    #[serde(default)]
    carried: bool,
    #[serde(default)]
    hand_matches: Option<Vec<HandMatchRow>>,
}

/// The album's "leave unmanaged" row, if it has one.
pub(crate) fn exclusion_row(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<ExclusionRow>> {
    conn.prepare_cached(sql::EXCLUSION_ROW.text())?
        .query_row(params![album_id], sql::StoredExclusion::read)
        .map(|row| ExclusionRow {
            reason: row.reason,
            excluded_by_user_id: row.excluded_by_user_id,
            excluded_at: crate::clock::secs(row.excluded_at.0),
            row_revision: row.row_revision,
        })
        .optional()
}

/// Put a kept "leave unmanaged" row back on the album. When the person who
/// set it has been removed since, `actor` stands in for them.
pub(crate) fn restore_exclusion(
    tx: &Transaction<'_>,
    album_id: &str,
    row: &ExclusionRow,
    actor: Option<&str>,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::RESTORE_EXCLUSION.text())?
        .execute(params![
            album_id,
            row.reason,
            row.excluded_by_user_id,
            crate::clock::ms(row.excluded_at),
            actor
        ])?;
    Ok(())
}

fn hold(conn: &Connection, album_id: &str) -> rusqlite::Result<Hold> {
    let exclusion = exclusion_row(conn, album_id)?;
    let custom_manifest_id = conn
        .prepare_cached(sql::CUSTOM_ACTIVE.text())?
        .query_row(params![album_id], |row| row.get(0))
        .optional()?;
    let remap = conn
        .prepare_cached(sql::REMAP_ROW.text())?
        .query_row(params![album_id], sql::StoredRemap::read)
        .map(|row| RemapRow {
            release_mbid: row.release_mbid,
            chosen_by_user_id: row.chosen_by_user_id,
            origin: row.origin,
            queued_at: crate::clock::secs(row.queued_at.0),
        })
        .optional()?;
    Ok(Hold {
        exclusion,
        custom_manifest_id,
        remap,
    })
}

fn hand_matches(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<HandMatchRow>> {
    conn.prepare_cached(sql::ALBUM_HAND_MATCHES.text())?
        .query_map(params![album_id], |row| {
            let row = sql::StoredHandMatch::read(row)?;
            Ok(HandMatchRow {
                local_track_id: row.local_track_id,
                release_mbid: row.release_mbid,
                release_track_mbid: row.release_track_mbid,
                prior_identity_json: row.prior_identity_json,
                matched_by_user_id: row.matched_by_user_id,
                matched_at: row.matched_at,
            })
        })?
        .collect()
}

/// Set each indexed file's match by hand to what `kept` holds for it: put
/// back as it was, or forgotten.
fn restore_hand_matches(
    tx: &Transaction<'_>,
    album_id: &str,
    kept: &[HandMatchRow],
) -> rusqlite::Result<()> {
    for track_id in indexed_track_ids(tx, album_id)? {
        match kept.iter().find(|row| row.local_track_id == track_id) {
            Some(row) => {
                tx.prepare_cached(sql::KEEP_HAND_MATCH.text())?
                    .execute(params![
                        row.local_track_id,
                        row.release_mbid,
                        row.release_track_mbid,
                        row.prior_identity_json,
                        row.matched_by_user_id,
                        row.matched_at,
                    ])?;
            }
            None => {
                tx.prepare_cached(sql::FORGET_HAND_MATCH.text())?
                    .execute(params![track_id])?;
            }
        }
    }
    Ok(())
}

fn match_state_row(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<MatchStateRow>> {
    conn.prepare_cached(sql::MATCH_STATE_ROW.text())?
        .query_row(params![album_id], sql::StoredMatchState::read)
        .map(|row| MatchStateRow {
            state: row.state,
            reason_code: row.reason_code,
            release_mbid: row.release_mbid,
            candidates_json: row.candidates_json,
            updated_at: crate::clock::secs(row.updated_at.0),
        })
        .optional()
}

/// What a change replaced, taken before it writes.
pub(crate) struct Snapshot {
    pub(crate) identity: Option<PriorAlbumIdentity>,
    pub(crate) tracks: Vec<PriorTrackIdentity>,
    pub(crate) state: Option<MatchStateRow>,
    hold: Hold,
    hand_matches: Vec<HandMatchRow>,
}

impl Snapshot {
    /// The state the album's match flag was in, if it had one.
    pub(crate) fn flag_state(&self) -> Option<&str> {
        self.state.as_ref().map(|state| state.state.as_str())
    }
}

pub(crate) fn snapshot(conn: &Connection, album_id: &str) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        identity: album_identity_row(conn, album_id)?,
        tracks: track_identity_rows(conn, album_id)?,
        state: match_state_row(conn, album_id)?,
        hold: hold(conn, album_id)?,
        hand_matches: hand_matches(conn, album_id)?,
    })
}

pub(crate) fn to_json<T: Serialize + ?Sized>(value: &T) -> rusqlite::Result<String> {
    serde_json::to_string(value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))
}

fn from_json<T: serde::de::DeserializeOwned>(raw: &str) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
    })
}

/// Keep `before` as the album's one undo, keyed to the identity revision
/// the change left behind, with the album's hold before and after it.
/// `carried` marks an undo that outlives the identification the change
/// queued.
pub(crate) fn keep_choice_undo(
    tx: &Transaction<'_>,
    album_id: &str,
    before: &Snapshot,
    action_id: &str,
    carried: bool,
    now: f64,
) -> rusqlite::Result<()> {
    let revision = identity_revision(tx, album_id)?;
    let identity = before.identity.as_ref().map(to_json).transpose()?;
    let state = before.state.as_ref().map(to_json).transpose()?;
    let held = to_json(&HoldChange {
        before: before.hold.clone(),
        after: hold(tx, album_id)?,
        carried,
        hand_matches: Some(before.hand_matches.clone()),
    })?;
    tx.prepare_cached(sql::KEEP_CHOICE_UNDO.text())?
        .execute(params![
            uuid::Uuid::new_v4().to_string(),
            album_id,
            action_id,
            identity,
            to_json(&before.tracks)?,
            state,
            revision,
            crate::clock::ms(now),
            held,
        ])?;
    Ok(())
}

/// After a person's change: write its audit row and keep `before` as the
/// album's one undo. Returns the audit row id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_and_keep(
    tx: &Transaction<'_>,
    album_id: &str,
    before: &Snapshot,
    actor: &str,
    kind: ActionKind,
    reason: &str,
    carried: bool,
    now: f64,
) -> rusqlite::Result<String> {
    let after = album_identity_row(tx, album_id)?;
    let action = record_action(
        tx,
        CatalogAction {
            actor,
            kind,
            album_id,
            job_id: None,
            before: serde_json::to_value(&before.identity).unwrap_or_default(),
            after: serde_json::to_value(&after).unwrap_or_default(),
            reason,
        },
        now,
    )?;
    keep_choice_undo(tx, album_id, before, &action, carried, now)?;
    Ok(action)
}

/// Key a carried undo still at identity revision `from` to `to`, the
/// revision an automatic seal or retraction left, and clear its mark.
pub(crate) fn carry_undo(
    tx: &Transaction<'_>,
    album_id: &str,
    from: i64,
    to: i64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::CARRY_CHOICE_UNDO.text())?
        .execute(params![album_id, from, to])?;
    Ok(())
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
                tx.prepare_cached(sql::DELETE_EXCLUSION.text())?
                    .execute(params![album_id])?;
            }
            Some(row) => restore_exclusion(tx, album_id, row, Some(actor))?,
        }
    }
    if before.custom_manifest_id != after.custom_manifest_id
        && current.custom_manifest_id == after.custom_manifest_id
    {
        match &before.custom_manifest_id {
            None => {
                tx.prepare_cached(sql::DROP_CUSTOM.text())?
                    .execute(params![album_id])?;
            }
            Some(manifest) => {
                // A manifest that is gone since is not put back.
                tx.prepare_cached(sql::RESTORE_CUSTOM.text())?
                    .execute(params![album_id, manifest, crate::clock::ms(now)])?;
            }
        }
    }
    if before.remap != after.remap && current.remap == after.remap {
        match &before.remap {
            None => {
                tx.prepare_cached(sql::DROP_REMAP.text())?
                    .execute(params![album_id])?;
            }
            Some(row) => {
                tx.prepare_cached(sql::RESTORE_REMAP.text())?
                    .execute(params![
                        album_id,
                        row.release_mbid,
                        row.chosen_by_user_id,
                        crate::clock::ms(row.queued_at),
                        row.origin
                    ])?;
            }
        }
    }
    Ok(())
}

fn put_match_state(
    tx: &Transaction<'_>,
    album_id: &str,
    state: Option<&MatchStateRow>,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::CLEAR_FLAG.text())?
        .execute(params![album_id])?;
    if let Some(state) = state {
        tx.prepare_cached(sql::PUT_MATCH_STATE.text())?
            .execute(params![
                album_id,
                state.state,
                state.reason_code,
                state.release_mbid,
                state.candidates_json,
                crate::clock::ms(state.updated_at),
            ])?;
    }
    Ok(())
}

/// Put back the match state an automatic acceptance replaced, when the
/// acceptance that left identity revision `post_identity_revision` kept
/// one, and use up that edition undo (the change it covers is gone). Only
/// the acceptance's own undo is used up; another one at that revision is
/// a person's the seal carried.
pub(crate) fn restore_match_state(
    tx: &Transaction<'_>,
    album_id: &str,
    post_identity_revision: i64,
) -> rusqlite::Result<()> {
    let kept = tx
        .prepare_cached(sql::CHOICE_UNDO_AT.text())?
        .query_row(
            params![album_id, post_identity_revision],
            sql::ChoiceUndoAt::read,
        )
        .optional()?;
    let Some(kept) = kept else {
        return Ok(());
    };
    let prior: Option<MatchStateRow> = kept
        .prior_match_state_json
        .as_deref()
        .map(from_json)
        .transpose()?;
    put_match_state(tx, album_id, prior.as_ref())?;
    tx.prepare_cached(sql::CONSUME_CHOICE_UNDO.text())?
        .execute(params![kept.id])?;
    Ok(())
}

/// Write one snapshot track row back exactly; an existing row takes the
/// next revision. An attempt pruned since the snapshot restores as no
/// attempt rather than failing the foreign key.
pub(crate) fn restore_track_row(
    tx: &Transaction<'_>,
    row: &PriorTrackIdentity,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::RESTORE_TRACK_ROW.text())?
        .execute(params![
            row.local_track_id,
            PROVIDER,
            row.recording_mbid,
            row.release_mbid,
            row.release_track_mbid,
            row.medium_position,
            row.release_track_position,
            row.decision_source.unwrap_or(DecisionSource::Automatic),
            row.attempt_id,
            crate::clock::ms(row.selected_at.unwrap_or(now)),
        ])?;
    Ok(())
}

/// Write a snapshot album row back; an existing row takes the next
/// revision, and a pruned attempt or a deleted account restores as none.
pub(crate) fn restore_album_row(
    tx: &Transaction<'_>,
    album_id: &str,
    prior: &PriorAlbumIdentity,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::RESTORE_ALBUM_ROW.text())?
        .execute(params![
            album_id,
            prior.release_group_mbid,
            prior.release_mbid,
            prior.decision_source.unwrap_or(DecisionSource::Automatic),
            prior.matcher_version,
            prior.attempt_id,
            prior.selected_by_user_id,
            crate::clock::ms(prior.selected_at.unwrap_or(now)),
            PROVIDER,
        ])?;
    Ok(())
}

/// Take back the album's last edition change: the identity, track rows and
/// match state go back to what they were, and so does each part of the
/// album's hold the change altered, while that part is still as the change
/// left it. Refused once anything else has changed the album's identity
/// since. The automatic-edition undo goes with it: the identity it was kept
/// for is no longer the album's.
pub fn undo(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), IdentityError> {
    let kept = tx
        .prepare_cached(sql::CHOICE_UNDO.text())?
        .query_row(params![album_id], sql::ChoiceUndoRow::read)
        .optional()?;
    let Some(kept) = kept else {
        return Err(Refusal::NoEditionChoice.into());
    };
    let current = album_identity_row(tx, album_id)?;
    if current
        .as_ref()
        .and_then(|row| row.row_revision)
        .unwrap_or(0)
        != kept.expected_identity_revision
    {
        return Err(Refusal::EditionChoiceStale.into());
    }
    let prior: Option<PriorAlbumIdentity> = kept
        .prior_identity_json
        .as_deref()
        .map(from_json)
        .transpose()?;
    let prior_tracks: Vec<PriorTrackIdentity> = from_json(&kept.prior_tracks_json)?;
    let prior_state: Option<MatchStateRow> = kept
        .prior_match_state_json
        .as_deref()
        .map(from_json)
        .transpose()?;
    let held: Option<HoldChange> = kept.prior_hold_json.as_deref().map(from_json).transpose()?;

    match prior.filter(|prior| prior.release_group_mbid.is_some()) {
        Some(prior) => restore_album_row(tx, album_id, &prior, now)?,
        None => {
            delete_album_row(tx, album_id, Rows::Any)?;
        }
    }
    for track_id in indexed_track_ids(tx, album_id)? {
        match prior_tracks
            .iter()
            .find(|row| row.local_track_id == track_id && row.recording_mbid.is_some())
        {
            Some(row) => restore_track_row(tx, row, now)?,
            None => {
                tx.prepare_cached(sql::DELETE_TRACK_ROW.text())?
                    .execute(params![track_id, PROVIDER])?;
            }
        }
    }
    put_match_state(tx, album_id, prior_state.as_ref())?;
    if let Some(change) = &held {
        restore_hold(tx, album_id, change, actor, now)?;
        if let Some(kept) = &change.hand_matches {
            restore_hand_matches(tx, album_id, kept)?;
        }
    }
    for statement in [sql::FORGET_CHOICE_UNDO, sql::FORGET_AUTOMATIC_UNDO] {
        tx.prepare_cached(statement.text())?
            .execute(params![album_id])?;
    }
    crate::management::automatic::queue(tx, album_id, now)?;
    record_action(
        tx,
        CatalogAction {
            actor,
            kind: ActionKind::EditionChoiceUndone,
            album_id,
            job_id: None,
            before: serde_json::to_value(&current).unwrap_or_default(),
            after: serde_json::json!({ "undo_id": kept.id }),
            reason: "EDITION_CHOICE_UNDONE",
        },
        now,
    )?;
    crate::catalog_revision::bump_catalog(tx)?;
    Ok(())
}

/// How undoing an automatic edition ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = AutomaticEditionUndoOutcome)]
pub enum AutomaticUndone {
    /// The album's earlier match is back.
    Restored,
    /// There was none: the album has no match now, and the caller files a
    /// review for it.
    ClearedToReview,
}

/// What an administrator echoes to undo an automatic edition.
#[derive(Debug, Clone, Copy)]
pub struct AutomaticUndo<'a> {
    pub expected_album_revision: i64,
    pub expected_identity_revision: i64,
    pub actor: &'a str,
    /// The review the caller files when nothing comes back; named in the
    /// audit row.
    pub review_id: &'a str,
    pub now: f64,
}

/// Put back what the album's last automatic edition replaced, column for
/// column. With no earlier album identity, the album is
/// left with no match and the automatic undo goes. Track rows a curator
/// holds now are never rewritten, and pins are never touched. An identity
/// check's acceptance also kept the album's match state in the edition
/// undo it left: that is put back and the undo used up first. A person's
/// undo the seal carried is kept, keyed to the revision this undo leaves
/// (0 with no match), so it still brings their edition back.
pub fn undo_automatic(
    tx: &Transaction<'_>,
    album_id: &str,
    asked: &AutomaticUndo<'_>,
) -> Result<AutomaticUndone, IdentityError> {
    let now = asked.now;
    let undo = tx
        .prepare_cached(sql::AUTOMATIC_UNDO.text())?
        .query_row(params![album_id], sql::AutomaticUndoRow::read)
        .optional()?;
    let Some(undo) = undo else {
        return Err(Refusal::NoAutomaticEdition.into());
    };
    let (undo_id, post_album_revision, post_identity_revision) = (
        undo.id,
        undo.expected_post_album_revision,
        undo.expected_post_identity_revision,
    );
    let album_revision: Option<i64> = tx
        .prepare_cached(sql::ALBUM_REVISION.text())?
        .query_row(params![album_id], |row| row.get(0))
        .optional()?;
    let current = album_identity_row(tx, album_id)?;
    // Only the row the automatic seal wrote is undone here.
    let fresh = album_revision == Some(asked.expected_album_revision)
        && asked.expected_album_revision == post_album_revision
        && current.as_ref().is_some_and(|identity| {
            identity.decision_source == Some(DecisionSource::Automatic)
                && identity.row_revision == Some(asked.expected_identity_revision)
                && asked.expected_identity_revision == post_identity_revision
        });
    if !fresh {
        return Err(Refusal::AutomaticUndoStale.into());
    }
    let prior: Option<PriorAlbumIdentity> = undo
        .prior_identity_json
        .as_deref()
        .map(from_json)
        .transpose()?;
    let prior_tracks: Vec<PriorTrackIdentity> = from_json(&undo.prior_track_identities_json)?;
    let current_tracks = track_identity_rows(tx, album_id)?;

    let prior =
        prior.filter(|prior| prior.release_group_mbid.is_some() && prior.decision_source.is_some());
    let outcome = if prior.is_some() {
        AutomaticUndone::Restored
    } else {
        AutomaticUndone::ClearedToReview
    };
    restore_match_state(tx, album_id, post_identity_revision)?;
    tx.prepare_cached(sql::MARK_CARRIED.text())?
        .execute(params![album_id, post_identity_revision])?;
    match &prior {
        Some(prior) => {
            tx.prepare_cached(sql::PUT_BACK_ALBUM_ROW.text())?
                .execute(params![
                    album_id,
                    prior.release_group_mbid,
                    prior.release_mbid,
                    prior.decision_source,
                    prior.matcher_version,
                    prior.attempt_id,
                    prior.selected_by_user_id,
                    crate::clock::ms(prior.selected_at.unwrap_or(now)),
                    PROVIDER,
                ])?;
        }
        None => {
            // The row is the automatic one (`fresh`); a carried undo stays,
            // keyed to the album having no match. A flag says so (the one put
            // back, else a new one), so the files' tags do not bring the
            // match back.
            delete_album_row(tx, album_id, Rows::Revisable)?;
            tx.prepare_cached(sql::MARK_UNMATCHED_UNLESS_FLAGGED.text())?
                .execute(params![
                    album_id,
                    "AUTOMATIC_EDITION_UNDONE",
                    crate::clock::ms(now)
                ])?;
        }
    }
    for track_id in indexed_track_ids(tx, album_id)? {
        let now_row = current_tracks
            .iter()
            .find(|row| row.local_track_id == track_id);
        // Only what the automatic seal wrote is put back; a curator's (or
        // the tags') row stays as it is.
        if now_row.is_some_and(|row| row.decision_source != Some(DecisionSource::Automatic)) {
            continue;
        }
        let before = prior_tracks
            .iter()
            .find(|row| row.local_track_id == track_id && row.recording_mbid.is_some());
        match before {
            Some(row) => restore_track_row(tx, row, now)?,
            None if now_row.is_some() => {
                tx.prepare_cached(sql::DELETE_TRACK_ROW.text())?
                    .execute(params![track_id, PROVIDER])?;
            }
            None => {}
        }
    }
    let review_id = (outcome == AutomaticUndone::ClearedToReview).then_some(asked.review_id);
    let action = record_action(
        tx,
        CatalogAction {
            actor: asked.actor,
            kind: ActionKind::UndoAutomaticEdition,
            album_id,
            job_id: None,
            before: serde_json::json!({ "identity": current, "tracks": current_tracks }),
            after: serde_json::json!({
                "outcome": match outcome {
                    AutomaticUndone::Restored => "restored",
                    AutomaticUndone::ClearedToReview => "cleared_to_review",
                },
                "review_id": review_id,
                "undo_snapshot_id": undo_id,
            }),
            reason: "AUTOMATIC_EDITION_UNDONE",
        },
        now,
    )?;
    tx.prepare_cached(sql::CONSUME_AUTOMATIC_UNDO.text())?
        .execute(params![undo_id, crate::clock::ms(now), action])?;
    if outcome == AutomaticUndone::Restored {
        carry_undo(
            tx,
            album_id,
            post_identity_revision,
            identity_revision(tx, album_id)?,
        )?;
    }
    crate::catalog_revision::bump_catalog(tx)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::ExclusionRow;

    /// An undo kept before times were whole milliseconds still matches the
    /// row the upgrade rounded, so undoing it lifts the "leave unmanaged".
    #[test]
    fn a_kept_exclusion_matches_the_row_rounded_to_the_millisecond() {
        let row = |excluded_at| ExclusionRow {
            reason: "administrator_choice".to_owned(),
            excluded_by_user_id: Some("u1".to_owned()),
            excluded_at,
            row_revision: 1,
        };
        assert_eq!(row(1_760_000_000.123_456), row(1_760_000_000.123));
        assert_ne!(row(1_760_000_000.123), row(1_760_000_000.124));
    }
}
