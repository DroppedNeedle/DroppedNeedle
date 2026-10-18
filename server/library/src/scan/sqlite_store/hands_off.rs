//! New files that take the place of a hands-off album are left alone.
//!
//! A person who left an album unmanaged, kept it as a custom edition or
//! took it over, and then retags and renames its files in one go (Picard
//! and beets do both), gets new track rows on a new album row: no track
//! moved and no MusicBrainz id ties the two. Nothing proves they are one
//! album, so nothing is joined; but the new album must not be written over
//! the retag either. At the end of every scan run R, an album B is held
//! from automatic writes ([`takeover::mark`] with
//! [`Cause::ReplacedHandsOff`]) when:
//!
//! 1. a live album A is hands-off: it has a "leave unmanaged" row, an
//!    active custom edition, or a takeover mark not handed back;
//! 2. A lost a file in R: a track row went missing since R began, or is
//!    still indexed and R walked past it, whether or not the mass-missing
//!    guard held its scope back;
//! 3. A's places in R are the folders holding its album folders (each
//!    lost row's grouping folder, read without a hidden `gone/` prefix),
//!    with their roots; the parent of a top-level folder is `.`;
//! 4. B has a track row new in R (scanned, first indexed since R began,
//!    in R's inventory) whose album folder sits at one of A's places: A's
//!    own folder or one next to it;
//! 5. B is live and is not A, holds only scanned rows first indexed since
//!    B was made, was made in R or is already held with this cause, has
//!    no "leave unmanaged" row and no match a person chose;
//! 6. at least half of B's rows, rounded up, pair one to one with A's
//!    lost rows by length (within the scan's duration tolerance; two
//!    unknown lengths fit). A retag never changes the audio; lengths only
//!    narrow the hold, they are no proof of identity.
//!
//! The pair (A, B) is recorded for a person ([`RetagPairReason::ReplacedHandsOff`])
//! when A holds no indexed row and A is the only album passing 1 to 3 at
//! B's places whose missing rows fit B's by 6. A B already held with this
//! cause that no pair names as the new album is read again at every run
//! end without 4, its places those of its own rows, so a retag two runs
//! see is paired once the old album is empty.
//!
//! Names, recordings, track counts and matches are not weighed. Nothing is
//! joined, no file is read or written, and no history moves.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::commit::{DURATION_TOLERANCE_SECONDS, WALKED_PAST};
use super::retag_pairs::{self, RetagPairReason};
use crate::identify::sqlite::match_chosen;
use crate::management::takeover::{self, Cause};
use crate::publish::HIDDEN_PREFIX;
use crate::scan::naming::grouping_directory;
use droppedneedle_kernel::db::savepoint;

/// Albums a person keeps their hands on: left unmanaged, kept as a custom
/// edition, or stepped back from and not handed back.
const HANDS_OFF: &str = "SELECT local_album_id FROM library_management_exclusions \
    UNION SELECT local_album_id FROM library_custom_edition_active \
    UNION SELECT local_album_id FROM library_album_takeovers WHERE handed_back_at IS NULL";

/// A track row of a hands-off album that went away in this run.
struct Lost {
    album_id: String,
    root_id: String,
    place: String,
    duration: Option<f64>,
}

/// Hold the albums whose new files took the place of a hands-off album's
/// in run `run_id`, and record the pairs for a person, in a savepoint of
/// the caller's transaction. A store error rolls the savepoint back with
/// one warning and leaves the caller's work to commit; the signal belongs
/// to the run, so it is not tried again.
pub(super) fn hold_replacements(
    tx: &Transaction<'_>,
    run_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    match savepoint(tx, "hold_replacements", || pass(tx, run_id, now))? {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(
                %error,
                run_id,
                "new files beside a hands-off album were not checked"
            );
            Ok(())
        }
    }
}

fn pass(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let started: Option<f64> = tx
        .query_row(
            "SELECT started_at FROM library_scan_runs WHERE id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(started) = started else {
        return Ok(());
    };
    let lost = lost_rows(tx, run_id, started)?;
    if lost.is_empty() {
        return Ok(());
    }
    let places: BTreeSet<(&str, &str)> = lost
        .iter()
        .map(|row| (row.root_id.as_str(), row.place.as_str()))
        .collect();
    let mut candidates = BTreeSet::new();
    for (root_id, place) in places {
        candidates.extend(new_albums_at(tx, run_id, started, root_id, place)?);
    }
    candidates.extend(held_unpaired(tx)?);
    for album_id in candidates {
        settle(tx, &lost, &album_id, started, now)?;
    }
    Ok(())
}

