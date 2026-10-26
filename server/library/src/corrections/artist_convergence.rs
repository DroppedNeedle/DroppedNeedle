//! Merge artist rows MusicBrainz proves to be one artist, at the end of
//! every scan run and after each pass of identification's artist links.
//!
//! Two live rows end up proven to be one artist when, for example, a
//! person links a row to an artist whose files already proved another
//! row, a row's contradicting files were retagged away, or a matched
//! album proves a row to be an artist another row already is. Which rows
//! of such a group merge, and into which, is the rule in [`merge_basis`]:
//! a row is retired only when it has no link and every one of its credits
//! holds the artist, so a file nothing proves never moves with it, and a
//! group an administrator marked as distinct artists is left alone. The
//! merge is the manual merge's, with no undo: credits, favorites, ratings,
//! plays, links and source links move, and the old id stays as an alias.
//! No file is read or written. What each merge rests on is a
//! [`MergeBasis`], made from the rows in the merging transaction and
//! written to the action log.
//!
//! A row a pass of artist links left empty follows its credits
//! ([`follow_moved`]): when a credit proven to be the artist moved to
//! another row (a changed sort name, say), nothing proves that row another
//! artist, and the old row has nothing left of its own, it is merged into
//! the row the credit went to, while a current proof there still names the
//! artist. While some of its credits have yet to move (a scan commits a
//! large retag in several windows), the old row waits as an open merge
//! candidate and is looked at again at each run end and in each sweep
//! ([`follow_waiting`]).
//!
//! A row a retag emptied with no proof at all follows its credits by name
//! ([`follow_renamed`]), the one merge here that names alone decide. The
//! scan records each credit it moves to another row ([`record_move`]).
//! At the run end, a row left with no credits merges into the row they
//! went to when every move went by name (no id in the file on either side,
//! no proof at the old credit) to that one row, the row has no link and no
//! proof, the two names differ only in sort name, capitals or spacing, the
//! play guard holds and no administrator marked them distinct. Any other
//! row a scan emptied stays, and the duplicate artists list offers it.
//!
//! The app's links among a run end's candidates are settled first, so a
//! link whose support is gone does not lead a group. MusicBrainz's
//! special-purpose artist ids are never candidates.
//!
//! [`merge_basis`]: super::merge_basis

use std::collections::BTreeSet;

use rusqlite::{Connection, Transaction, params};

use super::artists::{CREDITS_MOVED, retire_artist};
pub(crate) use super::artists::{MERGED_BY_NAME, MOVED_BY_NAME, MOVED_WITH_ID};
use super::merge_basis::{Follow, MergeBasis};
use crate::catalog_revision::bump_catalog;
use crate::identify::artist_links::{MovedProof, placeholder, settle};
use crate::identity::sql::REVISABLE;
use crate::operations::decisions::{ArtistAction, record_artist_action};
use crate::scan::sqlite_store::artists::SENTINEL_ARTIST_IDS;
use droppedneedle_kernel::db::savepoint;

/// The reason the duplicate artists list explains an automatic merge by.
const REASON: &str = "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE";

/// Merge every group of live rows proven to be one MusicBrainz artist,
/// then the rows waiting to follow their credits that now can, then the
/// rows a retag emptied by name. A group the store fails on is rolled
/// back and logged, and the next run end tries it again.
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
    follow_waiting(tx, now)?;
    follow_renamed(tx, now)
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
            .prepare_cached(&format!(
                "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = 'musicbrainz' AND decision_source IN {REVISABLE} \
                 AND lower(provider_artist_id) IN (SELECT value FROM json_each(?1))"
            ))?
            .query_map(params![wanted], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        settle(tx, &rows, (now * 1000.0) as i64)
    })?;
    if let Err(error) = settled {
        tracing::warn!(%error, "the app's artist links could not be settled; next scan tries again");
    }
    Ok(())
}

