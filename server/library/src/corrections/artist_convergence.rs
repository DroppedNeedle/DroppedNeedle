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
//! in the group only through a matched album's proofs takes others only
//! when it has credits and every one holds the artist too: one wrong album
//! match cannot pull other rows into it. A survivor the app linked takes
//! them when that holds, or when its own files hold the artist's id and
//! nothing names another, so its link does not rest on a match alone.
//! A group an administrator marked as distinct artists is left alone. The
//! merge is the manual merge's, with no undo: credits, favorites, plays,
//! links and app ids move, and the old id stays as an alias. No file is
//! read or written.
//!
//! A row a pass of artist links left empty follows its credits
//! ([`follow_moved`]): when a credit proven to be the artist moved to
//! another row (a changed sort name, say), nothing proves that row another
//! artist, and the old row has nothing left of its own, it is merged into
//! the row the credit went to. While some of its credits have yet to move
//! (a scan commits a large retag in several windows), the old row waits as
//! an open merge candidate and is looked at again at each run end and in
//! each sweep ([`follow_waiting`]).
//!
//! The app's links among a run end's candidates are settled first, so a
//! link whose support is gone does not lead a group.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::artists::{CREDITS_MOVED, retire_artist};
use crate::catalog_revision::bump_catalog;
use crate::identify::artist_links::{CURRENT_PROOF, MovedProof, settle};
use crate::operations::decisions::{ArtistAction, record_artist_action};
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

/// Merge every group of live rows proven to be one MusicBrainz artist,
/// then the rows waiting to follow their credits that now can. A group
/// the store fails on is rolled back and logged, and the next run end
/// tries it again.
pub(crate) fn converge_artists(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    match candidate_ids(tx) {
        Ok(ids) => {
            settle_automatic(tx, &ids, now)?;
            converge_ids(tx, &ids, now)?;
        }
        Err(error) => {
            tracing::warn!(%error, "artist convergence could not list its candidates");
        }
    }
    follow_waiting(tx, now)
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
        let takes_others = if survivor.proof_only {
            fully_proven(tx, &survivor.id, mbid)?
        } else if survivor.automatic {
            fully_proven(tx, &survivor.id, mbid)? || files_prove(tx, &survivor.id, mbid)?
        } else {
            true
        };
        if !takes_others {
            tracing::debug!(
                artist_mbid = mbid,
                artist_id = survivor.id,
                "survivor proven by a matched album has credits not proven to be this artist; \
                 not merged"
            );
            continue;
        }
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        if marked_distinct(tx, &ids)? {
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

/// Whether a file of one of `artist`'s credits holds `mbid` and nothing
/// proves it another artist: what lets a survivor the app linked take in
/// others while a credit of its own is not proven.
fn files_prove(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let held: bool = tx
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM local_track_artists \
             WHERE local_artist_id = ?1 AND artist_mbid = ?2) \
             OR EXISTS (SELECT 1 FROM local_album_artists \
             WHERE local_artist_id = ?1 AND artist_mbid = ?2)",
        )?
        .query_row(params![artist, mbid], |row| row.get(0))?;
    Ok(held && !other_id(tx, artist, mbid)?)
}

/// True when something other than credits still on it keeps row `?1`,
/// whose proven credits for `?3` moved to the row that is now `?2`, from
/// following them: it is not live, is a sentinel (`?4`, `?5`) or a Various
/// Artists row, has a link, has a proof naming another id, or has a play
/// of a track now credited to neither row (a play goes with its row, and
/// must not land on another artist's).
const HELD_BACK: &str = "SELECT NOT EXISTS (SELECT 1 FROM local_artists WHERE id = ?1 \
     AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?4, ?5)) \
     OR EXISTS (SELECT 1 FROM local_artist_external_identities WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM library_artist_credit_proofs \
      WHERE (source_local_artist_id = ?1 OR local_artist_id = ?1) AND artist_mbid <> ?3) \
     OR EXISTS (SELECT 1 FROM library_play_history h WHERE h.local_artist_id = ?1 \
      AND EXISTS (SELECT 1 FROM local_track_artists c WHERE c.local_track_id = h.local_track_id) \
      AND NOT EXISTS (SELECT 1 FROM local_track_artists c \
       WHERE c.local_track_id = h.local_track_id AND c.local_artist_id IN (?1, ?2)))";

