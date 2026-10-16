//! Merge artist rows MusicBrainz proves to be one artist, at the end of
//! every scan run.
//!
//! Two live rows end up proven to be one artist when, for example, a
//! person links a row to an artist whose files already proved another
//! row, or a row's contradicting files were retagged away. The survivor
//! is the linked row, else the oldest. A row is retired into it only when
//! it has no link and every one of its credits carries the artist's id,
//! so files without an id never move with it; a group an administrator
//! marked as distinct artists is left alone. The merge is the manual
//! merge's, with no undo: credits, favorites, plays, links and app ids
//! move, and the old id stays as an alias. No file is read or written.

use rusqlite::{Transaction, params};

use super::artists::retire_artist;
use crate::operations::decisions::{ArtistAction, bump_catalog, record_artist_action};
use crate::scan::sqlite_store::artists::{Proven, proven_rows};

/// The reason the duplicate artists list explains an automatic merge by.
const REASON: &str = "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE";

/// Merge every group of live rows proven to be one MusicBrainz artist.
/// A group the store fails on is rolled back and logged, and the next run
/// end tries it again.
pub(crate) fn converge_artists(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let ids = match candidate_ids(tx) {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(%error, "artist convergence could not list its candidates");
            return Ok(());
        }
    };
    let mut merged = 0;
    for mbid in ids {
        let rows = proven_rows(tx, &mbid)?;
        let Some((survivor, others)) = rows.split_first() else {
            continue;
        };
        let mut retire = Vec::new();
        for row in others.iter().filter(|row| !row.linked) {
            if every_credit_holds(tx, &row.id, &mbid)? {
                retire.push(row.id.clone());
            } else {
                tracing::debug!(
                    artist_mbid = mbid,
                    artist_id = row.id,
                    "artist row has credits without this id; not merged"
                );
            }
        }
        if retire.is_empty() {
            continue;
        }
        if marked_distinct(tx, &rows)? {
            tracing::info!(
                artist_mbid = mbid,
                artist_ids = ?rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
                "artist rows marked distinct by an administrator; not merged"
            );
            continue;
        }
        tx.execute("SAVEPOINT artist_convergence", [])?;
        match merge_group(tx, &survivor.id, &retire, &mbid, now) {
            Ok(()) => {
                tx.execute("RELEASE artist_convergence", [])?;
                merged += 1;
                tracing::info!(
                    artist_mbid = mbid,
                    surviving_artist_id = survivor.id,
                    retired_artist_ids = ?retire,
                    "artist rows proven to be one artist merged"
                );
            }
            Err(error) => {
                tx.execute("ROLLBACK TO artist_convergence", [])?;
                tx.execute("RELEASE artist_convergence", [])?;
                tracing::warn!(
                    %error,
                    artist_mbid = mbid,
                    surviving_artist_id = survivor.id,
                    retired_artist_ids = ?retire,
                    "artist rows proven to be one artist could not be merged; next scan tries again"
                );
            }
        }
    }
    if merged > 0 {
        bump_catalog(tx)?;
    }
    Ok(())
}

/// The ids two or more live rows hold, by their credits or a person's or
/// v2's link: the only ids that can have a group to merge.
fn candidate_ids(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    tx.prepare_cached(
        "SELECT c.mbid FROM ( \
         SELECT artist_mbid AS mbid, local_artist_id AS artist FROM local_track_artists \
          WHERE artist_mbid IS NOT NULL \
         UNION SELECT artist_mbid, local_artist_id FROM local_album_artists \
          WHERE artist_mbid IS NOT NULL \
         UNION SELECT lower(provider_artist_id), local_artist_id \
          FROM local_artist_external_identities WHERE provider = 'musicbrainz' \
          AND decision_source IN ('manual', 'legacy_import')) c \
         JOIN local_artists a ON a.id = c.artist AND a.retired_into_artist_id IS NULL \
         GROUP BY c.mbid HAVING COUNT(*) > 1 ORDER BY c.mbid",
    )?
    .query_map([], |row| row.get(0))?
    .collect()
}

/// Whether every track and album credit of `artist` carries `mbid`.
fn every_credit_holds(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let other: bool = tx
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1 \
             AND artist_mbid IS NOT ?2) OR EXISTS(SELECT 1 FROM local_album_artists \
             WHERE local_artist_id = ?1 AND artist_mbid IS NOT ?2)",
        )?
        .query_row(params![artist, mbid], |row| row.get(0))?;
    Ok(!other)
}

/// Whether an administrator marked any two of the rows as distinct
/// artists, at any revision, either row possibly merged since.
fn marked_distinct(tx: &Transaction<'_>, rows: &[Proven]) -> rusqlite::Result<bool> {
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    let ids = serde_json::to_string(&ids).unwrap_or_default();
    tx.prepare_cached(
        "WITH members(id) AS (SELECT value FROM json_each(?1)), \
         pairs AS (SELECT \
          COALESCE((SELECT local_artist_id FROM local_artist_aliases \
           WHERE alias = d.left_artist_id), d.left_artist_id) AS left_id, \
          COALESCE((SELECT local_artist_id FROM local_artist_aliases \
           WHERE alias = d.right_artist_id), d.right_artist_id) AS right_id \
          FROM library_artist_reconciliation_dismissals d) \
         SELECT EXISTS(SELECT 1 FROM pairs WHERE left_id <> right_id \
          AND left_id IN (SELECT id FROM members) AND right_id IN (SELECT id FROM members))",
    )?
    .query_row(params![ids], |row| row.get(0))
}

/// Retire each row into the survivor, mark what proved it, and write one
/// audit row for the group.
fn merge_group(
    tx: &Transaction<'_>,
    survivor: &str,
    retire: &[String],
    mbid: &str,
    now: f64,
) -> rusqlite::Result<()> {
    for artist in retire {
        retire_artist(tx, artist, survivor, now)?;
        tx.execute(
            "UPDATE local_artists SET converged_mbid = ?2 WHERE id = ?1",
            params![artist, mbid],
        )?;
    }
    tx.execute(
        "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1",
        params![survivor, now],
    )?;
    record_artist_action(
        tx,
        ArtistAction {
            actor: "system",
            kind: "merge_artist",
            artist_id: survivor,
            before: serde_json::json!({ "artist_ids": retire }),
            after: serde_json::json!({
                "kind": "artist_convergence",
                "surviving_artist_id": survivor,
                "retired_artist_ids": retire,
                "provider_artist_mbid": mbid,
                "filesystem_writes": 0,
            }),
            reason: REASON,
        },
        now,
    )?;
    Ok(())
}
