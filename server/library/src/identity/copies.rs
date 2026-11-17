//! Copies of one edition in the library: the copy new files for that
//! edition join, the copies split across folders that a scan's end joins
//! into one album, and the undo of that join.
//!
//! The join itself is the retag merge's (`corrections::retag_merge`): it
//! keeps what it is about to drop through [`record_copy`] in its audit
//! row, and [`undo_copy_join`] brings that back. No file is read or
//! written by either.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};

use super::fold::{FoldUndo, TakeoverMark};
use super::reads::{album_identity_row, track_row};
use super::sql;
use super::undo::{
    ExclusionRow, exclusion_row, restore_album_row, restore_exclusion, restore_track_row,
};
use super::{DecisionSource, PROVIDER};
use crate::catalog_actions::ActionKind;
use crate::catalog_actions::CatalogAction;
use crate::catalog_actions::record_action;
use crate::catalog_revision::bump_catalog;
use crate::identify::models::{PriorAlbumIdentity, PriorTrackIdentity};

/// The reason code of the audit row of two copies joined at a scan's end.
pub const COPIES_JOINED_REASON: &str = "SAME_EDITION_COPIES_JOINED";

/// The reason code of the undo of that join, recorded on the copy it
/// brought back: the scan's end leaves that copy apart from then on.
pub const COPIES_JOIN_UNDONE_REASON: &str = "COPIES_JOIN_UNDONE";

/// A live copy new files may join: its album and the release its row
/// names, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCopy {
    pub album_id: String,
    pub release_mbid: String,
}

