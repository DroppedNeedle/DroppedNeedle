//! Joining an album to the album whose files replaced it on the same
//! edition.
//!
//! An album retagged outside the app to a new name, with its MusicBrainz
//! ids removed and its files renamed in one go, comes back as new track
//! rows on a new album row. No track moved between albums, so neither the
//! scan's follow nor its join across scans sees it. Once identification
//! matches the new row to the edition the old row was on, the old row A is
//! merged into the new row B ([`merge_retagged`]) when all of this holds:
//!
//! 1. A and B are live, different rows, each with an accepted exact
//!    match ([`accepted_release`]) naming the same release R;
//! 2. no track row of A is indexed;
//! 3. the rule that continues a missing track ([`pairs_among`]), run over
//!    A and the albums on R together, pairs missing rows O of A with
//!    indexed rows N on other albums. O is replaced by N when
//!    1. a scan on record marked O missing, while it ran, and no person
//!       removed O in the app;
//!    2. the run history reaches back to N: some run on record ended at or
//!       before N was first indexed (history drops the oldest runs
//!       first);
//!    3. no finished scan saw both files: none ended after N was first
//!       indexed and before O went missing with a completed walk of a
//!       folder holding O's file;
//!    4. no finished scan saw O gone before N arrived: none ended after O
//!       went missing and before N was first indexed with a completed walk
//!       of a folder holding N's file;
//!    5. when N was first indexed before the scan that marked O missing
//!       began, a later scan has run: a share that dropped out under an
//!       older copy comes back in that time.
//!
//!    A pair failing 3.2, 3.3 or 3.4 stops the join: it is proof of two
//!    copies or of a deletion and a later arrival, and once A sat on B the
//!    continuation would move that row anyway. A pair failing 3.1 only
//!    stays behind;
//! 4. no pair naming A as the old album was dismissed by a person, and
//!    the pair (A, B) is not on record or waits to be tried again;
//! 5. no row that stayed behind went missing in the latest scan run
//!    ([`holds`]), and when no more rows were replaced than stayed, each
//!    row that stayed is excluded or has been gone a day
//!    ([`waits_for_the_rest`]). Until then nothing is done or recorded;
//! 6. every replaced row was replaced by a row of one album B. Rows
//!    replaced on several albums (a split) are never merged by the app:
//!    each pair is recorded once as `tracks_elsewhere` for a person.
//!
//! Nothing else is weighed: not names, recordings, lengths, paths or
//! folders. A merge refused (live work, a store error) is recorded and
//! tried again at the next run end. No file is read or written and no
//! track row is deleted; the callers continue the tracks afterwards.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, Transaction, params};

use super::continuation::{Pair, Scope, accepted_release, pairs_among};
use super::retag_merge::{JoinCause, MergeOutcome, merge_retagged};
use crate::publish::HIDDEN_PREFIX;
use crate::scan::models::scope_covers_path;
use crate::scan::sqlite_store::retag_pairs::{RetagPairReason, RetagPairState};
use crate::scan::sqlite_store::{RetagPair, holds, tried, waits_for_the_rest};
use droppedneedle_kernel::db::savepoint;

/// The scan run that started last, as a SQL expression.
const LATEST_RUN: &str = "(SELECT id FROM library_scan_runs WHERE started_at IS NOT NULL \
    ORDER BY started_at DESC LIMIT 1)";

/// Old and new albums on one accepted exact release: the old one with no
/// indexed track and a mapped track that went missing since the oldest
/// run on record began, the new one with an indexed track. Starts from the
/// missing rows and joins the two albums through their release.
const CANDIDATES: &str = "SELECT DISTINCT a.id, b.id FROM local_tracks o \
    JOIN local_albums a ON a.id = o.local_album_id AND a.retired_into_album_id IS NULL \
    JOIN local_album_external_identities ae \
      ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' \
    JOIN local_album_external_identities be \
      ON lower(be.release_mbid) = lower(ae.release_mbid) AND be.provider = 'musicbrainz' \
      AND be.local_album_id <> a.id \
    JOIN local_albums b ON b.id = be.local_album_id AND b.retired_into_album_id IS NULL \
    WHERE o.availability = 'missing' \
    AND o.missing_since >= (SELECT MIN(started_at) FROM library_scan_runs) \
    AND (?1 IS NULL OR a.id = ?1) AND (?2 IS NULL OR b.id = ?2) \
    AND trim(COALESCE(ae.release_mbid, '')) <> '' \
    AND EXISTS (SELECT 1 FROM local_track_external_identities oi \
      WHERE oi.local_track_id = o.id AND oi.provider = 'musicbrainz' \
      AND trim(COALESCE(oi.release_track_mbid, '')) <> '') \
    AND NOT EXISTS (SELECT 1 FROM local_tracks t \
      WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
    AND EXISTS (SELECT 1 FROM local_tracks t \
      WHERE t.local_album_id = b.id AND t.availability = 'indexed') \
    AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
      WHERE s.local_album_id IN (a.id, b.id)) \
    AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
      WHERE c.local_album_id IN (a.id, b.id)) \
    ORDER BY a.id, b.id";

