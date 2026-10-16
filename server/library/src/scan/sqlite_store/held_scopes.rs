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
//! covering it to the end without holding that folder back, unless a
//! file under it went missing in that run from an album that holds no
//! indexed file and is not merged into another: the retag join may still
//! take such an album over after its one-scan wait, so the hold stays one
//! more clean run. It also stays while an album with a held file waits for
//! identification, so that album is judged before it can be written, and
//! when the judging failed. Albums parked on the hold are then looked at
//! again.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, Transaction, params};

use super::commit::WALKED_PAST;
use crate::corrections::continuation::accepted_release;
use crate::corrections::retag_merge::album_has_record;
use crate::identify::sqlite::{match_chosen, release_chosen};
use crate::management::automatic;
use crate::management::takeover::{self, Cause};

/// Whether the folder at the SQL expression `ancestor` covers the path at
/// `descendant`: `.` covers everything, otherwise the path itself or
/// anything below it ([`scope_covers_path`](crate::scan::models::scope_covers_path)).
fn covers(ancestor: &str, descendant: &str) -> String {
    format!(
        "({ancestor} = '.' OR {descendant} = {ancestor} \
         OR substr({descendant}, 1, length({ancestor}) + 1) = {ancestor} || '/')"
    )
}

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

/// Judge the held albums, lift the holds run `run_id` settled, record the
/// folders it held back, then wake the albums parked on a lifted hold.
/// Part of the run-end transaction. When the judging fails, nothing is
/// lifted or woken at this run end.
pub(super) fn settle(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let judged = mark_replacements(tx, run_id, now)?;
    let lifted = if judged {
        tx.execute(
            &format!(
                "DELETE FROM library_scan_held_scopes AS h \
                 WHERE EXISTS (SELECT 1 FROM library_scan_runs r \
                   JOIN library_scan_run_scopes s ON s.run_id = r.id \
                   WHERE r.id = ?1 AND r.state = 'completed' AND s.root_id = h.root_id \
                   AND s.discovery_state = 'completed' AND {scope_covers_held} \
                   AND NOT EXISTS (SELECT 1 FROM library_scan_failures f WHERE f.run_id = r.id \
                     AND f.root_id = s.root_id AND f.relative_path = s.relative_path \
                     AND f.failure_code = 'MASS_MISSING_GUARD')) \
                 AND NOT EXISTS (SELECT 1 FROM local_tracks t \
                   JOIN local_albums a ON a.id = t.local_album_id \
                   WHERE t.availability = 'missing' AND t.root_id = h.root_id \
                   AND t.missing_since >= (SELECT COALESCE(started_at, ?2) \
                     FROM library_scan_runs WHERE id = ?1) \
                   AND {held_covers_track} AND a.retired_into_album_id IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM local_tracks k \
                     WHERE k.local_album_id = a.id AND k.availability = 'indexed')) \
                 AND NOT EXISTS (SELECT 1 FROM local_tracks t \
                   JOIN library_identify_jobs j ON j.local_album_id = t.local_album_id \
                   WHERE {held_track} AND j.state IN ('queued', 'running', 'deferred'))",
                scope_covers_held = covers("s.relative_path", "h.relative_path"),
                held_covers_track = covers("h.relative_path", "t.relative_path"),
                held_track = held("t", "h"),
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
/// would leave it unmanaged after, in a savepoint of the caller's
/// transaction. Returns whether it judged: a store error rolls the
/// savepoint back with one warning, and the next run end judges again.
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
/// B is marked `files_changed` when A is taken over and not handed back
/// or the app has a record of A's files, and `retag_followed` when A's
/// release was chosen and B's match was not. Otherwise nothing is done:
/// the app never wrote A and nobody chose its edition, so B's first
/// automatic run may write it once the folder settles. `files_changed`
/// wins when B replaced several albums. Nothing is joined, marked missing
/// or written, and no hold is lifted here.
fn mark_replacements(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<bool> {
    tx.execute_batch("SAVEPOINT mark_replacements")?;
    match judge(tx, run_id, now) {
        Ok(()) => {
            tx.execute_batch("RELEASE mark_replacements")?;
            Ok(true)
        }
        Err(error) => {
            tx.execute_batch("ROLLBACK TO mark_replacements; RELEASE mark_replacements")?;
            tracing::warn!(
                %error,
                run_id,
                "new albums in held folders were not judged; the folders stay held"
            );
            Ok(false)
        }
    }
}

/// Held rows and the rows of other live albums on the same release track
/// that went since the hold began, as (new album, old album, held row).
/// Starts from the holds, so no hold means no work, and reaches the old
/// album through its release. The old album's rows are read through its
/// index by name: without planner statistics SQLite would build a
/// temporary index over every track instead. `?1` is the run.
fn replacements_sql() -> String {
    format!(
        "SELECT DISTINCT n.local_album_id, a.id, n.id \
         FROM library_scan_held_scopes h \
         CROSS JOIN local_tracks n \
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
         AND ((t.availability = 'missing' AND t.missing_since * 1000 >= h.held_at) \
           OR (t.availability = 'indexed' AND {WALKED_PAST})) \
         ORDER BY 1, 2, 3",
        held_row = held("n", "h"),
    )
}

fn judge(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let mut found: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    {
        let mut statement = tx.prepare_cached(&replacements_sql())?;
        let rows = statement.query_map(params![run_id], |row| {
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
    }
    for (album, olds) in &found {
        if takeover::is_marked(tx, album)? || album_has_record(tx, album)? {
            continue;
        }
        let Some(release) = accepted_release(tx, album)? else {
            continue;
        };
        let mut cause = None;
        let mut replaced = Vec::new();
        let mut tracks = BTreeSet::new();
        for (old, paired) in olds {
            if accepted_release(tx, old)?.as_deref() != Some(release.as_str()) {
                continue;
            }
            let Some(found) = step_back(tx, old, album)? else {
                continue;
            };
            cause = Some(match cause {
                Some(Cause::FilesChanged) => Cause::FilesChanged,
                _ => found,
            });
            replaced.push(old.as_str());
            tracks.extend(paired.iter().cloned());
        }
        let Some(cause) = cause else {
            continue;
        };
        let tracks: Vec<String> = tracks.into_iter().collect();
        if takeover::mark(tx, album, cause, &tracks, None, now)? {
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

/// Why the app would step back from `new` after `old` was joined into it,
/// if it would: `old` was taken over and not handed back, or the app has
/// a record of its files; or a person chose `old`'s release and not
/// `new`'s match.
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
    if release_chosen(conn, old)? && !match_chosen(conn, new)? {
        return Ok(Some(Cause::RetagFollowed));
    }
    Ok(None)
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
