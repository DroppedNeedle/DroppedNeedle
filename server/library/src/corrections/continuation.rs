//! A missing track continues into the track that took its place.
//!
//! A file retagged and renamed in one go comes back as a new track row,
//! and a deleted copy of a track leaves its row missing beside the copy
//! that stays. When the app can prove the new row is the same track of the
//! same edition, what people made on the missing row moves to it and the
//! old id becomes an alias that opens it.
//!
//! The proof, for a missing row O and an indexed row N:
//! - both sit on the same album row;
//! - the album's match names a release R, the album has no match state (no
//!   best guess, no unplaced or failed choice) and no custom edition; any
//!   decision source counts, a person's or the app's;
//! - O and N are mapped to the same release track X, each naming R or no
//!   release (approvals and imports record only the release track); a row
//!   on X naming another release stops X altogether;
//! - N is the only indexed row on X. Every missing row on X that is not an
//!   alias yet continues into N. Excluded rows are neither side.
//!
//! The release proof weighs nothing else: not names, recordings, lengths,
//! paths or ages.
//!
//! A file moved to another folder whose new folder a scan read before its
//! old one comes back as a new row N, and a later scan marks the old row O
//! missing. At a run end the scan continues O into N ([`continue_moved`])
//! when N is the file it would have continued O into had it read N after
//! O went (same album names, disc and track number, and the same
//! recording id or tagged title and length), the match is unambiguous both
//! ways and the run history shows a move, not two copies.
//!
//! No track row and no file is deleted, moved or read. The move has no
//! undo, but its audit row keeps every row it moved or deleted.
//!
//! A track a person had replaced by an edition's version of it (another
//! recording, see [`continue_replaced`]) continues into its replacement
//! without that proof, as the import that replaced it names the pair. Its
//! field overrides described its own recording, so they stay on it; the
//! rest moves.

pub use crate::corrections::sql::CURRENT_TRACK;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use crate::catalog_revision::bump_catalog;
use crate::identify::sqlite::retarget_references;
use crate::operations::decisions::{CatalogAction, record_action};
use droppedneedle_kernel::db::savepoint;

/// Audit kind and reason of a continuation.
const KIND: &str = "continue_tracks";
const REASON: &str = "TRACK_CONTINUED";
/// The basis an audit row gives for a pair the scan proved itself: two
/// rows for one file on disk, or one file moved to another folder.
pub(crate) const SAME_FILE: &str = "same_file";
const MOVED_FILE: &str = "moved_file";

/// Which albums a pass looks at for new pairs. Aliases are kept current
/// and swept over the whole library whatever the scope.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope<'a> {
    /// One album.
    Album(&'a str),
    /// Every album.
    Library,
}

use crate::corrections::sql::CANDIDATE_ALBUMS;

use crate::corrections::sql::HOLDING_ALIASES;

/// How many aliases a pass inside another write moves, besides the pairs
/// that write itself made on its album: each alias costs about half a
/// millisecond on a large library, so a backlog never pushes a request
/// or job write past its time budget. [`drain_continuations`] moves the
/// rest.
pub(crate) const WRITE_BOUND: usize = 200;

/// How many aliases one turn of [`drain_continuations`] moves.
pub(crate) const DRAIN_BOUND: usize = 500;

/// Find new pairs, keep aliases current and move what people made from
/// aliases to the tracks they name, in a savepoint of the caller's
/// transaction. On one album, every pair the pass makes there moves in
/// full; beyond those, at most [`WRITE_BOUND`] aliases move, and what is
/// left waits for [`drain_continuations`]. A store error rolls the
/// savepoint back with one warning and leaves the caller's work to
/// commit; the next pass tries again. Call it after the transaction's
/// last identity, match-state and custom-edition write.
pub(crate) fn continue_tracks(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    run(tx, scope, &[], None, true, WRITE_BOUND, now).map(|_| ())
}

/// One turn of working off aliases that still hold what people made:
/// up to `limit` of them move, each in full, and no new pair is looked
/// for. `true` while more wait. A store error rolls the turn back with
/// one warning and answers `false`; the next turn tries again.
pub(crate) fn drain_continuations(
    tx: &Transaction<'_>,
    limit: usize,
    now: f64,
) -> rusqlite::Result<bool> {
    run(tx, Scope::Library, &[], None, false, limit, now)
}

