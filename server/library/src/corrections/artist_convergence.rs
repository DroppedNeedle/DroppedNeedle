//! Merge artist rows MusicBrainz proves to be one artist, at the end of
//! every scan run and after each pass of identification's artist links.
//!
//! Two live rows end up proven to be one artist when, for example, a
//! person links a row to an artist whose files already proved another
//! row, a row's contradicting files were retagged away, or a matched
//! album proves a row to be an artist another row already is. A row with
//! no link counts when its files' ids or a matched album's current proofs
//! name the artist and nothing names another. The survivor is the row a
//! person or v2 linked, else the row the app linked, else the oldest.
//!
//! A row is retired into it only when it has no link and every one of its
//! credits holds the artist, by its file's id or by a current proof at
//! that credit, so a file nothing proves never moves with it. A survivor
//! the app linked, or one in the group only through a matched album's
//! proofs, takes others only when it has credits and every one holds the
//! artist too: one wrong album match cannot pull other rows into it.
//! A group an administrator marked as distinct artists is left alone. The
//! merge is the manual merge's, with no undo: credits, favorites, plays,
//! links and app ids move, and the old id stays as an alias. No file is
//! read or written.
//!
//! The app's links among a run end's candidates are settled first, so a
//! link whose support is gone does not lead a group.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::artists::retire_artist;
use crate::identify::artist_links::{CURRENT_PROOF, settle};
use crate::operations::decisions::{ArtistAction, bump_catalog, record_artist_action};
use crate::scan::sqlite_store::artists::{SENTINEL_ARTIST_IDS, proven_rows};
use droppedneedle_kernel::db::savepoint;

/// The reason the duplicate artists list explains an automatic merge by.
const REASON: &str = "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE";

/// SQL that is true when artist `?1` is proven to be an artist other
/// than `?2`: a credit's file holds another id, or a current proof names
/// another. Each probe is a range on an (artist, id) index.
const OTHER_ID: &str = "(EXISTS(SELECT 1 FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid < ?2) \
     OR EXISTS(SELECT 1 FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid > ?2) \
     OR EXISTS(SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid < ?2) \
     OR EXISTS(SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid > ?2) \
     OR EXISTS(SELECT 1 FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid < ?2 AND (CURRENT_PROOF)) \
     OR EXISTS(SELECT 1 FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid > ?2 AND (CURRENT_PROOF)))";

/// The rows a current proof says are `?1`.
const PROOF_ROWS: &str = "SELECT DISTINCT p.source_local_artist_id \
     FROM library_artist_credit_proofs p WHERE p.artist_mbid = ?1 \
     AND p.source_local_artist_id IS NOT NULL AND (CURRENT_PROOF)";

/// The `created_at` of row `?1`, proven to be `?2` without a link, when it
/// belongs in that group: live, neither sentinel (`?3`, `?4`) nor a
/// Various Artists row, no link, and nothing proving it another artist.
const UNLINKED_MEMBER: &str = "SELECT created_at FROM local_artists WHERE id = ?1 \
     AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?3, ?4) \
     AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities WHERE local_artist_id = ?1) \
     AND NOT OTHER_ID";

/// True when every track and album credit of artist `?1` holds `?2`: by
/// its file's id, or by a current proof of `?2` at that very credit.
const EVERY_CREDIT_HOLDS: &str = "SELECT NOT EXISTS (SELECT 1 FROM local_track_artists credit \
     WHERE credit.local_artist_id = ?1 AND credit.artist_mbid IS NOT ?2 \
     AND NOT EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
      WHERE p.subject_kind = 'track' AND p.subject_id = credit.local_track_id \
      AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
      AND p.artist_mbid = ?2 AND (CURRENT_PROOF))) \
     AND NOT EXISTS (SELECT 1 FROM local_album_artists credit \
     WHERE credit.local_artist_id = ?1 AND credit.artist_mbid IS NOT ?2 \
     AND NOT EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
      WHERE p.subject_kind = 'album' AND p.subject_id = credit.local_album_id \
      AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
      AND p.artist_mbid = ?2 AND (CURRENT_PROOF)))";

fn with_current(sql: &str) -> String {
    sql.replace("OTHER_ID", OTHER_ID)
        .replace("CURRENT_PROOF", CURRENT_PROOF)
}

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
    settle_automatic(tx, &ids, now)?;
    converge_ids(tx, &ids, now)
}

