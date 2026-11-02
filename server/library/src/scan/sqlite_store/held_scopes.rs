//! Library folders the mass-missing guard holds back, and the new files in
//! them that automatic management leaves alone meanwhile.
//!
//! A scan that finds most of a folder's files gone at once marks none of
//! them missing (an unmounted share looks like that). If the files were a
//! person's retag and rename instead, the old rows stay indexed, so no
//! check can see that the new album replaced the old one, and automatic
//! management would write the edition over the retag. So the folder is
//! held: each run end records the folders it held back
//! (`library_scan_held_scopes`, kept past the run history), and an album
//! with a scanned file first indexed in a held folder since the hold began
//! is not written by itself. Rows that were there before the hold are not
//! held: a retag that keeps them keeps their records too, which the
//! takeover checks see.
//!
//! Each run end first judges the held albums ([`mark_replacements`]): one
//! that holds the same edition's tracks as an album whose files went since
//! the hold began is left unmanaged at once where the retag join would
//! leave it so, without waiting for the folder. A folder whose files were
//! deleted for good may never settle, and the old album may be removed by
//! hand or outlast the scan history before it does.
//!
//! A hold lifts at the end of a completed run that walked a folder
//! covering it to the end without holding back a folder covering it or
//! inside it, unless a file under it went missing in that run from an
//! album that holds no indexed file and is not merged into another: the
//! retag join may still take such an album over after its one-scan wait,
//! so the hold stays one more clean run. Nothing is lifted when the judging
//! failed. Albums parked on the hold are then looked at again.
//!
//! A held album is never held for its match. When a hold lifts, the held
//! rows of every album it lets go that is not left unmanaged and has no
//! record are remembered (`library_scan_unjudged_tracks`, with the hold's
//! start): a match that is a best guess, missing or waiting for review
//! could not be judged, and one that was judged is judged again on the
//! edition that is written. Automatic management judges the album the same
//! way before it first writes it ([`judge_remembered`]).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::corrections::continuation::accepted_release;
use crate::corrections::retag_merge::album_has_record;
use crate::identify::sqlite::{match_chosen, release_chosen};
use crate::management::automatic;
use crate::management::takeover::{self, Cause};
use crate::scan::sql::HeldRows;
use droppedneedle_kernel::db::savepoint;

/// Judge the held albums and remember the ones the lift lets go, lift the
/// holds run `run_id` settled, record the folders it held back, then wake
/// the albums parked on a lifted hold. Part of the run-end transaction.
/// When the judging fails, nothing is remembered, lifted or woken at this
/// run end.
pub(super) fn settle(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let judged = mark_replacements(tx, run_id, now)?;
    let lifted = if judged {
        tx.execute(
            crate::scan::sql::LIFT_SETTLED_HOLDS.text(),
            params![run_id, crate::clock::ms(now)],
        )?
    } else {
        0
    };
    tx.execute(
        crate::scan::sql::SETTLE.text(),
        params![run_id, crate::clock::ms(now)],
    )?;
    if lifted > 0 {
        automatic::wake(tx, Some(automatic::SCAN_HELD_BACK), now)?;
        tracing::info!(
            run_id,
            folders = lifted,
            "scan found held-back folders as expected; their new files may be managed again"
        );
    }
    Ok(())
}

