//! Joining copies of one edition that sit apart as two albums.
//!
//! A scan files copies of one album in different folders as different
//! albums. When two of them are on the same release and share no track,
//! they are one album split in two (tracks added later, landed under
//! another folder), and the end of a scan joins the later copy C into the
//! survivor S, the copy the library shows first (a person's choice, then
//! the oldest). All of this holds for both:
//!
//! 1. live, with an indexed file;
//! 2. a match on the same release, accepted (no match flag, no custom
//!    edition), automatic or a person's, and not two different choices;
//! 3. no track in both: every indexed file of either is placed on a track
//!    of the release, and no indexed file of either shares a release track
//!    or a recording with one of the other (files in the recycle bin are not
//!    counted);
//! 4. neither grouped by a curator;
//! 5. not kept apart by a person ("Keep them apart" on a retag pair of the
//!    two) and neither brought back out of a join by an undo;
//! 6. both hands-off (left unmanaged, taken over, or stamped after a retag
//!    by hand), or neither;
//! 7. no live work, no retag waiting to be read to its end, and no open
//!    retag pair on either.
//!
//! A pair that fails waits, with nothing recorded, and is tried again at
//! the next run end. The join is the retag merge's
//! ([`merge_retagged`], [`JoinCause::SameEditionCopies`]): catalog rows only,
//! with an undo in the album's history. The copy's files are locked to the
//! survivor, so later scans keep them there. No file is read or written; the
//! survivor is queued for automatic management, which moves files where
//! the folder's management moves them.

use rusqlite::{Connection, Transaction, params};

use super::retag_merge::{JoinCause, MergeOutcome, live_work, merge_retagged};
use crate::scan::sqlite_store::awaits_retag;
use droppedneedle_kernel::db::savepoint;

/// Join every pair of copies the rule above allows, in a savepoint of the
/// caller's transaction. A store error rolls the savepoint back with one
/// warning and leaves the caller's work to commit; the next run end tries
/// again.
pub(crate) fn join_copies(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let mut at: Option<(String, String)> = None;
    match savepoint(tx, "join_copies", || pass(tx, now, &mut at))? {
        Ok(()) => Ok(()),
        Err(error) => {
            let (copy, survivor) = at.unwrap_or_default();
            tracing::warn!(
                %error,
                album_id = copy,
                target = survivor,
                "copies of one edition not joined; the next run end tries again"
            );
            Ok(())
        }
    }
}

fn pass(tx: &Transaction<'_>, now: f64, at: &mut Option<(String, String)>) -> rusqlite::Result<()> {
    for (release, copies) in crate::identity::split_copies(tx)? {
        let Some((survivor, others)) = copies.split_first() else {
            continue;
        };
        for copy in others {
            *at = Some((copy.clone(), survivor.clone()));
            if !joinable(tx, survivor, copy, &release, now)? {
                continue;
            }
            match merge_retagged(tx, copy, survivor, None, now, JoinCause::SameEditionCopies)? {
                MergeOutcome::Merged { .. } => {
                    crate::management::automatic::queue(tx, survivor, now)?;
                    tracing::info!(
                        album_id = %copy,
                        target = %survivor,
                        "copy of one edition joined into the album that kept the edition"
                    );
                }
                MergeOutcome::Refused { reason, error } => {
                    tracing::info!(
                        album_id = %copy,
                        target = %survivor,
                        ?reason,
                        error = error.map(|error| error.to_string()),
                        "copy of one edition not joined; the next run end tries again"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Whether `copy` may join `survivor` on `release` now: rules 3 to 7.
/// Rules 1 and 2 are the read that paired them.
fn joinable(
    conn: &Connection,
    survivor: &str,
    copy: &str,
    release: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    if crate::identity::copies_overlap(conn, survivor, copy, release)? {
        return Ok(false);
    }
    let kept_apart: bool = conn
        .prepare_cached(crate::corrections::sql::PAIR_KEPT_APART.text())?
        .query_row(params![survivor, copy], |row| row.get(0))?;
    if kept_apart {
        return Ok(false);
    }
    let asks = |sql: &str, album: &str| -> rusqlite::Result<bool> {
        conn.prepare_cached(sql)?
            .query_row(params![album], |row| row.get(0))
    };
    for album in [survivor, copy] {
        if asks(crate::corrections::sql::GROUPED_BY_HAND.text(), album)?
            || crate::identity::fold_undone(conn, album)?
            || live_work(conn, album, now)?
            || awaits_retag(conn, album)?
            || asks(crate::corrections::sql::IN_OPEN_RETAG_PAIR.text(), album)?
        {
            return Ok(false);
        }
    }
    let hands_off = crate::corrections::sql::ALBUM_HANDS_OFF.text();
    Ok(asks(hands_off, survivor)? == asks(hands_off, copy)?)
}