/// The rows of hands-off albums that went away in this run: missing since
/// it began, or still indexed and walked past.
fn lost_rows(tx: &Transaction<'_>, run_id: &str, started: f64) -> rusqlite::Result<Vec<Lost>> {
    // CROSS JOIN keeps the order: the few hands-off albums first, never a
    // walk over every live album.
    let sql = format!(
        "SELECT t.id, t.local_album_id, t.root_id, t.relative_path, t.duration_seconds \
         FROM ({HANDS_OFF}) h \
         CROSS JOIN local_albums a ON a.id = h.local_album_id \
         CROSS JOIN local_tracks t ON t.local_album_id = a.id \
         WHERE a.retired_into_album_id IS NULL AND ((t.availability = 'missing' \
         AND t.missing_since >= ?2) OR (t.availability = 'indexed' AND {WALKED_PAST}))"
    );
    tx.prepare_cached(&sql)?
        .query_map(params![run_id, started], |row| {
            let id: String = row.get(0)?;
            let path: String = row.get(3)?;
            Ok(Lost {
                place: place_of(&id, &path),
                album_id: row.get(1)?,
                root_id: row.get(2)?,
                duration: row.get(4)?,
            })
        })?
        .collect()
}

/// The albums of rows new in this run whose album folder sits at `place`
/// under `root_id`.
fn new_albums_at(
    tx: &Transaction<'_>,
    run_id: &str,
    started: f64,
    root_id: &str,
    place: &str,
) -> rusqlite::Result<BTreeSet<String>> {
    const NEW: &str = "SELECT t.id, t.local_album_id, t.relative_path FROM local_tracks t \
         WHERE t.root_id = ?2 AND t.availability = 'indexed' AND t.ingest_source = 'scan' \
         AND t.imported_at >= ?3 \
         AND EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ?1 \
           AND i.root_id = t.root_id AND i.relative_path = t.relative_path)";
    let (sql, below) = if place == "." {
        (NEW.to_owned(), None)
    } else {
        (
            format!("{NEW} AND t.relative_path >= ?4 AND t.relative_path < ?5"),
            Some((format!("{place}/"), format!("{place}0"))),
        )
    };
    let mut statement = tx.prepare_cached(&sql)?;
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String, String)> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    };
    let rows = match &below {
        Some((from, to)) => statement
            .query_map(params![run_id, root_id, started, from, to], map)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => statement
            .query_map(params![run_id, root_id, started], map)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(rows
        .into_iter()
        .filter(|(id, _, path)| place_of(id, path) == place)
        .map(|(_, album_id, _)| album_id)
        .collect())
}

/// Albums already held with this cause that no pair names as the new
/// album.
fn held_unpaired(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    tx.prepare_cached(
        "SELECT k.local_album_id FROM library_album_takeovers k \
         WHERE k.cause = ?1 AND k.handed_back_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM library_retag_pairs p \
           WHERE p.new_album_id = k.local_album_id)",
    )?
    .query_map(params![Cause::ReplacedHandsOff.as_str()], |row| row.get(0))?
    .collect()
}