/// [`continue_tracks`] on one album, after an import whose files replaced
/// tracks of it as other versions: each `(old, new)` pair continues
/// without the proof, its field overrides staying on the old row. Only
/// when the two rows differ, both are on `album`, the old one is missing
/// and not an alias yet, and the new one is indexed; otherwise nothing is
/// written for that pair. The aliases are written in `tx`, so a store error
/// there fails the import that names them. Only the move is in the
/// savepoint: when it fails, the aliases stay and
/// [`drain_continuations`] moves what is on the old rows later.
pub(crate) fn continue_replaced(
    tx: &Transaction<'_>,
    album: &str,
    replaced: &[(String, String)],
    now: f64,
) -> rusqlite::Result<()> {
    let created_ms = (now * 1000.0) as i64;
    let mut made = Vec::new();
    for (old, new) in replaced {
        let fits: bool = tx.query_row(
            crate::corrections::sql::CONTINUE_REPLACED.text(),
            params![old, new, album],
            |row| row.get(0),
        )?;
        if old != new && fits {
            add_alias(tx, old, new, false, created_ms)?;
            made.push((old.clone(), new.clone()));
        }
    }
    run(tx, Scope::Album(album), &made, None, true, WRITE_BOUND, now).map(|_| ())
}

/// Continue each `(old, new)` pair of rows for one file that two library
/// folders reach, whose real paths overlap: the scan marked `old` missing
/// because the file is indexed as `new` under the other folder. No proof
/// beyond that is needed, as both rows name the same file on disk. Only
/// when the two rows differ, the old one is missing and not an alias yet,
/// and the new one is indexed; otherwise nothing is written for that
/// pair. What people made on `old` then moves as for any continuation.
pub(crate) fn continue_same_files(
    tx: &Transaction<'_>,
    pairs: &[(String, String)],
    now: f64,
) -> rusqlite::Result<()> {
    continue_given(tx, "continue_same_files", SAME_FILE, pairs, now)
}

/// Continue each `(old, new)` pair the scan proved to be one file moved to
/// another folder, the new folder read first. The fit and the move are
/// those of [`continue_same_files`].
pub(crate) fn continue_moved(
    tx: &Transaction<'_>,
    pairs: &[(String, String)],
    now: f64,
) -> rusqlite::Result<()> {
    continue_given(tx, "continue_moved", MOVED_FILE, pairs, now)
}

fn continue_given(
    tx: &Transaction<'_>,
    name: &'static str,
    basis: &'static str,
    pairs: &[(String, String)],
    now: f64,
) -> rusqlite::Result<()> {
    let created_ms = (now * 1000.0) as i64;
    let made = savepoint(tx, name, || {
        let mut made = BTreeSet::new();
        for (old, new) in pairs {
            let fits: bool = tx.query_row(
                crate::corrections::sql::CONTINUE_GIVEN.text(),
                params![old, new],
                |row| row.get(0),
            )?;
            if old != new && fits {
                add_alias(tx, old, new, true, created_ms)?;
                made.insert((old.clone(), new.clone()));
            }
        }
        Ok::<_, rusqlite::Error>(made)
    })??;
    run(
        tx,
        Scope::Library,
        &[],
        Some(Proven { pairs: made, basis }),
        false,
        WRITE_BOUND,
        now,
    )
    .map(|_| ())
}

/// Make `old` an alias of `new`, and move every alias naming `old` on to
/// `new`: no alias ever names an alias.
fn add_alias(
    tx: &Transaction<'_>,
    old: &str,
    new: &str,
    moves_overrides: bool,
    created_ms: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        crate::corrections::sql::ADD_TRACK_ALIAS.text(),
        params![old, new, created_ms, moves_overrides],
    )?;
    tx.execute(
        crate::corrections::sql::REPOINT_TRACK_ALIASES.text(),
        params![old, new, moves_overrides],
    )?;
    Ok(())
}

/// One pass in a savepoint. `made` holds the replaced pairs the caller
/// has just made aliases of; `discover` looks for new pairs; `limit` bounds
/// the aliases moved beyond the pairs made on the pass's album. `true` when
/// aliases that hold something are left.
fn run(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    made: &[(String, String)],
    proven: Option<Proven>,
    discover: bool,
    limit: usize,
    now: f64,
) -> rusqlite::Result<bool> {
    let mut at = Context::default();
    match savepoint(tx, "continue_tracks", || {
        pass(
            tx,
            scope,
            made,
            proven.as_ref(),
            discover,
            limit,
            now,
            &mut at,
        )
    })? {
        Ok(more) => Ok(more),
        Err(error) => {
            tracing::warn!(
                %error,
                album = at.album.as_deref().unwrap_or(""),
                old_track = at.old.as_deref().unwrap_or(""),
                new_track = at.new.as_deref().unwrap_or(""),
                "tracks not continued; the next pass tries again"
            );
            Ok(false)
        }
    }
}

