//! Retag pairs a person decides on: two album rows a scan would not join
//! by itself, because each holds an edition a person chose, the old row
//! holds a custom edition, the album was split against a rename, or new
//! files appeared beside a hands-off album in the scan that saw its files
//! go (nothing proves those are one album; the new album stays held).
//!
//! A join drops what stands in the way first, all in the caller's
//! transaction, so a refusal leaves both albums as they were:
//! - a custom edition on the old album (it lists that row's own tracks);
//! - then, when two different chosen editions remain, the one the person
//!   did not keep.
//!
//! The old album is then merged into the new one ([`merge_retagged`]),
//! and each dropped edition leaves a pending review on the album that
//! stays, with the edition as its candidate. Approving the review of a
//! chosen edition puts it back; approving that of a custom edition seals
//! only its release group, since nothing re-activates a custom edition.
//!
//! A pair whose new album was merged into another since stands for that
//! album: a retired id points one step at the live album it became.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::edition::{self, Identity, album_title, drop_edition, open_review};
use super::membership::load_album;
use super::models::{
    CorrectionError, DroppedEdition, PairEdition, PairJoinRequest, PairReason, PairSide, RetagPair,
    RetagPairList,
};
use super::reasons;
use super::retag_merge::{JoinCause, MergeOutcome, merge_retagged};
use crate::identify::sqlite::chosen_matches_differ;
use crate::identity::DecisionSource;
use crate::operations::reasons::Reason;
use crate::scan::sqlite_store::retag_pairs::{RetagPairReason, RetagPairState};

/// The reasons a person decides on. The app settles every other one by
/// itself.
const SHOWN: [(RetagPairReason, PairReason); 4] = [
    (
        RetagPairReason::ChosenMatchesDiffer,
        PairReason::ChosenMatchesDiffer,
    ),
    (
        RetagPairReason::TracksElsewhere,
        PairReason::TracksElsewhere,
    ),
    (RetagPairReason::CustomEdition, PairReason::CustomEdition),
    (
        RetagPairReason::ReplacedHandsOff,
        PairReason::ReplacedHandsOff,
    ),
];

/// At most this many pairs are listed.
const LIMIT: i64 = 100;

/// Open pairs with a reason a person decides on, both albums live, the
/// new album standing for the album it was merged into since. Pairs
/// already joined that way are left out.
const OPEN_PAIRS: &str = "FROM library_retag_pairs p \
     JOIN local_albums o ON o.id = p.old_album_id \
     JOIN local_albums n ON n.id = p.new_album_id \
     LEFT JOIN local_albums t ON t.id = n.retired_into_album_id \
     WHERE p.state = ?1 AND p.reason IN (?2, ?3, ?4, ?5) \
     AND o.retired_into_album_id IS NULL \
     AND (n.retired_into_album_id IS NULL OR t.retired_into_album_id IS NULL) \
     AND COALESCE(n.retired_into_album_id, n.id) <> p.old_album_id";

/// The open pairs, newest first, with how many there are.
pub(super) fn list(conn: &Connection) -> Result<RetagPairList, CorrectionError> {
    let values = params![
        RetagPairState::Open.as_str(),
        SHOWN[0].0.as_str(),
        SHOWN[1].0.as_str(),
        SHOWN[2].0.as_str(),
        SHOWN[3].0.as_str(),
    ];
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) {OPEN_PAIRS}"), values, |row| {
        row.get(0)
    })?;
    let rows: Vec<(String, String, String, i64)> = conn
        .prepare(&format!(
            "SELECT p.old_album_id, COALESCE(n.retired_into_album_id, n.id), p.reason, \
             p.recorded_at {OPEN_PAIRS} \
             ORDER BY p.recorded_at DESC, p.old_album_id, p.new_album_id LIMIT {LIMIT}"
        ))?
        .query_map(values, |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut pairs = Vec::with_capacity(rows.len());
    for (old, new, reason, recorded_at) in rows {
        let Some((_, reason)) = SHOWN
            .into_iter()
            .find(|(stored, _)| stored.as_str() == reason)
        else {
            continue;
        };
        let old_album = side(conn, &old)?;
        let custom = old_album.edition.as_ref().is_some_and(|found| found.custom);
        pairs.push(RetagPair {
            editions_differ: !custom && chosen_matches_differ(conn, &old, &new)?,
            old_album,
            new_album: side(conn, &new)?,
            reason,
            recorded_at,
        });
    }
    Ok(RetagPairList { pairs, total })
}