/// The copy on `release_mbid` that files for these release tracks and
/// recordings (`wanted`, any case) join: live with an indexed file, its
/// match accepted or a person's choice on that release with no custom
/// edition, and holding none of them on an indexed file. A person's
/// choice first, then the oldest. `None` when no copy qualifies.
///
/// The candidates and what each holds are read in one closure by the
/// caller, so they agree.
pub fn copy_on_release(
    conn: &Connection,
    release_mbid: &str,
    wanted: &[(String, String)],
) -> rusqlite::Result<Option<ReleaseCopy>> {
    let release = release_mbid.trim().to_ascii_lowercase();
    if release.is_empty() {
        return Ok(None);
    }
    let copies: Vec<ReleaseCopy> = conn
        .prepare_cached(sql::COPIES_ON_RELEASE.text())?
        .query_map(params![release, PROVIDER], |row| {
            let row = sql::ReleaseCopy::read(row)?;
            Ok(ReleaseCopy {
                album_id: row.local_album_id,
                release_mbid: row.release_mbid,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let lowered = |value: &str| {
        let value = value.trim().to_ascii_lowercase();
        (!value.is_empty()).then_some(value)
    };
    let tracks: Vec<String> = wanted.iter().filter_map(|(on, _)| lowered(on)).collect();
    let recordings: Vec<String> = wanted.iter().filter_map(|(_, rec)| lowered(rec)).collect();
    for copy in copies {
        let holds = conn
            .prepare_cached(sql::COPY_HOLDS.text())?
            .query_map(params![copy.album_id, PROVIDER], sql::CopyHolds::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let held = holds.iter().any(|held| {
            held.release_track
                .as_ref()
                .is_some_and(|on| tracks.contains(on))
                || held
                    .recording
                    .as_ref()
                    .is_some_and(|rec| recordings.contains(rec))
        });
        if !held {
            return Ok(Some(copy));
        }
    }
    Ok(None)
}

/// Each release at least two live copies with an accepted match name,
/// with those copies in the order the library shows a copy: a person's
/// choice first, then the oldest.
pub(crate) fn split_copies(conn: &Connection) -> rusqlite::Result<Vec<(String, Vec<String>)>> {
    let releases: Vec<String> = conn
        .prepare_cached(sql::SPLIT_RELEASES.text())?
        .query_map(params![PROVIDER], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut split = Vec::with_capacity(releases.len());
    for release in releases {
        let copies = conn
            .prepare_cached(sql::SPLIT_COPIES.text())?
            .query_map(params![release, PROVIDER], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        split.push((release, copies));
    }
    Ok(split)
}

/// Whether `copy` could not join `survivor` on `release` (lowered): a file
/// of either is not placed on a track of the release, or the two hold a
/// release track or a recording in common on their indexed files. Files in
/// the recycle bin are not counted.
pub(crate) fn copies_overlap(
    conn: &Connection,
    survivor: &str,
    copy: &str,
    release: &str,
) -> rusqlite::Result<bool> {
    for album in [survivor, copy] {
        let unplaced: bool = conn
            .prepare_cached(sql::HAS_UNPLACED_FILE.text())?
            .query_row(params![album, PROVIDER, release], |row| row.get(0))?;
        if unplaced {
            return Ok(true);
        }
    }
    conn.prepare_cached(sql::COPIES_SHARE.text())?
        .query_row(params![survivor, copy, PROVIDER], |row| row.get(0))
}

/// What the join of a copy into another drops or moves, kept as its audit
/// row's `before_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinedCopy {
    /// The copy retired into the album that kept the files.
    pub retired_album_id: String,
    /// The copy's grouping lock before it was retired.
    pub grouping_locked: bool,
    /// The copy's match.
    pub identity: PriorAlbumIdentity,
    /// Every file of the copy, moved onto the album that kept them.
    pub moved_track_ids: Vec<String>,
    /// The matches of those files that had one.
    pub track_identities: Vec<PriorTrackIdentity>,
    /// Those files matched by hand.
    pub hand_matched: Vec<String>,
    /// The copy's "leave unmanaged".
    pub exclusion: Option<ExclusionRow>,
    /// The copy's takeover mark.
    pub takeover: Option<TakeoverMark>,
    /// How each moved file was held to the copy before the join locked it
    /// to the album that kept it.
    pub memberships: Vec<Membership>,
    /// Whether the album that kept the files had a "leave unmanaged" of its
    /// own: the join copies the copy's onto it when it has none.
    pub survivor_exclusion: bool,
    /// Whether the copy was in the automatic management queue, and stamped
    /// there as retagged by hand.
    pub queued: bool,
    pub retag_followed: bool,
}

/// How a file was held to its album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub track_id: String,
    pub source: String,
    pub locked: bool,
}

/// Read what joining `copy` into `survivor` drops or moves, before
/// anything is written.
pub(crate) fn record_copy(
    conn: &Connection,
    copy: &str,
    survivor: &str,
) -> rusqlite::Result<JoinedCopy> {
    let identity = album_identity_row(conn, copy)?.unwrap_or_default();
    let memberships: Vec<Membership> = conn
        .prepare_cached(sql::ALBUM_FILES.text())?
        .query_map(params![copy], |row| {
            let row = sql::FileMembership::read(row)?;
            Ok(Membership {
                track_id: row.id,
                source: row.membership_source,
                locked: row.membership_locked,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let moved_track_ids: Vec<String> = memberships.iter().map(|m| m.track_id.clone()).collect();
    let mut track_identities = Vec::new();
    for track_id in &moved_track_ids {
        if let Some(row) = track_row(conn, track_id)? {
            track_identities.push(row);
        }
    }
    let hand_matched = conn
        .prepare_cached(sql::HAND_MATCHED_ON.text())?
        .query_map(params![copy], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let grouping_locked: bool = conn
        .prepare_cached(sql::GROUPING_LOCKED.text())?
        .query_row(params![copy], |row| row.get(0))?;
    let takeover = takeover_mark(conn, copy)?;
    let has = |statement: &str, album: &str| -> rusqlite::Result<bool> {
        conn.prepare_cached(statement)?
            .query_row(params![album], |row| row.get(0))
    };
    let queue_row: Option<bool> = conn
        .prepare_cached(sql::QUEUE_ROW.text())?
        .query_row(params![copy], |row| row.get(0))
        .optional()?;
    Ok(JoinedCopy {
        retired_album_id: copy.to_owned(),
        grouping_locked,
        identity,
        moved_track_ids,
        track_identities,
        hand_matched,
        exclusion: exclusion_row(conn, copy)?,
        takeover,
        memberships,
        survivor_exclusion: has(sql::HAS_EXCLUSION.text(), survivor)?,
        queued: queue_row.is_some(),
        retag_followed: queue_row.unwrap_or(false),
    })
}

/// The album's takeover mark, if it has one.
fn takeover_mark(conn: &Connection, album: &str) -> rusqlite::Result<Option<TakeoverMark>> {
    conn.prepare_cached(sql::TAKEOVER_ROW.text())?
        .query_row(params![album], |row| {
            Ok(TakeoverMark {
                cause: row.get(0)?,
                noticed_at: row.get(1)?,
                handed_back_at: row.get(2)?,
            })
        })
        .optional()
}

/// Undo the join of two copies recorded as audit row `action_id`, for
/// `actor` (`None` for the app): the copy comes back from retirement, its
/// files go back to it held as they were, its match and its files' matches
/// are written again, its "leave unmanaged" and takeover mark come back
/// when they are gone, the "leave unmanaged" the join copied onto the album
/// that kept the files leaves it (a copied takeover mark stays: the app may
/// rely on it since), its place in the automatic management queue
/// comes back with its "retagged by hand" stamp, the merged alias goes,
/// and an audit row (`fold_undone`) on the copy keeps the scan's end from
/// joining it again. The catalog revision moves once.
///
/// Stale, changing nothing, when the copy is no longer retired into the
/// album that kept its files, a moved file left that album or was matched
/// by hand since, or a moved file's match is neither the one recorded nor
/// that album's placement of it on the same release track. Hearts,
/// ratings, plays and playlist entries the join moved stay where they
/// are, and so do files automatic management moved since.
pub fn undo_copy_join(
    tx: &Transaction<'_>,
    action_id: &str,
    actor: Option<&str>,
    now: f64,
) -> rusqlite::Result<FoldUndo> {
    let (joined, survivor) = match standing(tx, action_id)? {
        Ok(join) => join,
        Err(outcome) => return Ok(outcome),
    };
    let copy = joined.retired_album_id.as_str();
    tx.prepare_cached(sql::UNRETIRE_ALBUM.text())?
        .execute(params![copy, joined.grouping_locked, crate::clock::ms(now)])?;
    for held in &joined.memberships {
        tx.prepare_cached(sql::RESTORE_MEMBERSHIP.text())?
            .execute(params![copy, held.track_id, held.source, held.locked])?;
    }
    if joined.identity.release_group_mbid.is_some() {
        restore_album_row(tx, copy, &joined.identity, now)?;
    }
    for row in &joined.track_identities {
        restore_track_row(tx, row, now)?;
    }
    if let Some(row) = &joined.exclusion {
        let kept: bool = tx
            .prepare_cached(sql::HAS_EXCLUSION.text())?
            .query_row(params![copy], |row| row.get(0))?;
        if !kept {
            restore_exclusion(tx, copy, row, actor)?;
        }
        let copied = exclusion_row(tx, &survivor)?.is_some_and(|on| on.same_choice(row));
        if !joined.survivor_exclusion && copied {
            tx.prepare_cached(sql::DELETE_EXCLUSION.text())?
                .execute(params![survivor])?;
        }
    }
    if let Some(mark) = &joined.takeover {
        tx.prepare_cached(sql::RESTORE_TAKEOVER.text())?
            .execute(params![
                copy,
                mark.cause,
                mark.noticed_at,
                mark.handed_back_at
            ])?;
    }
    if joined.retag_followed {
        crate::management::automatic::stamp_retag_followed(tx, copy, now)?;
    } else if joined.queued {
        crate::management::automatic::queue(tx, copy, now)?;
    }
    tx.prepare_cached(sql::DROP_MERGED_ALIAS.text())?
        .execute(params![copy, survivor])?;
    tx.prepare_cached(sql::TOUCH_ALBUM.text())?
        .execute(params![survivor, crate::clock::ms(now)])?;
    record_action(
        tx,
        CatalogAction {
            actor: actor.unwrap_or("system"),
            kind: ActionKind::FoldUndone,
            album_id: copy,
            job_id: None,
            before: serde_json::json!({ "surviving_album_id": survivor }),
            after: serde_json::json!({
                "fold_action_id": action_id,
                "moved_track_ids": joined.moved_track_ids,
            }),
            reason: COPIES_JOIN_UNDONE_REASON,
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(FoldUndo::Restored)
}

/// Whether [`undo_copy_join`] would bring back the copy joined in audit
/// row `action_id` now. Reads only.
pub fn copy_join_undoable(conn: &Connection, action_id: &str) -> rusqlite::Result<bool> {
    Ok(standing(conn, action_id)?.is_ok())
}

/// The join recorded as `action_id` and the album that kept the files, or
/// why [`undo_copy_join`] would refuse it.
fn standing(
    conn: &Connection,
    action_id: &str,
) -> rusqlite::Result<Result<(JoinedCopy, String), FoldUndo>> {
    let action: Option<(String, String)> = conn
        .prepare_cached(sql::FOLD_ACTION.text())?
        .query_row(params![action_id, COPIES_JOINED_REASON], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let Some((before, survivor)) = action else {
        return Ok(Err(FoldUndo::Unknown));
    };
    let Ok(joined) = serde_json::from_str::<JoinedCopy>(&before) else {
        return Ok(Err(FoldUndo::Unknown));
    };
    let retired: bool = conn
        .prepare_cached(sql::RETIRED_INTO.text())?
        .query_row(params![joined.retired_album_id, survivor], |row| row.get(0))?;
    if !retired {
        return Ok(Err(FoldUndo::Stale));
    }
    for track_id in &joined.moved_track_ids {
        let there: bool = conn
            .prepare_cached(sql::ON_ALBUM.text())?
            .query_row(params![track_id, survivor], |row| row.get(0))?;
        let hand: bool = conn
            .prepare_cached(sql::HAND_MATCHED.text())?
            .query_row(params![track_id], |row| row.get(0))?;
        if !there || (hand && !joined.hand_matched.contains(track_id)) {
            return Ok(Err(FoldUndo::Stale));
        }
        let recorded = joined
            .track_identities
            .iter()
            .find(|row| &row.local_track_id == track_id);
        if !kept_or_placed(recorded, track_row(conn, track_id)?.as_ref()) {
            return Ok(Err(FoldUndo::Stale));
        }
    }
    Ok(Ok((joined, survivor)))
}

/// Whether a moved file's match is still the one the join recorded, or the
/// placement of the album that kept it on the same release track.
fn kept_or_placed(recorded: Option<&PriorTrackIdentity>, now: Option<&PriorTrackIdentity>) -> bool {
    let same = |a: &Option<String>, b: &Option<String>| {
        a.as_deref().map(|a| a.trim().to_ascii_lowercase())
            == b.as_deref().map(|b| b.trim().to_ascii_lowercase())
    };
    match (recorded, now) {
        (None, None) => true,
        (Some(recorded), Some(now)) => {
            let kept = same(&recorded.recording_mbid, &now.recording_mbid)
                && same(&recorded.release_mbid, &now.release_mbid)
                && same(&recorded.release_track_mbid, &now.release_track_mbid)
                && recorded.medium_position == now.medium_position
                && recorded.release_track_position == now.release_track_position
                && recorded.decision_source == now.decision_source;
            let placed = recorded.release_track_mbid.is_some()
                && same(&recorded.release_track_mbid, &now.release_track_mbid)
                && now.decision_source == Some(DecisionSource::Manual);
            kept || placed
        }
        _ => false,
    }
}