/// Hold `album_id` and record its pair when the rule says so.
fn settle(
    tx: &Transaction<'_>,
    lost: &[Lost],
    album_id: &str,
    started: f64,
    now: f64,
) -> rusqlite::Result<()> {
    let Some((created, live, foreign, excluded, held)) = tx
        .prepare_cached(
            "SELECT b.created_at, b.retired_into_album_id IS NULL, \
             EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
               AND (t.ingest_source <> 'scan' OR t.imported_at < b.created_at)), \
             EXISTS (SELECT 1 FROM library_management_exclusions e \
               WHERE e.local_album_id = b.id), \
             EXISTS (SELECT 1 FROM library_album_takeovers k WHERE k.local_album_id = b.id \
               AND k.cause = ?2 AND k.handed_back_at IS NULL) \
             FROM local_albums b WHERE b.id = ?1",
        )?
        .query_row(params![album_id, Cause::ReplacedHandsOff.as_str()], |row| {
            Ok((
                row.get::<_, f64>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .optional()?
    else {
        return Ok(());
    };
    if !live || foreign || excluded || !(created >= started || held) {
        return Ok(());
    }
    if match_chosen(tx, album_id)? {
        return Ok(());
    }
    let rows: Vec<(String, String, String, Option<f64>)> = tx
        .prepare_cached(
            "SELECT id, root_id, relative_path, duration_seconds FROM local_tracks \
             WHERE local_album_id = ?1 ORDER BY id",
        )?
        .query_map(params![album_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let places: BTreeSet<(String, String)> = rows
        .iter()
        .map(|(id, root_id, path, _)| (root_id.clone(), place_of(id, path)))
        .collect();
    let mut beside: BTreeMap<&str, Vec<Option<f64>>> = BTreeMap::new();
    for row in lost.iter().filter(|row| {
        row.album_id != album_id && places.contains(&(row.root_id.clone(), row.place.clone()))
    }) {
        beside
            .entry(row.album_id.as_str())
            .or_default()
            .push(row.duration);
    }
    let lengths: Vec<Option<f64>> = rows.iter().map(|row| row.3).collect();
    let replaced: Vec<&str> = beside
        .iter()
        .filter(|(_, gone)| fits(&lengths, gone))
        .map(|(old, _)| *old)
        .collect();
    // An album held in an earlier run is still paired once the old one
    // empties, whatever went in this run.
    if replaced.is_empty() && !held {
        return Ok(());
    }
    let ids: Vec<String> = rows.iter().map(|row| row.0.clone()).collect();
    let marked = takeover::mark(tx, album_id, Cause::ReplacedHandsOff, &ids, None, now)?;
    let mut fitting = Vec::new();
    for old in beside.keys() {
        let missing: Vec<Option<f64>> = tx
            .prepare_cached(
                "SELECT duration_seconds FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'missing'",
            )?
            .query_map(params![old], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if fits(&lengths, &missing) {
            fitting.push(*old);
        }
    }
    let mut paired = None;
    if let [old] = fitting.as_slice() {
        let present: bool = tx
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'indexed')",
            )?
            .query_row(params![old], |row| row.get(0))?;
        if !present
            && retag_pairs::record(
                tx,
                old,
                album_id,
                RetagPairReason::ReplacedHandsOff,
                (now * 1000.0) as i64,
            )?
        {
            paired = Some(*old);
        }
    }
    if marked || paired.is_some() {
        tracing::info!(
            album_id,
            replaced = ?replaced,
            paired = paired.unwrap_or(""),
            "new files beside a hands-off album left alone in case they are its retag"
        );
    }
    Ok(())
}

/// Whether at least half of `new` (rounded up) pair one to one with
/// `old` by length. Two unknown lengths fit; a known one fits a known one
/// within the tolerance.
fn fits(new: &[Option<f64>], old: &[Option<f64>]) -> bool {
    if new.is_empty() {
        return false;
    }
    let unknown =
        |lengths: &[Option<f64>]| lengths.iter().filter(|length| length.is_none()).count();
    let known = |lengths: &[Option<f64>]| {
        let mut known: Vec<f64> = lengths.iter().flatten().copied().collect();
        known.sort_by(f64::total_cmp);
        known
    };
    let (a, b) = (known(new), known(old));
    let mut fitted = unknown(new).min(unknown(old));
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if (a[i] - b[j]).abs() < DURATION_TOLERANCE_SECONDS {
            fitted += 1;
            i += 1;
            j += 1;
        } else if a[i] < b[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    fitted * 2 >= new.len()
}

/// The folder an album folder at `relative_path` sits in: the parent of
/// its grouping folder, `.` at the top of the root. A row that gave its
/// path up to a file placed there is read at the path it had.
fn place_of(track_id: &str, relative_path: &str) -> String {
    let hidden = format!("{HIDDEN_PREFIX}gone/{track_id}/");
    let path = relative_path.strip_prefix(&hidden).unwrap_or(relative_path);
    let folder = grouping_directory(path);
    folder
        .rsplit_once('/')
        .map_or(".", |(parent, _)| parent)
        .to_owned()
}