/// True when row `?1` still has a credit, or an album names it as album
/// artist: it follows its credits once the rest of them have moved.
const STILL_CREDITED: &str = "SELECT \
     EXISTS (SELECT 1 FROM local_album_artists WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM local_albums WHERE album_artist_id = ?1)";

/// The live row `artist` is now: itself, or the row a chain of merges
/// retired it into.
fn live_row(tx: &Transaction<'_>, artist: &str) -> rusqlite::Result<Option<String>> {
    tx.prepare_cached(
        "WITH RECURSIVE chain(id, next, depth) AS ( \
         SELECT id, retired_into_artist_id, 0 FROM local_artists WHERE id = ?1 \
         UNION ALL SELECT a.id, a.retired_into_artist_id, chain.depth + 1 \
          FROM local_artists a JOIN chain ON a.id = chain.next WHERE chain.depth < 16) \
         SELECT id FROM chain WHERE next IS NULL LIMIT 1",
    )?
    .query_row(params![artist], |row| row.get(0))
    .optional()
}

/// Whether `artist` is proven to be an artist other than `mbid`: a link
/// to another id, a file holding another id, or a current proof naming
/// another.
fn contradicted(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let link: Option<String> = tx
        .prepare_cached(
            "SELECT lower(provider_artist_id) FROM local_artist_external_identities \
             WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
        )?
        .query_row(params![artist], |row| row.get(0))
        .optional()?;
    Ok(link.is_some_and(|link| link != mbid) || other_id(tx, artist, mbid)?)
}

/// Where one row whose proven credits moved stands.
enum Follow {
    /// Merged into the row its credits went to.
    Merged,
    /// Some of its credits have not moved yet.
    Waiting(String),
    /// It stays where it is.
    Stays,
}

/// Merge row `from` into the live row of `to`, where its credits proven to
/// be `mbid` moved, once nothing of its own is left ([`HELD_BACK`],
/// [`STILL_CREDITED`]) and nothing proves that row another artist. A
/// merge the store fails on is rolled back and logged.
fn follow_one(
    tx: &Transaction<'_>,
    from: &str,
    to: &str,
    mbid: &str,
    now: f64,
) -> rusqlite::Result<Follow> {
    let Some(survivor) = live_row(tx, to)? else {
        return Ok(Follow::Stays);
    };
    if survivor == from {
        return Ok(Follow::Stays);
    }
    let held: bool = tx.prepare_cached(HELD_BACK)?.query_row(
        params![
            from,
            survivor,
            mbid,
            SENTINEL_ARTIST_IDS[0],
            SENTINEL_ARTIST_IDS[1]
        ],
        |row| row.get(0),
    )?;
    if held {
        tracing::debug!(
            artist_mbid = mbid,
            artist_id = from,
            "artist row whose credits moved has something of its own; not merged"
        );
        return Ok(Follow::Stays);
    }
    if contradicted(tx, &survivor, mbid)? {
        tracing::debug!(
            artist_mbid = mbid,
            artist_id = survivor,
            "row the credits moved to is proven to be another artist; not merged"
        );
        return Ok(Follow::Stays);
    }
    if marked_distinct(tx, &[from, survivor.as_str()])? {
        tracing::info!(
            artist_mbid = mbid,
            artist_ids = ?[from, survivor.as_str()],
            "artist rows marked distinct by an administrator; not merged"
        );
        return Ok(Follow::Stays);
    }
    let credited: bool = tx
        .prepare_cached(STILL_CREDITED)?
        .query_row(params![from], |row| row.get(0))?;
    if credited {
        return Ok(Follow::Waiting(survivor));
    }
    let retire = [from.to_owned()];
    match savepoint(tx, "artist_follow_credits", || {
        merge_group(tx, &survivor, &retire, mbid, now)
    })? {
        Ok(()) => {
            tracing::info!(
                artist_mbid = mbid,
                surviving_artist_id = survivor,
                retired_artist_id = from,
                "artist row left empty merged into the row its credits moved to"
            );
            Ok(Follow::Merged)
        }
        Err(error) => {
            tracing::warn!(
                %error,
                artist_mbid = mbid,
                surviving_artist_id = survivor,
                retired_artist_id = from,
                "artist row left empty could not be merged; it stays as it is"
            );
            Ok(Follow::Stays)
        }
    }
}