/// Settle the app's links to any of `ids` before their groups are judged.
/// A failure is rolled back and logged; the groups are judged anyway.
fn settle_automatic(tx: &Transaction<'_>, ids: &[String], now: f64) -> rusqlite::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let wanted = serde_json::to_string(ids).unwrap_or_default();
    let settled = savepoint(tx, "artist_links", || {
        let rows: Vec<String> = tx
            .prepare_cached(
                "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = 'musicbrainz' AND decision_source IN ('automatic', 'embedded') \
                 AND lower(provider_artist_id) IN (SELECT value FROM json_each(?1))",
            )?
            .query_map(params![wanted], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        settle(tx, &rows, (now * 1000.0) as i64)
    })?;
    if let Err(error) = settled {
        tracing::warn!(%error, "the app's artist links could not be settled; next scan tries again");
    }
    Ok(())
}

/// One row of a group proven to be one artist.
struct Member {
    id: String,
    /// Proven by a link, not by its files or proofs.
    linked: bool,
    /// The link is the app's own.
    automatic: bool,
    /// In the group only through a matched album's current proofs.
    proof_only: bool,
}

/// Merge the groups of live rows proven to be each of `ids`.
pub(crate) fn converge_ids(tx: &Transaction<'_>, ids: &[String], now: f64) -> rusqlite::Result<()> {
    let mut merged = 0;
    for mbid in ids {
        let mbid = mbid.as_str();
        let rows = group(tx, mbid)?;
        let Some((survivor, others)) = rows.split_first() else {
            continue;
        };
        let mut retire = Vec::new();
        for row in others.iter().filter(|row| !row.linked) {
            if every_credit_holds(tx, &row.id, mbid)? {
                retire.push(row.id.clone());
            } else {
                tracing::debug!(
                    artist_mbid = mbid,
                    artist_id = row.id,
                    "artist row has credits not proven to be this artist; not merged"
                );
            }
        }
        if retire.is_empty() {
            continue;
        }
        if (survivor.automatic || survivor.proof_only) && !fully_proven(tx, &survivor.id, mbid)? {
            tracing::debug!(
                artist_mbid = mbid,
                artist_id = survivor.id,
                "survivor proven by a matched album has credits not proven to be this artist; \
                 not merged"
            );
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
        match savepoint(tx, "artist_convergence", || {
            merge_group(tx, &survivor.id, &retire, mbid, now)
        })? {
            Ok(()) => {
                merged += 1;
                tracing::info!(
                    artist_mbid = mbid,
                    surviving_artist_id = survivor.id,
                    retired_artist_ids = ?retire,
                    "artist rows proven to be one artist merged"
                );
            }
            Err(error) => {
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

/// The live rows proven to be `mbid`, the survivor first: the row a
/// person or v2 linked, then the row the app linked, then the rows with
/// no link proven by their files or by current proofs, oldest first
/// (`created_at`, then id). A row with no link is left out when anything
/// proves it another artist.
fn group(tx: &Transaction<'_>, mbid: &str) -> rusqlite::Result<Vec<Member>> {
    let mut members: Vec<Member> = Vec::new();
    let mut unlinked: Vec<(String, bool)> = Vec::new();
    for row in proven_rows(tx, mbid)? {
        if row.linked {
            members.push(Member {
                id: row.id,
                linked: true,
                automatic: row.automatic,
                proof_only: false,
            });
        } else {
            unlinked.push((row.id, false));
        }
    }
    let by_proofs: Vec<String> = tx
        .prepare_cached(&with_current(PROOF_ROWS))?
        .query_map(params![mbid], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for row in by_proofs {
        if !unlinked.iter().any(|(id, _)| *id == row)
            && !members.iter().any(|member| member.id == row)
        {
            unlinked.push((row, true));
        }
    }
    let member_sql = with_current(UNLINKED_MEMBER);
    let mut dated: Vec<(f64, String, bool)> = Vec::new();
    for (row, proof_only) in unlinked {
        let created: Option<f64> = tx
            .prepare_cached(&member_sql)?
            .query_row(
                params![row, mbid, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
                |found| found.get(0),
            )
            .optional()?;
        if let Some(created) = created {
            dated.push((created, row, proof_only));
        }
    }
    dated.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    members.extend(dated.into_iter().map(|(_, id, proof_only)| Member {
        id,
        linked: false,
        automatic: false,
        proof_only,
    }));
    Ok(members)
}

/// The ids two or more live rows hold, by their credits, a link or a
/// proof: the only ids that can have a group to merge.
fn candidate_ids(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    tx.prepare_cached(
        "SELECT c.mbid FROM ( \
         SELECT artist_mbid AS mbid, local_artist_id AS artist FROM local_track_artists \
          WHERE artist_mbid IS NOT NULL \
         UNION SELECT artist_mbid, local_artist_id FROM local_album_artists \
          WHERE artist_mbid IS NOT NULL \
         UNION SELECT lower(provider_artist_id), local_artist_id \
          FROM local_artist_external_identities WHERE provider = 'musicbrainz' \
         UNION SELECT artist_mbid, source_local_artist_id FROM library_artist_credit_proofs \
          WHERE source_local_artist_id IS NOT NULL) c \
         JOIN local_artists a ON a.id = c.artist AND a.retired_into_artist_id IS NULL \
         GROUP BY c.mbid HAVING COUNT(*) > 1 ORDER BY c.mbid",
    )?
    .query_map([], |row| row.get(0))?
    .collect()
}

/// Whether every track and album credit of `artist` holds `mbid`, by its
/// file's id or by a current proof at that credit.
fn every_credit_holds(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&with_current(EVERY_CREDIT_HOLDS))?
        .query_row(params![artist, mbid], |row| row.get(0))
}

/// Whether `artist` has a credit, every credit holds `mbid`, and nothing
/// proves it another artist: what a survivor proven by a matched album
/// needs before it takes in other rows.
fn fully_proven(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let credited: bool = tx
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1) \
             OR EXISTS (SELECT 1 FROM local_album_artists WHERE local_artist_id = ?1)",
        )?
        .query_row(params![artist], |row| row.get(0))?;
    Ok(credited && every_credit_holds(tx, artist, mbid)? && !other_id(tx, artist, mbid)?)
}

/// Whether anything proves `artist` to be an artist other than `mbid`.
fn other_id(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&with_current("SELECT OTHER_ID"))?
        .query_row(params![artist, mbid], |row| row.get(0))
}

/// Whether an administrator marked any two of the rows as distinct
/// artists, at any revision, either row possibly merged since.
fn marked_distinct(tx: &Transaction<'_>, rows: &[Member]) -> rusqlite::Result<bool> {
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
