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
//! Live records whose names have the same `artist_name_key` (capitals,
//! spacing and Latin accents aside) join last ([`join_namesakes`]), the
//! one merge here that names decide: when they hold one MusicBrainz id
//! between them or none, and no administrator marked them distinct
//! ([`MergeBasis::same_name`]). Records with two ids between them stay
//! apart, and their ids are asked about, so the web UI can tell them
//! apart by MusicBrainz's note. Each join keeps a record of what it moved,
//! and an administrator can undo it (`artist_join`). The scan records each
//! credit it moves to another row ([`record_move`]); any row a scan
//! emptied that no join takes stays, and the duplicate artists list offers
//! it.
//!
//! The upgrade from v2 folds a carried row named exactly "Unknown Artist"
//! into the built-in one ([`fold_v2_placeholder`]) when nothing on it names
//! a real id and every credit it holds is under that name. The scan never
//! calls it.
//!
//! The app's links among a run end's candidates are settled first, so a
//! link whose support is gone does not lead a group. MusicBrainz's
//! special-purpose artist ids are never candidates.
//!
//! [`merge_basis`]: super::merge_basis

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::artist_join::ArtistJoin;
pub(crate) use super::artists::{
    CREDITS_MOVED, MERGED_BY_NAME, MOVED_BY_NAME, MOVED_WITH_ID, SAME_NAME_JOINED,
    V2_PLACEHOLDER_FOLDED,
};
use super::artists::{retarget_retired, retire_artist};
use super::merge_basis::{Follow, MergeBasis, SameName, V2PlaceholderKept};
use super::models::{ArtistAliasKind, MergeCandidateState};
use crate::catalog_actions::ActionKind;
use crate::catalog_actions::ArtistAction;
use crate::catalog_actions::record_artist_action;
use crate::catalog_revision::bump_catalog;
use crate::identify::artist_links::{MovedProof, placeholder, settle_holders};
use crate::identify::merged_ids::nominate;
use crate::scan::naming::{UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID, artist_name_key};
use crate::scan::sqlite_store::artists::{SENTINEL_ARTIST_IDS, UNKEYED_NAMES};
use droppedneedle_kernel::db::savepoint;

/// The reason the duplicate artists list explains an automatic merge by.
const REASON: &str = "AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE";

/// Merge every group of live rows proven to be one MusicBrainz artist,
/// then the rows waiting to follow their credits that now can, then join
/// records of one name. A group the store fails on is rolled back and
/// logged, and the next run end tries it again.
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
    join_namesakes(tx, now)
}