/// Leave a held album unmanaged where it replaced an album the retag join
/// would leave it unmanaged after, then remember the held rows the lift
/// lets go ([`remember`]), in a savepoint of the caller's transaction.
/// Returns whether it judged: a store error rolls the savepoint back with
/// one warning, and the next run end judges again.
///
/// An album B with held rows, live, with no takeover mark and no record of
/// any of its files, replaces an album A when:
///
/// 1. both are matched to the same accepted exact release;
/// 2. a held row of B and a row of A that is neither an alias nor excluded
///    name the same MusicBrainz release track;
/// 3. that row of A went since the hold began: marked missing since then,
///    by a scan or a person, or still indexed and walked past by this run.
///
/// B is then marked as [`step_back`] says. `files_changed` wins when B
/// replaced several albums. Nothing is joined, marked missing or written,
/// and no hold is lifted here.
fn mark_replacements(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<bool> {
    match savepoint(tx, "mark_replacements", || {
        judge(tx, run_id, now).and_then(|()| remember(tx, run_id, now))
    })? {
        Ok(()) => Ok(true),
        Err(error) => {
            tracing::warn!(
                %error,
                run_id,
                "new albums in held folders were not judged; the folders stay held"
            );
            Ok(false)
        }
    }
}

/// Each new album with the old albums it may have replaced, and the held
/// rows paired with each old album.
type Found = BTreeMap<String, BTreeMap<String, Vec<String>>>;

fn replacements(tx: &Transaction<'_>, source: HeldRows, key: &str) -> rusqlite::Result<Found> {
    let mut found = Found::new();
    let mut statement = tx.prepare_cached(&crate::scan::sql::replacements_sql(source))?;
    let rows = statement.query_map(params![key], |row| {
        let row = crate::scan::sql::ReplacementRow::read(row)?;
        Ok((row.new_album_id, row.old_album_id, row.track_id))
    })?;
    for row in rows {
        let (new, old, track) = row?;
        found
            .entry(new)
            .or_default()
            .entry(old)
            .or_default()
            .push(track);
    }
    Ok(found)
}

fn judge(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    for (album, olds) in &replacements(tx, HeldRows::UnderHolds, run_id)? {
        if takeover::is_marked(tx, album)? || album_has_record(tx, album)? {
            continue;
        }
        let Some(release) = accepted_release(tx, album)? else {
            continue;
        };
        if let Some((cause, replaced)) = mark_replaced(tx, album, &release, olds, now)? {
            tracing::info!(
                run_id,
                album_id = album.as_str(),
                replaced = ?replaced,
                cause = cause.as_str(),
                "new album in a held folder left unmanaged as a retag of the album it replaced"
            );
        }
    }
    Ok(())
}

/// Mark `album`, matched to `release`, taken over where it replaced one of
/// `olds` (each old album with the held rows paired with it): the old album
/// is matched to the same accepted release and [`step_back`] gives a cause.
/// Returns the cause and the albums it replaced when it marked.
fn mark_replaced(
    tx: &Transaction<'_>,
    album: &str,
    release: &str,
    olds: &BTreeMap<String, Vec<String>>,
    now: f64,
) -> rusqlite::Result<Option<(Cause, Vec<String>)>> {
    let mut cause = None;
    let mut replaced = Vec::new();
    let mut tracks = BTreeSet::new();
    for (old, paired) in olds {
        if accepted_release(tx, old)?.as_deref() != Some(release) {
            continue;
        }
        let Some(found) = step_back(tx, old, album)? else {
            continue;
        };
        cause = Some(match cause {
            Some(Cause::FilesChanged) => Cause::FilesChanged,
            _ => found,
        });
        replaced.push(old.clone());
        tracks.extend(paired.iter().cloned());
    }
    let Some(cause) = cause else {
        return Ok(None);
    };
    let tracks: Vec<String> = tracks.into_iter().collect();
    Ok(takeover::mark(tx, album, cause, &tracks, None, now)?.then_some((cause, replaced)))
}

/// Why the app would step back from `new` after `old` was joined into it,
/// if it would: `old` was taken over and not handed back, or the app has
/// a record of its files; or a person chose `old`'s release or left it
/// unmanaged, and not `new`'s match (the join carries "leave unmanaged"
/// unless the match on `new` is a person's). Otherwise the app never wrote
/// `old` and nobody chose its edition, so `new` may be written.
fn step_back(conn: &Connection, old: &str, new: &str) -> rusqlite::Result<Option<Cause>> {
    let taken_over: bool = conn
        .prepare_cached(crate::scan::sql::ALBUM_IS_TAKEN_OVER.text())?
        .query_row(params![old], |row| row.get(0))?;
    if taken_over || album_has_record(conn, old)? {
        return Ok(Some(Cause::FilesChanged));
    }
    let left_unmanaged: bool = conn
        .prepare_cached(crate::scan::sql::ALBUM_IS_EXCLUDED.text())?
        .query_row(params![old], |row| row.get(0))?;
    if (release_chosen(conn, old)? || left_unmanaged) && !match_chosen(conn, new)? {
        return Ok(Some(Cause::RetagFollowed));
    }
    Ok(None)
}

/// Remember the held rows the holds run `?1` lifts let go, of every live
/// album with no takeover mark and no record of any of its files, so the
/// album is judged before it is first written. A track under several holds,
/// or remembered already, keeps the earliest start.
fn remember(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let mut albums: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    {
        let mut statement = tx.prepare_cached(crate::scan::sql::ROWS_LET_GO.text())?;
        let rows = statement.query_map(params![run_id, crate::clock::ms(now)], |row| {
            let row = crate::scan::sql::LetGoRow::read(row)?;
            Ok((row.local_album_id, row.id, row.first_held_at))
        })?;
        for row in rows {
            let (album, track, held_at) = row?;
            albums.entry(album).or_default().push((track, held_at));
        }
    }
    let mut insert = tx.prepare_cached(crate::scan::sql::REMEMBER.text())?;
    for (album, tracks) in &albums {
        if takeover::is_marked(tx, album)? || album_has_record(tx, album)? {
            continue;
        }
        for (track, held_at) in tracks {
            insert.execute(params![track, held_at])?;
        }
    }
    Ok(())
}

/// Whether a lifted hold let some of the album's files go before the album
/// was judged: automatic management judges it before it first writes it.
pub(crate) fn remembered(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(crate::scan::sql::REMEMBERED.text())?
        .query_row(params![album_id], |row| row.get(0))
}

/// Judge an album a lifted hold let go before automatic management first
/// writes it, as a run end judges a held album ([`mark_replacements`]),
/// then forget its remembered rows whatever is decided. Returns whether it
/// was marked taken over.
///
/// An album with a takeover mark, or a record of any of its files, is left
/// as it is. Otherwise its release is the one its match names, the edition
/// about to be written (a best guess too, when unconfirmed matches are
/// managed); a custom edition names none and is never marked. Its held rows
/// are its remembered rows still indexed on it, and an old album's row went
/// when it was marked missing since the hold began.
pub(crate) fn judge_remembered(
    tx: &Transaction<'_>,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let open = !takeover::is_marked(tx, album_id)? && !album_has_record(tx, album_id)?;
    let marked = match edition_release(tx, album_id)? {
        Some(release) if open => {
            match replacements(tx, HeldRows::Remembered, album_id)?.get(album_id) {
                Some(olds) => mark_replaced(tx, album_id, &release, olds, now)?,
                None => None,
            }
        }
        _ => None,
    };
    if let Some((cause, replaced)) = &marked {
        tracing::info!(
            album_id,
            replaced = ?replaced,
            cause = cause.as_str(),
            "new album from a held folder left unmanaged as a retag of the album it replaced"
        );
    }
    tx.prepare_cached(crate::scan::sql::JUDGE_REMEMBERED.text())?
        .execute(params![album_id])?;
    Ok(marked.is_some())
}

/// The release the album's match names, lowercased: the edition automatic
/// management writes.
fn edition_release(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(crate::scan::sql::EDITION_RELEASE.text())?
        .query_row(params![album_id], |row| row.get(0))
        .optional()
}

/// Whether automatic management leaves the album alone because the scan
/// holds a folder back: one of its scanned files sits in a held folder
/// and was first indexed after the hold began.
pub(crate) fn holds_new_files(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(crate::scan::sql::ALBUM_HOLDS_NEW_FILES.text())?
        .query_row(params![album_id], |row| row.get(0))
}
