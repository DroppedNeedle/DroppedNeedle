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

use super::commit::WALKED_PAST;
use crate::corrections::continuation::accepted_release;
use crate::corrections::retag_merge::album_has_record;
use crate::identify::sqlite::{match_chosen, release_chosen};
use crate::management::automatic;
use crate::management::takeover::{self, Cause};
use droppedneedle_kernel::db::savepoint;

/// Whether the folder at the SQL expression `ancestor` covers the path at
/// `descendant`: `.` covers everything, otherwise the path itself or
/// anything below it ([`scope_covers_path`](crate::scan::models::scope_covers_path)).
pub(super) fn covers(ancestor: &str, descendant: &str) -> String {
    format!(
        "({ancestor} = '.' OR {descendant} = {ancestor} \
         OR substr({descendant}, 1, length({ancestor}) + 1) = {ancestor} || '/')"
    )
}

/// Whether run `$run` held back a folder covering the row aliased `$row`: a
/// mass-missing guard failure on its root at a folder that [`covers`] its
/// path, a scan scope or a mount found empty. Nothing under a held-back
/// folder counts as walked: it is not missing, gone or moved. The guard
/// index is named: SQLite would otherwise read every failure of the run on
/// the root through the primary key, since that one covers the query.
macro_rules! held_back {
    ($run:literal, $row:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM library_scan_failures g \
             INDEXED BY idx_scan_failures_guard WHERE g.run_id = ",
            $run,
            " AND g.root_id = ",
            $row,
            ".root_id AND g.failure_code = 'MASS_MISSING_GUARD' \
             AND (g.relative_path = '.' OR ",
            $row,
            ".relative_path = g.relative_path OR substr(",
            $row,
            ".relative_path, 1, length(g.relative_path) + 1) = g.relative_path || '/'))"
        )
    };
}
pub(super) use held_back;

/// Whether the track row aliased `track` is held by the held folder
/// aliased `hold`: indexed, found by a scan, in that folder of its root,
/// and first indexed at or after the hold began. The looser bound on
/// `imported_at` selects nothing more; it lets a search start from the
/// recently indexed tracks.
fn held(track: &str, hold: &str) -> String {
    format!(
        "{track}.availability = 'indexed' AND {track}.ingest_source = 'scan' \
         AND {track}.root_id = {hold}.root_id \
         AND {track}.imported_at >= ({hold}.held_at - 1) / 1000.0 \
         AND CAST({track}.imported_at * 1000 AS INTEGER) >= {hold}.held_at AND {}",
        covers(
            &format!("{hold}.relative_path"),
            &format!("{track}.relative_path")
        )
    )
}

/// Whether run `?1` lifts the hold aliased `hold`: the run completed and
/// walked a folder covering it to the end, held back no folder covering it
/// or inside it, and no file under it went missing in the run from an album
/// that holds no indexed file and is not merged into another. `?2` is now,
/// for a run with no start.
fn lifts(hold: &str) -> String {
    format!(
        concat!(
            "EXISTS (SELECT 1 FROM library_scan_runs r \
               JOIN library_scan_run_scopes s ON s.run_id = r.id \
               WHERE r.id = ?1 AND r.state = 'completed' AND s.root_id = {hold}.root_id \
               AND s.discovery_state = 'completed' AND {scope_covers_held}) \
             AND NOT ",
            held_back!("?1", "{hold}"),
            " AND NOT EXISTS (SELECT 1 FROM library_scan_failures g WHERE g.run_id = ?1 \
               AND g.root_id = {hold}.root_id AND g.failure_code = 'MASS_MISSING_GUARD' \
               AND {held_covers_failure}) \
             AND NOT EXISTS (SELECT 1 FROM local_tracks t \
               JOIN local_albums a ON a.id = t.local_album_id \
               WHERE t.availability = 'missing' AND t.root_id = {hold}.root_id \
               AND t.missing_since >= (SELECT COALESCE(started_at, ?2) \
                 FROM library_scan_runs WHERE id = ?1) \
               AND {held_covers_track} AND a.retired_into_album_id IS NULL \
               AND NOT EXISTS (SELECT 1 FROM local_tracks k \
                 WHERE k.local_album_id = a.id AND k.availability = 'indexed'))"
        ),
        hold = hold,
        scope_covers_held = covers("s.relative_path", &format!("{hold}.relative_path")),
        held_covers_failure = covers(&format!("{hold}.relative_path"), "g.relative_path"),
        held_covers_track = covers(&format!("{hold}.relative_path"), "t.relative_path"),
    )
}