/// Settle the app's links of every row holding any of `ids`
/// ([`settle_holders`]) before their groups are judged. A failure is
/// rolled back and logged; the groups are judged anyway.
fn settle_automatic(tx: &Transaction<'_>, ids: &[String], now: f64) -> rusqlite::Result<()> {
    settle_holders(tx, ids, (now * 1000.0) as i64)
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
        match savepoint(tx, "artist_convergence", || {
            merge_group(tx, bases, None, now)
        })? {
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
        .prepare_cached(crate::corrections::sql::CANDIDATE_IDS.text())?
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
        merge_group(tx, vec![basis], None, now)
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
                tx.prepare_cached(crate::corrections::sql::FOLLOW_MOVED.text())?
                    .execute(params![
                        waiting_id(from, mbid),
                        from,
                        survivor,
                        CREDITS_MOVED,
                        crate::clock::ms(now),
                        MergeCandidateState::Open,
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
    conn.prepare_cached(crate::corrections::sql::ANY_WAITING.text())?
        .query_row(params![CREDITS_MOVED], |row| row.get(0))
}

/// Look again at every row waiting to follow its credits
/// ([`follow_moved`]), at the end of each scan run and in each sweep of
/// identification's artist links. A merge resolves the candidate; one
/// still waiting stays open.
pub(crate) fn follow_waiting(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let waiting: Vec<(String, String, String)> = tx
        .prepare_cached(crate::corrections::sql::FOLLOW_WAITING.text())?
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
/// for the group, with the basis it rests on. A same-name join also gives
/// the survivor `sort_fill` while it has no sort name, and keeps what it
/// moved as the audit row's `before_json`, for its undo. Bases that name
/// more than one survivor or kind, or a person's choice, are refused and
/// nothing is written: false.
fn merge_group(
    tx: &Transaction<'_>,
    bases: Vec<MergeBasis<'_>>,
    sort_fill: Option<&str>,
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
    let joined = first.keeps_undo();
    let (reason, kind) = if joined {
        (SAME_NAME_JOINED, "same_name_join")
    } else {
        (REASON, "artist_convergence")
    };
    let survivor_sort_name: Option<String> = if joined {
        tx.prepare_cached(crate::corrections::sql::JOIN_NAMES.text())?
            .query_row(params![survivor], crate::corrections::sql::JoinNames::read)?
            .sort_name
    } else {
        None
    };
    let basis = MergeBasis::audit_group(&bases);
    let retire: Vec<String> = bases
        .iter()
        .map(|basis| basis.retiree().to_owned())
        .collect();
    let mut retirees = Vec::new();
    for basis in bases {
        retirees.extend(retire_artist(tx, basis, now)?);
    }
    let filled_sort_name = match sort_fill {
        Some(sort) if joined && survivor_sort_name.is_none() => {
            tx.prepare_cached(crate::corrections::sql::FILL_SORT_NAME.text())?
                .execute(params![survivor, sort])?;
            Some(sort.to_owned())
        }
        _ => None,
    };
    tx.execute(
        crate::corrections::sql::BUMP_ARTIST_REVISION.text(),
        params![survivor, crate::clock::ms(now)],
    )?;
    let listed = serde_json::json!({ "artist_ids": retire });
    let before = if joined {
        let record = ArtistJoin {
            survivor: survivor.clone(),
            survivor_sort_name,
            filled_sort_name,
            retirees,
        };
        serde_json::to_value(record).unwrap_or_else(|error| {
            tracing::warn!(
                %error,
                surviving_artist_id = survivor,
                "artist join's undo record could not be written; it has no undo"
            );
            listed
        })
    } else {
        listed
    };
    record_artist_action(
        tx,
        ArtistAction {
            actor: "system",
            kind: ActionKind::MergeArtist,
            artist_id: &survivor,
            before,
            after: serde_json::json!({
                "kind": kind,
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
/// merge candidate the duplicate artists list reads.
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
    tx.prepare_cached(crate::corrections::sql::RECORD_MOVE.text())?
        .execute(params![
            format!("{kind}:{from}:{to}"),
            from,
            to,
            reason,
            crate::clock::ms(now),
            MergeCandidateState::Open,
        ])?;
    Ok(())
}

/// The most same-name groups one run end joins; the rest wait for the next.
const JOINS_PER_RUN: usize = 200;

/// Join each group of live records whose names have the same
/// `artist_name_key` and that nothing tells apart
/// ([`MergeBasis::same_name`]), up to [`JOINS_PER_RUN`] groups. A group two
/// MusicBrainz ids keep apart has its ids asked about. A group the store
/// fails on is rolled back and logged, and the next run end tries again.
pub(crate) fn join_namesakes(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    let names: Vec<String> = tx
        .prepare_cached(crate::corrections::sql::NAMESAKE_NAMES.text())?
        .query_map(
            params![SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut joined = 0;
    let mut waiting = 0;
    let mut nominated = BTreeSet::new();
    for folded in names {
        let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let rows = tx
            .prepare_cached(crate::corrections::sql::NAMESAKES_OF.text())?
            .query_map(
                params![folded, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
                crate::corrections::sql::NamesakeName::read,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for row in rows {
            let key = artist_name_key(&row.display_name);
            if !UNKEYED_NAMES.contains(&key.as_str()) {
                by_key.entry(key).or_default().push(row.id);
            }
        }
        for ids in by_key.into_values().filter(|ids| ids.len() > 1) {
            if joined >= JOINS_PER_RUN {
                waiting += 1;
                continue;
            }
            let (bases, sort_fill) = match MergeBasis::same_name(tx, &ids)? {
                SameName::Join { bases, sort_fill } => (bases, sort_fill),
                SameName::Apart(held) => {
                    nominated.extend(held);
                    continue;
                }
                SameName::Stays => continue,
            };
            let survivor = bases
                .first()
                .map(|basis| basis.survivor().to_owned())
                .unwrap_or_default();
            match savepoint(tx, "artist_same_name", || {
                merge_group(tx, bases, sort_fill.as_deref(), now)
            })? {
                Ok(true) => {
                    joined += 1;
                    tracing::info!(
                        surviving_artist_id = survivor,
                        artist_ids = ?ids,
                        "artist records of one name joined"
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        surviving_artist_id = survivor,
                        artist_ids = ?ids,
                        "artist records of one name could not be joined; next scan tries again"
                    );
                }
            }
        }
    }
    if waiting > 0 {
        tracing::info!(
            joined,
            waiting,
            "artist records of one name left to join at the next run end"
        );
    }
    if !nominated.is_empty() {
        nominate(tx, &nominated, crate::clock::ms(now))?;
    }
    if joined > 0 {
        bump_catalog(tx)?;
    }
    Ok(())
}

/// What the upgrade did with one carried v2 "Unknown Artist" row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2Fold {
    /// The row merged into the built-in one.
    Folded,
    /// The row had folded already.
    AlreadyFolded {
        /// Rows that landed on it since and moved on.
        moved: u64,
        /// Links, source links and credits that stay on it.
        left: u64,
    },
    /// The row stays apart.
    Kept(V2PlaceholderKept),
}

/// Fold v2's placeholder row `retiree` into the built-in Unknown Artist
/// ([`MergeBasis::v2_placeholder`]), or move on what a later import landed
/// on a row that already folded ([`retarget_retired`]), with no second audit
/// row. Repeating it writes nothing. A failure is rolled back and answered
/// as a row kept.
pub fn fold_v2_placeholder(
    tx: &Transaction<'_>,
    retiree: &str,
    now: f64,
) -> rusqlite::Result<V2Fold> {
    let retired_into: Option<String> = tx
        .prepare_cached(crate::corrections::sql::ARTIST_RETIRED_INTO.text())?
        .query_row(params![retiree], |row| row.get(0))
        .optional()?
        .flatten();
    let (outcome, failed) = if retired_into.as_deref() == Some(UNKNOWN_ARTIST_ID) {
        let late = savepoint(tx, "artist_v2_placeholder", || late_rows(tx, retiree, now))?;
        (late, V2PlaceholderKept::LateRowsFailed)
    } else {
        let basis = match MergeBasis::v2_placeholder(tx, retiree)? {
            Ok(basis) => basis,
            Err(kept) => return Ok(V2Fold::Kept(kept)),
        };
        let merged = savepoint(tx, "artist_v2_placeholder", || {
            merge_v2_placeholder(tx, basis, now).map(|()| V2Fold::Folded)
        })?;
        (merged, V2PlaceholderKept::MergeFailed)
    };
    Ok(outcome.unwrap_or_else(|error: rusqlite::Error| {
        tracing::warn!(
            %error,
            retired_artist_id = retiree,
            "v2 Unknown Artist row could not be merged; it stays as it is"
        );
        V2Fold::Kept(failed)
    }))
}

fn late_rows(tx: &Transaction<'_>, retiree: &str, now: f64) -> rusqlite::Result<V2Fold> {
    let moved = retarget_retired(tx, retiree, UNKNOWN_ARTIST_ID, UNKNOWN_ARTIST, now)?;
    if moved > 0 {
        touch_built_in(tx, now)?;
    }
    let left = tx
        .prepare_cached(crate::corrections::sql::LEFT_ON_RETIRED.text())?
        .query_row(params![retiree], |row| row.get(0))?;
    Ok(V2Fold::AlreadyFolded { moved, left })
}

/// Retire the row and write the merge's one audit row. Rows v2 merged into
/// it, and their aliases, now answer through an alias on the built-in row,
/// which is all that opens them once it is retired too.
fn merge_v2_placeholder(
    tx: &Transaction<'_>,
    basis: MergeBasis<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    let retiree = basis.retiree().to_owned();
    let audit = basis.audit();
    retire_artist(tx, basis, now)?;
    tx.execute(
        crate::corrections::sql::REPOINT_ALIASES_OF_ROWS_MERGED_INTO.text(),
        params![retiree, UNKNOWN_ARTIST_ID],
    )?;
    tx.execute(
        crate::corrections::sql::ALIAS_ROWS_MERGED_INTO.text(),
        params![
            retiree,
            UNKNOWN_ARTIST_ID,
            now,
            ArtistAliasKind::MergedArtist
        ],
    )?;
    touch_built_in(tx, now)?;
    record_artist_action(
        tx,
        ArtistAction {
            actor: "system",
            kind: ActionKind::MergeArtist,
            artist_id: UNKNOWN_ARTIST_ID,
            before: serde_json::json!({ "artist_ids": [retiree] }),
            after: serde_json::json!({
                "kind": "v2_placeholder_fold",
                "surviving_artist_id": UNKNOWN_ARTIST_ID,
                "retired_artist_ids": [retiree],
                "filesystem_writes": 0,
                "basis": audit,
            }),
            reason: V2_PLACEHOLDER_FOLDED,
        },
        now,
    )?;
    Ok(())
}

fn touch_built_in(tx: &Transaction<'_>, now: f64) -> rusqlite::Result<()> {
    tx.execute(
        crate::corrections::sql::BUMP_ARTIST_REVISION.text(),
        params![UNKNOWN_ARTIST_ID, crate::clock::ms(now)],
    )?;
    bump_catalog(tx)?;
    Ok(())
}