/// Join every album whose files another album replaced on the same
/// edition, in a savepoint of the caller's transaction. With
/// [`Scope::Album`], only old albums that would join the album named are
/// looked at. A store error rolls the savepoint back with one warning and
/// leaves the caller's work to commit; the next run end tries again. Call
/// it after the transaction's last identity and match-state write and
/// before [`continue_tracks`](super::continuation::continue_tracks).
pub(crate) fn join_replaced(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    let mut at: (Option<String>, Option<String>) = (None, None);
    match savepoint(tx, "join_replaced", || pass(tx, scope, now, &mut at))? {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(
                %error,
                album_id = at.0.as_deref().unwrap_or(""),
                target = at.1.as_deref().unwrap_or(""),
                "album not joined to the album that replaced it; the next run end tries again"
            );
            Ok(())
        }
    }
}

fn candidates(
    conn: &Connection,
    old: Option<&str>,
    new: Option<&str>,
) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached(CANDIDATES)?
        .query_map(params![old, new], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn pass(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    now: f64,
    at: &mut (Option<String>, Option<String>),
) -> rusqlite::Result<()> {
    let mut olds: BTreeMap<String, Vec<String>> = BTreeMap::new();
    match scope {
        Scope::Library => {
            for (old, new) in candidates(tx, None, None)? {
                olds.entry(old).or_default().push(new);
            }
        }
        // Each old album is weighed against every album on its release,
        // so a split is seen whichever album asked.
        Scope::Album(album) => {
            for (old, _) in candidates(tx, None, Some(album))? {
                let news = candidates(tx, Some(&old), None)?
                    .into_iter()
                    .map(|(_, new)| new)
                    .collect();
                olds.insert(old, news);
            }
        }
    }
    for (old, news) in &olds {
        at.0 = Some(old.clone());
        at.1 = None;
        let Some(targets) = replaced_by(tx, old, news, now)? else {
            continue;
        };
        if let Scope::Album(album) = scope
            && !targets.contains(album)
        {
            continue;
        }
        let mut targets = targets.iter();
        match (targets.next(), targets.next()) {
            (Some(new), None) => {
                at.1 = Some(new.clone());
                join(tx, old, new, now)?;
            }
            (Some(first), Some(second)) => {
                for new in [first, second].into_iter().chain(targets) {
                    at.1 = Some(new.clone());
                    if tried(tx, old, new)? {
                        RetagPair { old, new, now }.leave_apart(
                            tx,
                            RetagPairReason::TracksElsewhere,
                            None,
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// What became of one pair of rows.
pub(crate) enum Verdict {
    /// O is replaced by N.
    Replaced,
    /// O stays behind: the pair does not count.
    Stayed,
    /// The pair proves two copies or a deletion: nothing joins.
    Veto,
    /// Nothing is done until a later scan.
    Wait,
}

/// The albums whose rows replaced rows of `old`, when the rule holds and
/// nothing waits; `None` when nothing is to be done now.
fn replaced_by(
    tx: &Transaction<'_>,
    old: &str,
    news: &[String],
    now: f64,
) -> rusqlite::Result<Option<BTreeSet<String>>> {
    let Some(release) = accepted_release(tx, old)? else {
        return Ok(None);
    };
    let albums: Vec<&str> = std::iter::once(old)
        .chain(news.iter().map(String::as_str))
        .collect();
    let mut replaced = Vec::new();
    let mut targets = BTreeSet::new();
    for pair in pairs_among(tx, &albums, &release)? {
        if pair.old_album != old || pair.new_album == old {
            continue;
        }
        match judge(tx, &pair, now)? {
            Verdict::Replaced => {
                replaced.push(pair.old);
                targets.insert(pair.new_album);
            }
            Verdict::Stayed => {}
            Verdict::Veto | Verdict::Wait => return Ok(None),
        }
    }
    if replaced.is_empty() {
        return Ok(None);
    }
    let dismissed: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND state = ?2)",
        )?
        .query_row(params![old, RetagPairState::Dismissed.as_str()], |row| {
            row.get(0)
        })?;
    if dismissed {
        return Ok(None);
    }
    let replaced_json = serde_json::to_string(&replaced)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let (held, rows): (bool, i64) = tx
        .prepare_cached(&format!(
            "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?1 \
             AND t.id NOT IN (SELECT value FROM json_each(?2)) AND {}), \
             (SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1)",
            holds("t", LATEST_RUN)
        ))?
        .query_row(params![old, replaced_json], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let moved = replaced.len() as i64;
    if held || waits_for_the_rest(tx, old, moved, rows - moved, &replaced, now)? {
        return Ok(None);
    }
    Ok(Some(targets))
}

/// Whether `pair.old` is replaced by `pair.new` (rule 3.1 to 3.5). Reads
/// only the two rows' times and paths and the run history, never their
/// identities. Times are seconds.
pub(crate) fn judge(tx: &Transaction<'_>, pair: &Pair, now: f64) -> rusqlite::Result<Verdict> {
    let (missing_since, old_root, old_path): (Option<f64>, String, String) = tx
        .prepare_cached(
            "SELECT missing_since, root_id, relative_path FROM local_tracks WHERE id = ?1",
        )?
        .query_row(params![pair.old], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let (imported_at, new_root, new_path): (Option<f64>, String, String) = tx
        .prepare_cached(
            "SELECT imported_at, root_id, relative_path FROM local_tracks WHERE id = ?1",
        )?
        .query_row(params![pair.new], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let Some(gone) = missing_since else {
        return Ok(Verdict::Stayed);
    };
    let (by_scan, removed): (bool, bool) = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM library_scan_runs \
               WHERE started_at <= ?2 AND ?2 <= COALESCE(terminal_at, ?3)), \
             EXISTS(SELECT 1 FROM library_catalog_actions WHERE local_album_id = ?4 \
               AND action_kind = 'remove_track' AND local_track_id = ?1)",
        )?
        .query_row(params![pair.old, gone, now, pair.old_album], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    if !by_scan || removed {
        return Ok(Verdict::Stayed);
    }
    let Some(arrived) = imported_at else {
        return Ok(Verdict::Veto);
    };
    let reaches: bool = tx
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM library_scan_runs WHERE terminal_at <= ?1)")?
        .query_row(params![arrived], |row| row.get(0))?;
    // A row that gave its path up to a file placed there is read at the
    // path it had.
    let hidden = format!("{HIDDEN_PREFIX}gone/{}/", pair.old);
    let old_path = old_path.strip_prefix(&hidden).unwrap_or(&old_path);
    if !reaches
        || walked(tx, &old_root, old_path, arrived, gone)?
        || walked(tx, &new_root, &new_path, gone, arrived)?
    {
        return Ok(Verdict::Veto);
    }
    // N there before the scan that marked O missing began: a share that
    // dropped out under an older copy looks the same. It waits a scan, so
    // a share back in time keeps the two apart.
    let waits: bool = tx
        .prepare_cached(&format!(
            "SELECT ?1 < (SELECT MAX(started_at) FROM library_scan_runs WHERE started_at <= ?2) \
             AND EXISTS(SELECT 1 FROM local_tracks o WHERE o.id = ?3 AND {})",
            holds("o", LATEST_RUN)
        ))?
        .query_row(params![arrived, gone, pair.old], |row| row.get(0))?;
    if waits {
        return Ok(Verdict::Wait);
    }
    Ok(Verdict::Replaced)
}

/// Whether a scan that completed, and ended at or after `from` and before
/// `to`, walked a folder holding the file to the end. A folder the
/// mass-missing guard held back does not count as walked.
fn walked(conn: &Connection, root: &str, path: &str, from: f64, to: f64) -> rusqlite::Result<bool> {
    let scopes: Vec<String> = conn
        .prepare_cached(
            "SELECT s.relative_path FROM library_scan_runs r \
             JOIN library_scan_run_scopes s ON s.run_id = r.id AND s.root_id = ?1 \
               AND s.discovery_state = 'completed' \
             WHERE r.state = 'completed' AND r.terminal_at >= ?2 AND r.terminal_at < ?3 \
             AND NOT EXISTS (SELECT 1 FROM library_scan_failures f WHERE f.run_id = r.id \
               AND f.root_id = s.root_id AND f.relative_path = s.relative_path \
               AND f.failure_code = 'MASS_MISSING_GUARD')",
        )?
        .query_map(params![root, from, to], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(scopes.iter().any(|scope| scope_covers_path(scope, path)))
}

/// Merge `old` into `new` unless the pair is settled already, recording a
/// refusal once.
fn join(tx: &Transaction<'_>, old: &str, new: &str, now: f64) -> rusqlite::Result<()> {
    if !tried(tx, old, new)? {
        return Ok(());
    }
    match merge_retagged(tx, old, new, None, now, JoinCause::SameRelease)? {
        MergeOutcome::Merged { .. } => {
            tracing::info!(
                album_id = old,
                target = new,
                "album joined to the album that replaced it on the same edition"
            );
        }
        MergeOutcome::Refused { reason, error } => {
            RetagPair { old, new, now }.leave_apart(tx, reason, error.as_ref())?;
        }
    }
    Ok(())
}