/// The id of the open candidate recording that `from` waits to follow its
/// credits proven to be `mbid`: the id is read back from it.
fn waiting_id(from: &str, mbid: &str) -> String {
    format!("credits-moved:{from}:{mbid}")
}

/// Merge each row a pass left empty into the row its proven credits moved
/// to ([`follow_one`]). A row with credits still to move is recorded as an
/// open merge candidate and looked at again by [`follow_waiting`]. Runs
/// after every pass, merging or not, since the moved proof is overwritten
/// in place and no later pass can find the move again.
pub(crate) fn follow_moved(
    tx: &Transaction<'_>,
    moved: &[MovedProof],
    now: f64,
) -> rusqlite::Result<()> {
    let mut seen = BTreeSet::new();
    let mut merged = 0;
    for MovedProof { from, to, mbid } in moved {
        if !seen.insert(from.as_str()) {
            continue;
        }
        match follow_one(tx, from, to, mbid, now)? {
            Follow::Merged => merged += 1,
            Follow::Waiting(survivor) => {
                tx.prepare_cached(
                    "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
                     right_artist_id, reason_code, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                )?
                .execute(params![
                    waiting_id(from, mbid),
                    from,
                    survivor,
                    CREDITS_MOVED,
                    now
                ])?;
            }
            Follow::Stays => {}
        }
    }
    if merged > 0 {
        bump_catalog(tx)?;
    }
    Ok(())
}

/// Whether a row waits to follow its credits ([`follow_waiting`]).
pub(crate) fn any_waiting(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM local_artist_merge_candidates \
         WHERE reason_code = ?1 AND state = 'open')",
    )?
    .query_row(params![CREDITS_MOVED], |row| row.get(0))
}

/// Look again at every row waiting to follow its credits
/// ([`follow_moved`]), at the end of each scan run and in each sweep of
/// identification's artist links. A merge resolves the candidate; one
/// still waiting stays open.
pub(crate) fn follow_waiting(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let waiting: Vec<(String, String, String)> = tx
        .prepare_cached(
            "SELECT id, left_artist_id, right_artist_id FROM local_artist_merge_candidates \
             WHERE reason_code = ?1 AND state = 'open' ORDER BY id",
        )?
        .query_map(params![CREDITS_MOVED], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut merged = 0;
    for (id, from, to) in waiting {
        let Some(mbid) = id
            .strip_prefix(&format!("credits-moved:{from}:"))
            .map(str::to_owned)
        else {
            tracing::warn!(
                candidate_id = id,
                "waiting artist follow has no artist id; skipped"
            );
            continue;
        };
        if let Follow::Merged = follow_one(tx, &from, &to, &mbid, now)? {
            merged += 1;
        }
    }
    if merged > 0 {
        bump_catalog(tx)?;
    }
    Ok(())
}

/// Whether an administrator marked any two of the rows as distinct
/// artists, at any revision, either row possibly merged since.
fn marked_distinct(tx: &Transaction<'_>, ids: &[&str]) -> rusqlite::Result<bool> {
    let ids = serde_json::to_string(ids).unwrap_or_default();
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