/// Judge the held albums and remember the ones the lift lets go, lift the
/// holds run `run_id` settled, record the folders it held back, then wake
/// the albums parked on a lifted hold. Part of the run-end transaction.
/// When the judging fails, nothing is remembered, lifted or woken at this
/// run end.
pub(super) fn settle(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let judged = mark_replacements(tx, run_id, now)?;
    let lifted = if judged {
        tx.execute(
            &format!(
                "DELETE FROM library_scan_held_scopes AS h WHERE {}",
                lifts("h")
            ),
            params![run_id, now],
        )?
    } else {
        0
    };
    tx.execute(
        "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
         SELECT DISTINCT f.root_id, f.relative_path, \
           CAST(COALESCE(r.started_at, ?2) * 1000 AS INTEGER) \
         FROM library_scan_failures f JOIN library_scan_runs r ON r.id = f.run_id \
         WHERE f.run_id = ?1 AND f.failure_code = 'MASS_MISSING_GUARD' \
         ON CONFLICT (root_id, relative_path) DO NOTHING",
        params![run_id, now],
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

/// Where a judgement takes the new album's held rows from.
#[derive(Clone, Copy)]
enum HeldRows {
    /// The rows under a hold, at the end of run `?1`.
    UnderHolds,
    /// The remembered rows of album `?1` still indexed on it, at a look.
    Remembered,
}

/// Held rows and the rows of other live albums on the same release track
/// that went since the hold began, as (new album, old album, held row).
/// Starts from the holds or the album's tracks, so no hold means no work,
/// and reaches the old album through its release. The old album's rows are
/// read through its index by name: without planner statistics SQLite would
/// build a temporary index over every track instead. Only a run end counts
/// an old row walked past as gone: no run walks anything at a look.
fn replacements_sql(source: HeldRows) -> String {
    let (from, held_row, walked_past) = match source {
        HeldRows::UnderHolds => (
            "library_scan_held_scopes h CROSS JOIN local_tracks n",
            held("n", "h"),
            format!(" OR (t.availability = 'indexed' AND {WALKED_PAST})"),
        ),
        HeldRows::Remembered => (
            "local_tracks n CROSS JOIN library_scan_unjudged_tracks h \
             ON h.local_track_id = n.id",
            "n.local_album_id = ?1 AND n.availability = 'indexed'".to_owned(),
            String::new(),
        ),
    };
    format!(
        "SELECT DISTINCT n.local_album_id, a.id, n.id \
         FROM {from} \
         CROSS JOIN local_albums b ON b.id = n.local_album_id \
         CROSS JOIN local_track_external_identities ni \
           ON ni.local_track_id = n.id AND ni.provider = 'musicbrainz' \
         CROSS JOIN local_album_external_identities be \
           ON be.local_album_id = b.id AND be.provider = 'musicbrainz' \
         CROSS JOIN local_album_external_identities ae \
           ON lower(ae.release_mbid) = lower(be.release_mbid) AND ae.provider = 'musicbrainz' \
         CROSS JOIN local_albums a ON a.id = ae.local_album_id \
         CROSS JOIN local_tracks t INDEXED BY idx_local_tracks_album_availability \
           ON t.local_album_id = a.id AND t.availability IN ('missing', 'indexed') \
         CROSS JOIN local_track_external_identities oi \
           ON oi.local_track_id = t.id AND oi.provider = 'musicbrainz' \
         WHERE {held_row} AND b.retired_into_album_id IS NULL \
         AND trim(COALESCE(be.release_mbid, '')) <> '' \
         AND a.id <> b.id AND a.retired_into_album_id IS NULL \
         AND trim(COALESCE(ni.release_track_mbid, '')) <> '' \
         AND lower(trim(oi.release_track_mbid)) = lower(trim(ni.release_track_mbid)) \
         AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = t.id) \
         AND ((t.availability = 'missing' AND t.missing_since * 1000 >= h.held_at){walked_past}) \
         ORDER BY 1, 2, 3"
    )
}

/// Each new album with the old albums it may have replaced, and the held
/// rows paired with each old album.
type Found = BTreeMap<String, BTreeMap<String, Vec<String>>>;

fn replacements(tx: &Transaction<'_>, source: HeldRows, key: &str) -> rusqlite::Result<Found> {
    let mut found = Found::new();
    let mut statement = tx.prepare_cached(&replacements_sql(source))?;
    let rows = statement.query_map(params![key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
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
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM library_album_takeovers \
             WHERE local_album_id = ?1 AND handed_back_at IS NULL)",
        )?
        .query_row(params![old], |row| row.get(0))?;
    if taken_over || album_has_record(conn, old)? {
        return Ok(Some(Cause::FilesChanged));
    }
    let left_unmanaged: bool = conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM library_management_exclusions \
             WHERE local_album_id = ?1)",
        )?
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
        let mut statement = tx.prepare_cached(&format!(
            "SELECT n.local_album_id, n.id, MIN(h.held_at) \
             FROM library_scan_held_scopes h \
             CROSS JOIN local_tracks n \
             CROSS JOIN local_albums b ON b.id = n.local_album_id \
             WHERE {} AND {} AND b.retired_into_album_id IS NULL \
             GROUP BY n.id",
            lifts("h"),
            held("n", "h"),
        ))?;
        let rows = statement.query_map(params![run_id, now], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (album, track, held_at) = row?;
            albums.entry(album).or_default().push((track, held_at));
        }
    }
    let mut insert = tx.prepare_cached(
        "INSERT INTO library_scan_unjudged_tracks (local_track_id, held_at) VALUES (?1, ?2) \
         ON CONFLICT (local_track_id) DO UPDATE SET held_at = min(held_at, excluded.held_at)",
    )?;
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
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_unjudged_tracks u ON u.local_track_id = t.id \
         WHERE t.local_album_id = ?1)",
    )?
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
    tx.prepare_cached(
        "DELETE FROM library_scan_unjudged_tracks WHERE local_track_id IN \
         (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
    )?
    .execute(params![album_id])?;
    Ok(marked.is_some())
}

/// The release the album's match names, lowercased: the edition automatic
/// management writes.
fn edition_release(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT lower(trim(release_mbid)) FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = 'musicbrainz' \
         AND trim(COALESCE(release_mbid, '')) <> ''",
    )?
    .query_row(params![album_id], |row| row.get(0))
    .optional()
}

/// Whether automatic management leaves the album alone because the scan
/// holds a folder back: one of its scanned files sits in a held folder
/// and was first indexed after the hold began.
pub(crate) fn holds_new_files(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(&format!(
        "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_held_scopes h \
         WHERE t.local_album_id = ?1 AND {})",
        held("t", "h")
    ))?
    .query_row(params![album_id], |row| row.get(0))
}