fn side(conn: &Connection, album_id: &str) -> Result<PairSide, CorrectionError> {
    let (title, album_artist_name, track_count) = conn.query_row(
        "SELECT title, COALESCE(album_artist_name, ''), (SELECT COUNT(*) FROM local_tracks t \
         WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         FROM local_albums a WHERE a.id = ?1",
        params![album_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let identity = edition::identity(conn, album_id)?;
    Ok(PairSide {
        id: album_id.to_owned(),
        title,
        album_artist_name,
        track_count,
        identity_revision: identity.as_ref().map_or(0, |found| found.row_revision),
        edition: identity.map(|found| PairEdition {
            chosen: DecisionSource::parse(&found.decision_source)
                .is_some_and(DecisionSource::is_persons_choice),
            custom: found.custom,
            release_group_mbid: found.release_group_mbid,
            release_mbid: found.release_mbid,
        }),
    })
}

/// Join the pair; returns the album that stays. Any refusal is returned
/// before the caller commits, so nothing of it is kept.
pub(super) fn join(
    tx: &Transaction<'_>,
    request: &PairJoinRequest,
    now: f64,
) -> Result<String, CorrectionError> {
    let old = request.old_album_id.as_str();
    let Some((stored_new, state)) = find(tx, old, &request.new_album_id)? else {
        // A retry after the join went through.
        let target = successor(tx, &request.new_album_id)?;
        let joined = retired_into(tx, old)?.filter(|into| Some(into) == target.as_ref());
        return joined.ok_or(CorrectionError::NotFound(reasons::PAIR_NOT_FOUND));
    };
    match state {
        RetagPairState::Open => {}
        RetagPairState::Dismissed => {
            return Err(CorrectionError::Conflict(reasons::PAIR_DISMISSED));
        }
        RetagPairState::Joined => return Err(CorrectionError::Conflict(reasons::PAIR_ALBUM_GONE)),
    }
    let new = successor(tx, &stored_new)?.unwrap_or_else(|| stored_new.clone());
    if new == old {
        // Joined already by the album tools: the hidden pair goes.
        tx.execute(
            "DELETE FROM library_retag_pairs WHERE old_album_id = ?1 AND new_album_id = ?2",
            params![old, stored_new],
        )?;
        return Ok(new);
    }
    // A card shown before the new album was merged away names the album
    // that is gone.
    let shown_before_merge =
        new != stored_new && !request.expected_identity_revisions.contains_key(&new);
    if shown_before_merge || load_album(tx, old)?.is_none() || load_album(tx, &new)?.is_none() {
        return Err(CorrectionError::Conflict(reasons::PAIR_ALBUM_GONE));
    }
    for album in [old, new.as_str()] {
        let expected = request
            .expected_identity_revisions
            .get(album)
            .ok_or(CorrectionError::Invalid(reasons::PAIR_REVISIONS_REQUIRED))?;
        let current = edition::identity(tx, album)?.map_or(0, |found| found.row_revision);
        if *expected != current {
            return Err(CorrectionError::Conflict(reasons::PAIR_EDITION_CHANGED));
        }
    }
    if let Some(keep) = request.keep_edition_of.as_deref()
        && keep != old
        && keep != new
    {
        return Err(CorrectionError::Invalid(reasons::KEEP_EDITION_REQUIRED));
    }

    let mut dropped = Vec::new();
    if let Some(custom) = edition::identity(tx, old)?.filter(|found| found.custom) {
        dropped.push(drop_for_join(tx, custom, reasons::EDITION_CUSTOM_CLEARED)?);
    }
    match (
        chosen_matches_differ(tx, old, &new)?,
        request.keep_edition_of.as_deref(),
    ) {
        (true, Some(keep)) => {
            let other = if keep == old { new.as_str() } else { old };
            if let Some(found) = edition::identity(tx, other)? {
                // Approving a custom edition's review restores only its
                // release group, so it says so.
                let reason = if found.custom {
                    reasons::EDITION_CUSTOM_CLEARED
                } else {
                    reasons::EDITION_CONFLICT_KEPT
                };
                dropped.push(drop_for_join(tx, found, reason)?);
            }
        }
        (true, None) => return Err(CorrectionError::Invalid(reasons::KEEP_EDITION_REQUIRED)),
        (false, Some(_)) => return Err(CorrectionError::Invalid(reasons::KEEP_EDITION_NOT_NEEDED)),
        (false, None) => {}
    }

    match merge_retagged(
        tx,
        old,
        &new,
        Some(&request.actor),
        now,
        JoinCause::AcrossScans,
    )? {
        MergeOutcome::Merged { .. } => {}
        MergeOutcome::Refused {
            reason: RetagPairReason::LiveWork,
            ..
        } => return Err(CorrectionError::Conflict(reasons::PAIR_LIVE_WORK)),
        MergeOutcome::Refused { reason, error } => {
            return Err(CorrectionError::Store(format!(
                "retag join refused ({}): {error:?}",
                reason.as_str()
            )));
        }
    }
    for (reason, title, edition) in dropped {
        open_review(tx, &new, reason, &title, &[edition], now)?;
    }
    Ok(new)
}

/// Drop an album's edition ahead of a join. Its track identities and any
/// custom edition manifest stay.
fn drop_for_join(
    tx: &Transaction<'_>,
    found: Identity,
    reason: Reason,
) -> rusqlite::Result<(Reason, String, DroppedEdition)> {
    drop_edition(tx, &found.album_id)?;
    let title = album_title(tx, &found.album_id)?;
    Ok((
        reason,
        title,
        DroppedEdition {
            album_id: found.album_id,
            release_group_mbid: found.release_group_mbid,
            release_mbid: found.release_mbid,
            decision_source: found.decision_source,
        },
    ))
}

/// Mark the pair dismissed. False when it was dismissed already.
pub(super) fn dismiss(
    tx: &Transaction<'_>,
    old: &str,
    new: &str,
    now_ms: i64,
) -> Result<bool, CorrectionError> {
    let Some((stored_new, state)) = find(tx, old, new)? else {
        return Err(CorrectionError::NotFound(reasons::PAIR_NOT_FOUND));
    };
    if state == RetagPairState::Dismissed {
        return Ok(false);
    }
    tx.execute(
        "UPDATE library_retag_pairs SET state = ?3, updated_at = ?4 \
         WHERE old_album_id = ?1 AND new_album_id = ?2",
        params![old, stored_new, RetagPairState::Dismissed.as_str(), now_ms],
    )?;
    Ok(true)
}

/// The recorded pair of `old` and `new`, where `new` is the pair's new
/// album or the album it was merged into since; its stored new album and
/// state. An exact match wins, then an open pair.
fn find(
    conn: &Connection,
    old: &str,
    new: &str,
) -> Result<Option<(String, RetagPairState)>, CorrectionError> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT p.new_album_id, p.state FROM library_retag_pairs p \
             JOIN local_albums n ON n.id = p.new_album_id \
             WHERE p.old_album_id = ?1 AND (p.new_album_id = ?2 OR n.retired_into_album_id = ?2) \
             ORDER BY p.new_album_id = ?2 DESC, p.state = ?3 DESC LIMIT 1",
            params![old, new, RetagPairState::Open.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    found
        .map(|(stored, state)| {
            RetagPairState::parse(&state)
                .map(|state| (stored, state))
                .ok_or_else(|| CorrectionError::Store(format!("unknown retag pair state {state}")))
        })
        .transpose()
}

fn retired_into(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT retired_into_album_id FROM local_albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// The live album `album_id` now stands for: itself, or the album it was
/// merged into. `None` for an unknown id.
fn successor(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT COALESCE(retired_into_album_id, id) FROM local_albums WHERE id = ?1",
        params![album_id],
        |row| row.get(0),
    )
    .optional()
}