/// Merge the groups of live rows proven to be each of `ids`
/// ([`MergeBasis::proven_group`]).
pub(crate) fn converge_ids(tx: &Transaction<'_>, ids: &[String], now: f64) -> rusqlite::Result<()> {
    let mut merged = 0;
    for mbid in ids {
        let mbid = mbid.as_str();
        let Some(bases) = MergeBasis::proven_group(tx, mbid)? else {
            continue;
        };
        let Some(survivor) = bases.first().map(|basis| basis.survivor().to_owned()) else {
            continue;
        };
        let retire: Vec<String> = bases
            .iter()
            .map(|basis| basis.retiree().to_owned())
            .collect();
        match savepoint(tx, "artist_convergence", || merge_group(tx, bases, now))? {
            Ok(true) => {
                merged += 1;
                tracing::info!(
                    artist_mbid = mbid,
                    surviving_artist_id = survivor,
                    retired_artist_ids = ?retire,
                    "artist rows proven to be one artist merged"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    artist_mbid = mbid,
                    surviving_artist_id = survivor,
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

/// The ids two or more live rows hold, by their credits, a link or a
/// proof: the only ids that can have a group to merge. MusicBrainz's
/// special-purpose ids are left out.
fn candidate_ids(tx: &Transaction<'_>) -> rusqlite::Result<Vec<String>> {
    let ids: Vec<String> = tx
        .prepare_cached(
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
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids.into_iter().filter(|id| !placeholder(id)).collect())
}

/// What [`follow_one`] did with one row.
enum Followed {
    Merged,
    /// Some of its credits have not moved yet to this row.
    Waiting(String),
    Stays,
}

/// Merge row `from` into the live row of `to`, where its credits proven to
/// be `mbid` moved, when [`MergeBasis::followed`] makes a basis. A merge
/// the store fails on is rolled back and logged, and the row stays.
fn follow_one(
    tx: &Transaction<'_>,
    from: &str,
    to: &str,
    mbid: &str,
    now: f64,
) -> rusqlite::Result<Followed> {
    let basis = match MergeBasis::followed(tx, from, to, mbid)? {
        Follow::Ready(basis) => basis,
        Follow::Waiting(survivor) => return Ok(Followed::Waiting(survivor)),
        Follow::Stays => return Ok(Followed::Stays),
    };
    let survivor = basis.survivor().to_owned();
    match savepoint(tx, "artist_follow_credits", || {
        merge_group(tx, vec![basis], now)
    })? {
        Ok(true) => {
            tracing::info!(
                artist_mbid = mbid,
                surviving_artist_id = survivor,
                retired_artist_id = from,
                "artist row left empty merged into the row its credits moved to"
            );
            Ok(Followed::Merged)
        }
        Ok(false) => Ok(Followed::Stays),
        Err(error) => {
            tracing::warn!(
                %error,
                artist_mbid = mbid,
                surviving_artist_id = survivor,
                retired_artist_id = from,
                "artist row left empty could not be merged; it stays as it is"
            );
            Ok(Followed::Stays)
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
            Followed::Merged => merged += 1,
            Followed::Waiting(survivor) => {
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
            Followed::Stays => {}
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
        if let Followed::Merged = follow_one(tx, &from, &to, &mbid, now)? {
            merged += 1;
        }
    }
    if merged > 0 {
        bump_catalog(tx)?;
    }
    Ok(())
}

/// Retire each basis's row into their one survivor and write one audit row
/// for the group, with the basis it rests on. Bases that name more than
/// one survivor or kind, or a person's choice, are refused and nothing is
/// written: false.
fn merge_group(
    tx: &Transaction<'_>,
    bases: Vec<MergeBasis<'_>>,
    now: f64,
) -> rusqlite::Result<bool> {
    let Some(first) = bases.first() else {
        return Ok(false);
    };
    if first.chosen_by_person()
        || bases
            .iter()
            .any(|basis| basis.survivor() != first.survivor() || !basis.same_kind(first))
    {
        tracing::warn!(
            surviving_artist_id = first.survivor(),
            "artist merge bases are not one automatic merge; not merged"
        );
        return Ok(false);
    }
    let survivor = first.survivor().to_owned();
    let provider_mbid = first.mbid().map(str::to_owned);
    let reason = if provider_mbid.is_some() {
        REASON
    } else {
        MERGED_BY_NAME
    };
    let basis = MergeBasis::audit_group(&bases);
    let retire: Vec<String> = bases
        .iter()
        .map(|basis| basis.retiree().to_owned())
        .collect();
    for basis in bases {
        retire_artist(tx, basis, now)?;
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
            artist_id: &survivor,
            before: serde_json::json!({ "artist_ids": retire }),
            after: serde_json::json!({
                "kind": "artist_convergence",
                "surviving_artist_id": survivor,
                "retired_artist_ids": retire,
                "provider_artist_mbid": provider_mbid,
                "filesystem_writes": 0,
                "basis": basis,
            }),
            reason,
        },
        now,
    )?;
    Ok(true)
}

/// Record that a scan moved a credit from row `from` to row `to`, by name
/// alone or (`with_id`) where an id or a proof was involved, as an open
/// merge candidate [`follow_renamed`] and the duplicate artists list read.
/// One row per pair and kind; a move to or from Various Artists or Unknown
/// Artist is not recorded.
pub(crate) fn record_move(
    tx: &Connection,
    from: &str,
    to: &str,
    with_id: bool,
    now: f64,
) -> rusqlite::Result<()> {
    if from == to || SENTINEL_ARTIST_IDS.contains(&from) || SENTINEL_ARTIST_IDS.contains(&to) {
        return Ok(());
    }
    let (reason, kind) = if with_id {
        (MOVED_WITH_ID, "moved-with-id")
    } else {
        (MOVED_BY_NAME, "moved-by-name")
    };
    tx.prepare_cached(
        "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
         right_artist_id, reason_code, created_at, updated_at) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?5 WHERE NOT EXISTS (SELECT 1 FROM local_artists \
          WHERE id IN (?2, ?3) AND kind = 'various_artists')",
    )?
    .execute(params![
        format!("{kind}:{from}:{to}"),
        from,
        to,
        reason,
        now
    ])?;
    Ok(())
}

/// Merge each row a scan left with no credits into the one row its
/// credits went to by name, when only the sort name, capitals or spacing
/// of the name changed ([`record_move`], [`MergeBasis::emptied_by_name`]).
/// A row that does not qualify keeps its open records, which the
/// duplicate artists list reads; a merge resolves them.
pub(crate) fn follow_renamed(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let rows: Vec<String> = tx
        .prepare_cached(
            "SELECT DISTINCT m.left_artist_id FROM local_artist_merge_candidates m \
             WHERE m.reason_code IN (?1, ?2) AND m.state = 'open' \
             AND NOT EXISTS (SELECT 1 FROM local_album_artists \
              WHERE local_artist_id = m.left_artist_id) \
             AND NOT EXISTS (SELECT 1 FROM local_track_artists \
              WHERE local_artist_id = m.left_artist_id) \
             AND NOT EXISTS (SELECT 1 FROM local_albums \
              WHERE album_artist_id = m.left_artist_id) \
             ORDER BY m.left_artist_id",
        )?
        .query_map(params![MOVED_BY_NAME, MOVED_WITH_ID], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut merged = 0;
    for from in rows {
        let Follow::Ready(basis) = MergeBasis::emptied_by_name(tx, &from)? else {
            continue;
        };
        let survivor = basis.survivor().to_owned();
        match savepoint(tx, "artist_follow_name", || {
            merge_group(tx, vec![basis], now)
        })? {
            Ok(true) => {
                merged += 1;
                tracing::info!(
                    surviving_artist_id = survivor,
                    retired_artist_id = from,
                    "artist row a retag left empty merged into the row of the same name"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    surviving_artist_id = survivor,
                    retired_artist_id = from,
                    "artist row a retag left empty could not be merged; it stays as it is"
                );
            }
        }
    }
    if merged > 0 {
        bump_catalog(tx)?;
    }
    Ok(())
}
