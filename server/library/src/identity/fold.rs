//! Folding the stray half of an album a v2 import split in two into the
//! album its files' own tags name, and putting it back.
//!
//! The fold is the one automatic step that drops a choice carried over
//! from v2: a catalog repair makes it only when every file's tags name
//! the other album and nothing else could be meant. So it keeps what it
//! dropped in its audit row, and [`undo_fold`] brings it back.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};

use super::reads::{album_identity_row, track_row};
use super::sql;
use super::undo::{
    ExclusionRow, exclusion_row, restore_album_row, restore_exclusion, restore_track_row,
};
use super::{DecisionSource, PROVIDER, Rows, delete_album_row};
use crate::catalog_revision::bump_catalog;
use crate::identify::models::{PriorAlbumIdentity, PriorTrackIdentity};
use crate::identify::queue::offer_album;
use crate::operations::decisions::{CatalogAction, record_action};

/// The reason code of a fold's audit row.
pub const FOLD_REASON: &str = "AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR";

/// The audit kind of an undone fold, recorded on the album it brought
/// back. A fold never runs again on that album.
pub const FOLD_UNDONE: &str = "fold_undone";

/// An album's takeover mark, as the fold keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeoverMark {
    pub cause: String,
    pub noticed_at: i64,
    pub handed_back_at: Option<i64>,
}

/// What a fold dropped, kept as its audit row's `before_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folded {
    /// The album folded away.
    pub source_album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: String,
    /// The source album's grouping lock before it was retired.
    pub grouping_locked: bool,
    /// The source album's v2 match.
    pub identity: PriorAlbumIdentity,
    /// The v2 matches of the files it held.
    pub track_identities: Vec<PriorTrackIdentity>,
    /// The files moved onto the album that kept them.
    pub moved_track_ids: Vec<String>,
    /// The source album's "leave unmanaged", which its retirement moved
    /// onto the album that kept the files.
    pub exclusion: Option<ExclusionRow>,
    /// The source album's takeover mark, moved the same way.
    pub takeover: Option<TakeoverMark>,
}

/// Whether a fold of the album was undone: the repair leaves it alone.
pub fn fold_undone(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(sql::FOLD_UNDONE.text())?
        .query_row(params![album_id, FOLD_UNDONE], |row| row.get(0))
}

/// Drop the v2 match of `source_album_id` and of the files it moves
/// (`moved_track_ids`), returning what was dropped for the audit row. The
/// caller moves the files and retires the album in the same transaction.
pub(crate) fn fold_legacy(
    tx: &Transaction<'_>,
    source_album_id: &str,
    moved_track_ids: &[String],
) -> rusqlite::Result<Folded> {
    let identity = album_identity_row(tx, source_album_id)?.unwrap_or_default();
    let mut track_identities = Vec::new();
    for track_id in moved_track_ids {
        if let Some(row) = track_row(tx, track_id)?
            && row.decision_source.as_deref() == Some(DecisionSource::LegacyImport.as_str())
        {
            track_identities.push(row);
        }
        tx.prepare_cached(sql::DROP_LEGACY_TRACK_ROW.text())?
            .execute(params![track_id, PROVIDER])?;
    }
    let grouping_locked: bool = tx
        .prepare_cached(sql::GROUPING_LOCKED.text())?
        .query_row(params![source_album_id], |row| row.get(0))?;
    let exclusion = exclusion_row(tx, source_album_id)?;
    let takeover = tx
        .prepare_cached(sql::TAKEOVER_ROW.text())?
        .query_row(params![source_album_id], |row| {
            Ok(TakeoverMark {
                cause: row.get(0)?,
                noticed_at: row.get(1)?,
                handed_back_at: row.get(2)?,
            })
        })
        .optional()?;
    delete_album_row(tx, source_album_id, Rows::Any)?;
    Ok(Folded {
        source_album_id: source_album_id.to_owned(),
        release_group_mbid: identity.release_group_mbid.clone().unwrap_or_default(),
        release_mbid: identity.release_mbid.clone().unwrap_or_default(),
        grouping_locked,
        identity,
        track_identities,
        moved_track_ids: moved_track_ids.to_vec(),
        exclusion,
        takeover,
    })
}

/// How [`undo_fold`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldUndo {
    /// The album and its v2 match are back.
    Restored,
    /// A moved file left the album that kept it, is no longer indexed, or
    /// holds a person's match or a match by hand made since, or the folded
    /// album is no longer retired into it: nothing changed.
    Stale,
    /// `action_id` is not a fold that kept what it dropped: nothing
    /// changed.
    Unknown,
}