/// Pairs the scan already aliased on its own proof, for the audit.
struct Proven {
    pairs: BTreeSet<(String, String)>,
    basis: &'static str,
}

/// Where a pass was when it failed, for the warning.
#[derive(Default)]
struct Context {
    album: Option<String>,
    old: Option<String>,
    new: Option<String>,
}

/// One album's audit entry: its pairs, new or swept, and what each moved.
#[derive(Default)]
struct AlbumLog {
    pairs: Vec<Value>,
    new_pairs: usize,
    moved: BTreeMap<&'static str, usize>,
}

#[allow(clippy::too_many_arguments)]
fn pass(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    made: &[(String, String)],
    proven: Option<&Proven>,
    discover: bool,
    limit: usize,
    now: f64,
    at: &mut Context,
) -> rusqlite::Result<bool> {
    // An old row whose file is back answers for itself again. What moved
    // stays where it went.
    tx.execute(crate::corrections::sql::DROP_LIVE_TRACK_ALIASES.text(), [])?;
    let only = match scope {
        Scope::Album(album) => Some(album),
        Scope::Library => None,
    };
    let mut new_pairs: BTreeSet<(String, String)> = proven
        .map(|proven| proven.pairs.clone())
        .unwrap_or_default();
    new_pairs.extend(made.iter().cloned());
    let replaced: BTreeSet<String> = made.iter().map(|(old, _)| old.clone()).collect();
    let created_ms = (now * 1000.0) as i64;
    // Every alias naming the old row moves on to the new one: no alias ever
    // names an alias.
    let alias = |old: &str, new: &str, moves_overrides: bool| {
        add_alias(tx, old, new, moves_overrides, created_ms)
    };
    let albums: Vec<String> = if discover {
        tx.prepare_cached(CANDIDATE_ALBUMS.text())?
            .query_map(params![only], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    for album in albums {
        at.album = Some(album.clone());
        for (old, new) in pairs_on(tx, &album)? {
            at.old = Some(old.clone());
            at.new = Some(new.clone());
            alias(&old, &new, true)?;
            new_pairs.insert((old, new));
        }
    }

    let holding: BTreeSet<String> = tx
        .prepare_cached(HOLDING_ALIASES.text())?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // The pairs made on the pass's own album move in full, so the write
    // that made them is complete at commit. Then up to `limit` others, in
    // id order.
    let own: Vec<&String> = match only {
        Some(_) => holding
            .iter()
            .filter(|old| new_pairs.iter().any(|(made, _)| made == *old))
            .collect(),
        None => Vec::new(),
    };
    let moving: Vec<String> = own
        .iter()
        .map(|old| (*old).clone())
        .chain(
            holding
                .iter()
                .filter(|old| !own.contains(old))
                .take(limit)
                .cloned(),
        )
        .collect();
    let more = holding.len() > moving.len();
    let pair_json = |old: &str, new: &str, is_new: bool| {
        let mut pair = json!({ "old_track_id": old, "new_track_id": new, "new_pair": is_new });
        if replaced.contains(old)
            && let Some(fields) = pair.as_object_mut()
        {
            fields.insert("replaced".to_owned(), Value::Bool(true));
        }
        if let Some(proven) = proven
            && proven.pairs.contains(&(old.to_owned(), new.to_owned()))
            && let Some(fields) = pair.as_object_mut()
        {
            fields.insert("basis".to_owned(), Value::from(proven.basis));
        }
        pair
    };
    let mut logs: BTreeMap<String, AlbumLog> = BTreeMap::new();
    for (old, new) in &new_pairs {
        if !moving.contains(old) {
            let album = album_of(tx, old)?;
            let log = logs.entry(album).or_default();
            log.new_pairs += 1;
            log.pairs.push(pair_json(old, new, true));
        }
    }
    for old in &moving {
        let (new, moves_overrides): (String, bool) = tx.query_row(
            crate::corrections::sql::TRACK_ALIAS_TARGET.text(),
            params![old],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        at.old = Some(old.clone());
        at.new = Some(new.clone());
        let album = album_of(tx, old)?;
        at.album = Some(album.clone());
        let log = logs.entry(album).or_default();
        let is_new = new_pairs.contains(&(old.clone(), new.clone()));
        if is_new {
            log.new_pairs += 1;
        }
        let entry = sweep(tx, old, &new, moves_overrides, now, &mut log.moved)?;
        let mut pair = pair_json(old, &new, is_new);
        if let (Some(target), Value::Object(fields)) = (pair.as_object_mut(), entry) {
            target.extend(fields);
        }
        log.pairs.push(pair);
    }

    let mut changed = false;
    for (album, log) in logs {
        let moved: usize = log.moved.values().sum();
        if log.new_pairs == 0 && moved == 0 {
            continue;
        }
        changed = true;
        at.album = Some(album.clone());
        record_action(
            tx,
            CatalogAction {
                actor: "system",
                kind: KIND,
                album_id: &album,
                job_id: None,
                before: json!({}),
                after: json!({
                    "pairs": log.pairs,
                    "new_pairs": log.new_pairs,
                    "moved": log.moved,
                    "filesystem_writes": 0,
                }),
                reason: REASON,
            },
            now,
        )?;
        tracing::info!(
            album,
            new_pairs = log.new_pairs,
            moved,
            "missing tracks continued into the tracks that took their place"
        );
    }
    if changed {
        bump_catalog(tx)?;
    }
    Ok(more)
}

/// The `(old, new)` pairs the rule proves on one album.
fn pairs_on(conn: &Connection, album: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let Some(release) = accepted_release(conn, album)? else {
        return Ok(Vec::new());
    };
    Ok(pairs_among(conn, &[album], &release, false)?
        .into_iter()
        .map(|pair| (pair.old, pair.new))
        .collect())
}

/// The release an album is matched to, lowercased, when the match is an
/// accepted exact one: it names a release, and the album has no match
/// state (no best guess, no unplaced or failed choice) and no custom
/// edition. Any decision source counts.
pub(crate) fn accepted_release(conn: &Connection, album: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(crate::corrections::sql::ACCEPTED_RELEASE.text())?
        .query_row(params![album], |row| row.get(0))
        .optional()
}

/// A missing row and the indexed row the rule pairs it with, with the
/// album rows each sits on.
pub(crate) struct Pair {
    pub(crate) old: String,
    pub(crate) old_album: String,
    pub(crate) new: String,
    pub(crate) new_album: String,
}

/// The pairs the rule proves over the mapped rows of `albums` taken
/// together, each album matched to `release` (lowercased): per release
/// track, unless a row on it names another release, every missing row
/// pairs with the one indexed row, when there is exactly one. Aliases and
/// excluded rows are neither side, except that with `aliased` a missing
/// row whose alias names that one indexed row pairs with it too.
pub(crate) fn pairs_among(
    conn: &Connection,
    albums: &[&str],
    release: &str,
    aliased: bool,
) -> rusqlite::Result<Vec<Pair>> {
    struct Row {
        id: String,
        album: String,
        indexed: bool,
        missing: bool,
        release: Option<String>,
        alias_of: Option<String>,
    }
    let albums = serde_json::to_string(albums)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let mut by_track: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut statement = conn.prepare_cached(crate::corrections::sql::PAIRS_AMONG.text())?;
    let rows = statement.query_map(params![albums], |row| {
        let availability: String = row.get(2)?;
        Ok((
            row.get::<_, String>(3)?,
            Row {
                id: row.get(0)?,
                album: row.get(1)?,
                indexed: availability == "indexed",
                missing: availability == "missing",
                release: row.get(4)?,
                alias_of: row.get(5)?,
            },
        ))
    })?;
    for row in rows {
        let (track, row) = row?;
        by_track.entry(track).or_default().push(row);
    }
    let mut pairs = Vec::new();
    for rows in by_track.values() {
        let rows: Vec<&Row> = rows
            .iter()
            .filter(|row| (row.indexed || row.missing) && (aliased || row.alias_of.is_none()))
            .collect();
        if rows
            .iter()
            .any(|row| row.release.as_deref().is_some_and(|named| named != release))
        {
            continue;
        }
        let indexed: Vec<&&Row> = rows.iter().filter(|row| row.indexed).collect();
        let [new] = indexed.as_slice() else {
            continue;
        };
        let pairs_with_new =
            |row: &&&Row| row.alias_of.as_ref().is_none_or(|target| *target == new.id);
        pairs.extend(
            rows.iter()
                .filter(|row| row.missing)
                .filter(pairs_with_new)
                .map(|row| Pair {
                    old: row.id.clone(),
                    old_album: row.album.clone(),
                    new: new.id.clone(),
                    new_album: new.album.clone(),
                }),
        );
    }
    Ok(pairs)
}

fn album_of(conn: &Connection, track: &str) -> rusqlite::Result<String> {
    conn.query_row(
        crate::corrections::sql::ALBUM_OF.text(),
        params![track],
        |row| row.get(0),
    )
}

/// Move what people made from `old` to `new`, counting into `moved`; its
/// track field overrides only with `moves_overrides`. Returns, for the
/// audit row, every row moved and, verbatim, every row deleted.
fn sweep(
    tx: &Transaction<'_>,
    old: &str,
    new: &str,
    moves_overrides: bool,
    now: f64,
    moved: &mut BTreeMap<&'static str, usize>,
) -> rusqlite::Result<Value> {
    let plays = strings(
        tx,
        crate::corrections::sql::PLAYS_OF_TRACK.text(),
        params![old],
    )?;
    // A star the user already has on the new row keeps its date; the old
    // one is deleted, kept here with its date and name.
    let stars_moved = strings(
        tx,
        crate::corrections::sql::FAVORITES_TO_MOVE.text(),
        params![old, new],
    )?;
    let stars_deleted: Vec<Value> = tx
        .prepare(crate::corrections::sql::FAVORITES_IN_BOTH.text())?
        .query_map(params![old, new], |row| {
            Ok(json!({
                "user_id": row.get::<_, String>(0)?,
                "created_at": row.get::<_, f64>(1)?,
                "display_name": row.get::<_, Option<String>>(2)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // A star the user already has on the new row with no name takes the
    // old star's name.
    let names_copied: Vec<Value> = tx
        .prepare(crate::corrections::sql::FAVORITE_NAMES_IN_BOTH.text())?
        .query_map(params![old, new], |row| {
            Ok(json!({
                "user_id": row.get::<_, String>(0)?,
                "display_name": row.get::<_, String>(1)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // A rating the user already has on the new row stays; the old one is
    // deleted, kept here as it was.
    let ratings_moved = strings(
        tx,
        crate::corrections::sql::RATINGS_TO_MOVE.text(),
        params![old, new],
    )?;
    let ratings_deleted: Vec<Value> = tx
        .prepare(crate::corrections::sql::RATINGS_IN_BOTH.text())?
        .query_map(params![old, new], |row| {
            Ok(json!({
                "user_id": row.get::<_, String>(0)?,
                "rating": row.get::<_, i64>(1)?,
                "rated_at": row.get::<_, i64>(2)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    retarget_references(tx, "track", old, new)?;

    let entries = strings(
        tx,
        crate::corrections::sql::PLAYLIST_ROWS_OF_TRACK.text(),
        params![old],
    )?;
    tx.execute(
        crate::corrections::sql::REPOINT_PLAYLIST_ROWS.text(),
        params![old, new],
    )?;

    // Where the user has a bookmark on both rows, the later one stays.
    let mut bookmarks_moved = Vec::new();
    let mut bookmarks_deleted = Vec::new();
    let bookmarks: Vec<(String, f64, Option<f64>)> = tx
        .prepare(crate::corrections::sql::BOOKMARKS_OF_TRACK.text())?
        .query_map(params![old, new], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (user, old_changed, new_changed) in bookmarks {
        let loser = match new_changed {
            Some(theirs) if theirs >= old_changed => Some(old),
            Some(_) => Some(new),
            None => None,
        };
        if let Some(loser) = loser {
            bookmarks_deleted.push(tx.query_row(
                crate::corrections::sql::BOOKMARK_ROW.text(),
                params![user, loser],
                |row| {
                    Ok(json!({
                        "user_id": user,
                        "file_id": row.get::<_, String>(0)?,
                        "position_ms": row.get::<_, i64>(1)?,
                        "comment": row.get::<_, String>(2)?,
                        "created_at": row.get::<_, f64>(3)?,
                        "changed_at": row.get::<_, f64>(4)?,
                    }))
                },
            )?);
            tx.execute(
                crate::corrections::sql::DROP_BOOKMARK.text(),
                params![user, loser],
            )?;
        }
        if loser != Some(old) {
            tx.execute(
                crate::corrections::sql::REPOINT_BOOKMARK.text(),
                params![user, old, new],
            )?;
            bookmarks_moved.push(user);
        }
    }

    // Where the user has played marks on both rows, the two merge on the
    // new row: the later of each time, and no played mark older than the
    // unplayed one, so plays the user hid stay hidden. The old row goes;
    // the audit keeps it and the new row as it was.
    let mut marks_moved = Vec::new();
    let mut marks_merged = Vec::new();
    let mut marks_deleted = Vec::new();
    type Mark = (Option<i64>, Option<i64>);
    let marks: Vec<(String, Mark, Option<Mark>)> = tx
        .prepare(crate::corrections::sql::PLAY_MARKS_OF_TRACK.text())?
        .query_map(params![old, new], |row| {
            let theirs: bool = row.get(3)?;
            Ok((
                row.get(0)?,
                (row.get(1)?, row.get(2)?),
                theirs
                    .then(|| Ok::<_, rusqlite::Error>((row.get(4)?, row.get(5)?)))
                    .transpose()?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mark_json = |user: &str, track: &str, (played, unplayed): Mark| {
        json!({
            "user_id": user,
            "local_track_id": track,
            "marked_played_at": played,
            "marked_unplayed_at": unplayed,
        })
    };
    for (user, mine, theirs) in marks {
        let Some(theirs) = theirs else {
            tx.execute(
                crate::corrections::sql::REPOINT_PLAY_MARK.text(),
                params![user, old, new],
            )?;
            marks_moved.push(user);
            continue;
        };
        let unplayed = mine.1.max(theirs.1);
        let played = mine
            .0
            .max(theirs.0)
            .filter(|at| unplayed.is_none_or(|cut| *at >= cut));
        tx.execute(
            crate::corrections::sql::WRITE_PLAY_MARK.text(),
            params![user, new, played, unplayed],
        )?;
        tx.execute(
            crate::corrections::sql::DROP_PLAY_MARK.text(),
            params![user, old],
        )?;
        marks_merged.push(mark_json(&user, new, theirs));
        marks_deleted.push(mark_json(&user, old, mine));
    }

    let queue: Vec<Value> = tx
        .prepare(crate::corrections::sql::QUEUE_ITEMS_OF_TRACK.text())?
        .query_map(params![old], |row| {
            Ok(json!({
                "user_id": row.get::<_, String>(0)?,
                "item_index": row.get::<_, i64>(1)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute(
        crate::corrections::sql::REPOINT_QUEUE_ITEMS.text(),
        params![old, new],
    )?;

    // The new row's own override for a field wins; the old one stays on
    // the old row.
    let overrides: Vec<(String, f64, i64)> = if moves_overrides {
        tx.prepare(crate::corrections::sql::OVERRIDES_TO_MOVE.text())?
            .query_map(params![old, new], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    for (id, _, _) in &overrides {
        tx.execute(
            crate::corrections::sql::REPOINT_OVERRIDE.text(),
            params![id, new, now],
        )?;
    }

    let mut count = |table: &'static str, n: usize| {
        if n > 0 {
            *moved.entry(table).or_default() += n;
        }
    };
    count("library_play_history", plays.len());
    count(
        "library_user_favorites",
        stars_moved.len() + stars_deleted.len(),
    );
    count(
        "library_user_ratings",
        ratings_moved.len() + ratings_deleted.len(),
    );
    count("playlist_tracks", entries.len());
    count(
        "compat_bookmarks",
        bookmarks_moved.len() + bookmarks_deleted.len(),
    );
    count(
        "library_user_play_marks",
        marks_moved.len() + marks_deleted.len(),
    );
    count("compat_play_queue_items", queue.len());
    count("library_management_overrides", overrides.len());
    Ok(json!({
        "moved": {
            "play_history_ids": plays,
            "star_user_ids": stars_moved,
            "rating_user_ids": ratings_moved,
            "playlist_entry_ids": entries,
            "bookmark_user_ids": bookmarks_moved,
            "play_mark_user_ids": marks_moved,
            "play_marks_merged_into": marks_merged,
            "queue_entries": queue,
            "overrides": overrides
                .iter()
                .map(|(id, updated_at, row_revision)| json!({
                    "id": id,
                    "updated_at": updated_at,
                    "row_revision": row_revision,
                }))
                .collect::<Vec<_>>(),
            "star_names_copied": names_copied,
        },
        "deleted": {
            "stars": stars_deleted,
            "ratings": ratings_deleted,
            "bookmarks": bookmarks_deleted,
            "play_marks": marks_deleted,
        },
    }))
}

/// The first column of every row a query answers.
fn strings(
    conn: &Connection,
    sql: &str,
    values: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<String>> {
    conn.prepare(sql)?
        .query_map(values, |row| row.get(0))?
        .collect()
}