/// Undo the fold recorded as audit row `action_id`, for `actor` (`None`
/// for the app): the folded album comes back from retirement, the moved
/// files go back to it, its v2 match and its files' v2 matches are written
/// again, its "leave unmanaged" and takeover mark come back, the merged
/// alias goes, both albums are offered to identification, an audit row
/// ([`FOLD_UNDONE`]) on the album keeps the repair from folding it again,
/// and the catalog revision moves once. A match a person made since on a
/// moved file is never replaced: the undo is then stale.
///
/// It does not reverse the rest of what the album's retirement moved onto
/// the album that kept the files: references to the folded album (plays,
/// favourites, ratings, playlist entries, bookmarks and aliases, and albums
/// retired into it earlier), its field overrides, its rejected reviews and
/// its dropped identification jobs. A copy of its "leave unmanaged" or
/// takeover mark on that album stays there.
pub fn undo_fold(
    tx: &Transaction<'_>,
    action_id: &str,
    actor: Option<&str>,
    now: f64,
) -> rusqlite::Result<FoldUndo> {
    let (folded, target) = match standing(tx, action_id)? {
        Ok(fold) => fold,
        Err(outcome) => return Ok(outcome),
    };
    let source = folded.source_album_id.as_str();
    tx.prepare_cached(sql::UNRETIRE_ALBUM.text())?
        .execute(params![source, folded.grouping_locked, now])?;
    for track_id in &folded.moved_track_ids {
        tx.prepare_cached(sql::MOVE_TRACK_TO.text())?
            .execute(params![source, track_id])?;
    }
    if folded.identity.release_group_mbid.is_some() {
        restore_album_row(tx, source, &folded.identity, now)?;
    }
    for row in &folded.track_identities {
        restore_track_row(tx, row, now)?;
    }
    if let Some(row) = &folded.exclusion {
        restore_exclusion(tx, source, row, actor)?;
    }
    if let Some(mark) = &folded.takeover {
        tx.prepare_cached(sql::RESTORE_TAKEOVER.text())?
            .execute(params![
                source,
                mark.cause,
                mark.noticed_at,
                mark.handed_back_at
            ])?;
    }
    tx.prepare_cached(sql::DROP_MERGED_ALIAS.text())?
        .execute(params![source, target])?;
    tx.prepare_cached(sql::TOUCH_ALBUM.text())?
        .execute(params![target, now])?;
    let now_ms = (now * 1000.0) as i64;
    offer_album(tx, source, now_ms)?;
    offer_album(tx, &target, now_ms)?;
    record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind: FOLD_UNDONE,
            album_id: source,
            job_id: None,
            before: serde_json::json!({ "surviving_album_id": target }),
            after: serde_json::json!({
                "fold_action_id": action_id,
                "moved_track_ids": folded.moved_track_ids,
            }),
            reason: "FOLD_UNDONE",
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(FoldUndo::Restored)
}

/// Whether [`undo_fold`] would bring back the fold recorded as audit row
/// `action_id` now. Reads only.
pub fn fold_undoable(conn: &Connection, action_id: &str) -> rusqlite::Result<bool> {
    Ok(standing(conn, action_id)?.is_ok())
}

/// The fold recorded as `action_id` and the album that kept its files, or
/// why [`undo_fold`] would refuse it.
fn standing(
    conn: &Connection,
    action_id: &str,
) -> rusqlite::Result<Result<(Folded, String), FoldUndo>> {
    let action: Option<(String, String)> = conn
        .prepare_cached(sql::FOLD_ACTION.text())?
        .query_row(params![action_id, FOLD_REASON], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((before, target)) = action else {
        return Ok(Err(FoldUndo::Unknown));
    };
    let Ok(folded) = serde_json::from_str::<Folded>(&before) else {
        return Ok(Err(FoldUndo::Unknown));
    };
    let retired: bool = conn
        .prepare_cached(sql::RETIRED_INTO.text())?
        .query_row(params![folded.source_album_id, target], |row| row.get(0))?;
    if !retired {
        return Ok(Err(FoldUndo::Stale));
    }
    for track_id in &folded.moved_track_ids {
        let there: bool = conn
            .prepare_cached(sql::INDEXED_ON.text())?
            .query_row(params![track_id, target], |row| row.get(0))?;
        let persons: bool = conn
            .prepare_cached(sql::PERSONS_TRACK.text())?
            .query_row(params![track_id, PROVIDER], |row| row.get(0))?;
        if !there || persons {
            return Ok(Err(FoldUndo::Stale));
        }
    }
    Ok(Ok((folded, target)))
}
