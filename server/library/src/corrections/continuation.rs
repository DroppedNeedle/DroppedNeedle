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
//! Nothing else is weighed: not names, recordings, lengths, paths or ages.
//! No track row and no file is deleted, moved or read. The move has no
//! undo, but its audit row keeps every row it moved or deleted.
//!
//! A track a person had replaced by an edition's version of it (another
//! recording, see [`continue_replaced`]) continues into its replacement
//! without that proof, as the import that replaced it names the pair. Its
//! field overrides described its own recording, so they stay on it; the
//! rest moves.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use crate::catalog_revision::bump_catalog;
use crate::identify::sqlite::retarget_references;
use crate::operations::decisions::{CatalogAction, record_action};
use droppedneedle_kernel::db::savepoint;

/// The track a local id names now: the track itself while its file is
/// indexed, else the track its alias names (a missing track that
/// continued into another), else the id itself, so a read's own
/// availability test answers for it as for any id.
pub const CURRENT_TRACK: &str = "SELECT id FROM ( \
    SELECT id, 0 AS rank FROM local_tracks WHERE id = ?1 AND availability = 'indexed' \
    UNION ALL SELECT local_track_id, 1 FROM local_track_aliases WHERE alias = ?1 \
    UNION ALL SELECT ?1, 2) \
    ORDER BY rank LIMIT 1";

/// Audit kind and reason of a continuation.
const KIND: &str = "continue_tracks";
const REASON: &str = "TRACK_CONTINUED";

/// Which albums a pass looks at for new pairs. Aliases are kept current
/// and swept over the whole library whatever the scope.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope<'a> {
    /// One album.
    Album(&'a str),
    /// Every album.
    Library,
}

/// Albums with a missing, unaliased row mapped to a release track that an
/// indexed row of the same album is also mapped to. Starts from the
/// missing rows and reads each album's indexed rows by album.
const CANDIDATE_ALBUMS: &str = "SELECT DISTINCT o.local_album_id FROM local_tracks o \
    JOIN local_track_external_identities oi \
      ON oi.local_track_id = o.id AND oi.provider = 'musicbrainz' \
    WHERE o.availability = 'missing' AND oi.release_track_mbid IS NOT NULL \
    AND (?1 IS NULL OR o.local_album_id = ?1) \
    AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = o.id) \
    AND EXISTS (SELECT 1 FROM local_tracks n \
      JOIN local_track_external_identities ni \
        ON ni.local_track_id = n.id AND ni.provider = 'musicbrainz' \
      WHERE n.local_album_id = o.local_album_id AND n.availability = 'indexed' \
      AND lower(ni.release_track_mbid) = lower(oi.release_track_mbid))";

/// Aliases whose own row is not indexed and that still hold something a
/// person made: one statement per table. A track override that the
/// target already has for its field, or on an alias whose overrides stay
/// (a replaced version), stays on the old row and does not count.
const HOLDING_ALIASES: &str = "WITH dead AS (SELECT x.alias FROM local_track_aliases x \
      JOIN local_tracks t ON t.id = x.alias WHERE t.availability <> 'indexed') \
    SELECT local_track_id FROM library_play_history WHERE local_track_id IN dead \
    UNION SELECT item_id FROM library_user_favorites \
      WHERE item_kind = 'track' AND item_id IN dead \
    UNION SELECT item_id FROM library_user_ratings \
      WHERE item_kind = 'track' AND item_id IN dead \
    UNION SELECT library_file_id FROM playlist_tracks WHERE library_file_id IN dead \
    UNION SELECT file_id FROM compat_bookmarks WHERE file_id IN dead \
    UNION SELECT local_track_id FROM library_user_play_marks WHERE local_track_id IN dead \
    UNION SELECT file_id FROM compat_play_queue_items WHERE file_id IN dead \
    UNION SELECT o.local_track_id FROM library_management_overrides o \
      JOIN local_track_aliases x ON x.alias = o.local_track_id \
      WHERE o.subject_kind = 'track' AND o.local_track_id IN dead \
      AND x.moves_overrides = 1 AND NOT EXISTS (SELECT 1 FROM library_management_overrides w \
        WHERE w.subject_kind = 'track' AND w.local_track_id = x.local_track_id \
        AND w.field_name = o.field_name)";

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
    run(tx, scope, &[], true, WRITE_BOUND, now).map(|_| ())
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
    run(tx, Scope::Library, &[], false, limit, now)
}

/// [`continue_tracks`] on one album, after an import whose files replaced
/// tracks of it as other versions: each `(old, new)` pair continues
/// without the proof, its field overrides staying on the old row. Only
/// when the two rows differ, both are on `album`, the old one is missing
/// and not an alias yet, and the new one is indexed; otherwise nothing is
/// written for that pair.
pub(crate) fn continue_replaced(
    tx: &Transaction<'_>,
    album: &str,
    replaced: &[(String, String)],
    now: f64,
) -> rusqlite::Result<()> {
    run(tx, Scope::Album(album), replaced, true, WRITE_BOUND, now).map(|_| ())
}

/// One pass in a savepoint. `discover` looks for new pairs; `limit`
/// bounds the aliases moved beyond the pairs made on the pass's album.
/// `true` when aliases that hold something are left.
fn run(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    replaced: &[(String, String)],
    discover: bool,
    limit: usize,
    now: f64,
) -> rusqlite::Result<bool> {
    let mut at = Context::default();
    match savepoint(tx, "continue_tracks", || {
        pass(tx, scope, replaced, discover, limit, now, &mut at)
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

fn pass(
    tx: &Transaction<'_>,
    scope: Scope<'_>,
    given: &[(String, String)],
    discover: bool,
    limit: usize,
    now: f64,
    at: &mut Context,
) -> rusqlite::Result<bool> {
    // An old row whose file is back answers for itself again. What moved
    // stays where it went.
    tx.execute(
        "DELETE FROM local_track_aliases WHERE EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.id = local_track_aliases.alias AND t.availability = 'indexed')",
        [],
    )?;
    let only = match scope {
        Scope::Album(album) => Some(album),
        Scope::Library => None,
    };
    let mut new_pairs: BTreeSet<(String, String)> = BTreeSet::new();
    let mut replaced: BTreeSet<String> = BTreeSet::new();
    let created_ms = (now * 1000.0) as i64;
    // The alias, and every alias naming the old row moved on to the new
    // one: no alias ever names an alias. A replaced version's overrides
    // stay, and so do those of the rows that continued into it.
    let alias = |old: &str, new: &str, moves_overrides: bool| -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO local_track_aliases (alias, local_track_id, created_at, \
             moves_overrides) VALUES (?1, ?2, ?3, ?4)",
            params![old, new, created_ms, moves_overrides],
        )?;
        tx.execute(
            "UPDATE local_track_aliases SET local_track_id = ?2, \
             moves_overrides = moves_overrides AND ?3 WHERE local_track_id = ?1",
            params![old, new, moves_overrides],
        )?;
        Ok(())
    };
    if let Some(album) = only {
        for (old, new) in given {
            at.album = Some(album.to_owned());
            at.old = Some(old.clone());
            at.new = Some(new.clone());
            let fits: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_tracks o JOIN local_tracks n \
                 ON n.id = ?2 AND n.local_album_id = o.local_album_id \
                 AND n.availability = 'indexed' \
                 WHERE o.id = ?1 AND o.local_album_id = ?3 AND o.availability = 'missing' \
                 AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = o.id))",
                params![old, new, album],
                |row| row.get(0),
            )?;
            if old == new || !fits {
                continue;
            }
            alias(old, new, false)?;
            replaced.insert(old.clone());
            new_pairs.insert((old.clone(), new.clone()));
        }
    }
    let albums: Vec<String> = if discover {
        tx.prepare_cached(CANDIDATE_ALBUMS)?
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
        .prepare_cached(HOLDING_ALIASES)?
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
            "SELECT local_track_id, moves_overrides FROM local_track_aliases WHERE alias = ?1",
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
    Ok(pairs_among(conn, &[album], &release)?
        .into_iter()
        .map(|pair| (pair.old, pair.new))
        .collect())
}

/// The release an album is matched to, lowercased, when the match is an
/// accepted exact one: it names a release, and the album has no match
/// state (no best guess, no unplaced or failed choice) and no custom
/// edition. Any decision source counts.
pub(crate) fn accepted_release(conn: &Connection, album: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT lower(trim(e.release_mbid)) FROM local_album_external_identities e \
         WHERE e.local_album_id = ?1 AND e.provider = 'musicbrainz' \
         AND trim(COALESCE(e.release_mbid, '')) <> '' \
         AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
           WHERE s.local_album_id = e.local_album_id) \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
           WHERE c.local_album_id = e.local_album_id)",
    )?
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
/// excluded rows are neither side.
pub(crate) fn pairs_among(
    conn: &Connection,
    albums: &[&str],
    release: &str,
) -> rusqlite::Result<Vec<Pair>> {
    struct Row {
        id: String,
        album: String,
        indexed: bool,
        missing: bool,
        release: Option<String>,
    }
    let albums = serde_json::to_string(albums)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let mut by_track: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    let mut statement = conn.prepare_cached(
        "SELECT t.id, t.local_album_id, t.availability, lower(trim(i.release_track_mbid)), \
         lower(NULLIF(trim(i.release_mbid), '')) FROM local_tracks t \
         JOIN local_track_external_identities i \
           ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
         WHERE t.local_album_id IN (SELECT value FROM json_each(?1)) \
         AND trim(COALESCE(i.release_track_mbid, '')) <> '' \
         AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = t.id)",
    )?;
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
            .filter(|row| row.indexed || row.missing)
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
        pairs.extend(rows.iter().filter(|row| row.missing).map(|row| Pair {
            old: row.id.clone(),
            old_album: row.album.clone(),
            new: new.id.clone(),
            new_album: new.album.clone(),
        }));
    }
    Ok(pairs)
}

fn album_of(conn: &Connection, track: &str) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT local_album_id FROM local_tracks WHERE id = ?1",
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
        "SELECT id FROM library_play_history WHERE local_track_id = ?1",
        params![old],
    )?;
    // A star the user already has on the new row keeps its date; the old
    // one is deleted, kept here with its date and name.
    let stars_moved = strings(
        tx,
        "SELECT f.user_id FROM library_user_favorites f WHERE f.item_kind = 'track' \
         AND f.item_id = ?1 AND NOT EXISTS (SELECT 1 FROM library_user_favorites g \
         WHERE g.user_id = f.user_id AND g.item_kind = 'track' AND g.item_id = ?2)",
        params![old, new],
    )?;
    let stars_deleted: Vec<Value> = tx
        .prepare(
            "SELECT f.user_id, f.created_at, n.display_name FROM library_user_favorites f \
             LEFT JOIN library_user_favorite_names n ON n.user_id = f.user_id \
             AND n.item_kind = f.item_kind AND n.item_id = f.item_id \
             WHERE f.item_kind = 'track' AND f.item_id = ?1 \
             AND EXISTS (SELECT 1 FROM library_user_favorites g WHERE g.user_id = f.user_id \
             AND g.item_kind = 'track' AND g.item_id = ?2)",
        )?
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
        .prepare(
            "SELECT n.user_id, n.display_name FROM library_user_favorite_names n \
             WHERE n.item_kind = 'track' AND n.item_id = ?1 \
             AND EXISTS (SELECT 1 FROM library_user_favorites g WHERE g.user_id = n.user_id \
             AND g.item_kind = 'track' AND g.item_id = ?2) \
             AND NOT EXISTS (SELECT 1 FROM library_user_favorite_names m \
             WHERE m.user_id = n.user_id AND m.item_kind = 'track' AND m.item_id = ?2)",
        )?
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
        "SELECT r.user_id FROM library_user_ratings r WHERE r.item_kind = 'track' \
         AND r.item_id = ?1 AND NOT EXISTS (SELECT 1 FROM library_user_ratings s \
         WHERE s.user_id = r.user_id AND s.item_kind = 'track' AND s.item_id = ?2)",
        params![old, new],
    )?;
    let ratings_deleted: Vec<Value> = tx
        .prepare(
            "SELECT r.user_id, r.rating, r.rated_at FROM library_user_ratings r \
             WHERE r.item_kind = 'track' AND r.item_id = ?1 \
             AND EXISTS (SELECT 1 FROM library_user_ratings s WHERE s.user_id = r.user_id \
             AND s.item_kind = 'track' AND s.item_id = ?2)",
        )?
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
        "SELECT id FROM playlist_tracks WHERE library_file_id = ?1 \
         ORDER BY playlist_id, position",
        params![old],
    )?;
    tx.execute(
        "UPDATE playlist_tracks SET library_file_id = ?2, track_source_id = \
         CASE WHEN track_source_id = ?1 THEN ?2 ELSE track_source_id END \
         WHERE library_file_id = ?1",
        params![old, new],
    )?;

    // Where the user has a bookmark on both rows, the later one stays.
    let mut bookmarks_moved = Vec::new();
    let mut bookmarks_deleted = Vec::new();
    let bookmarks: Vec<(String, f64, Option<f64>)> = tx
        .prepare(
            "SELECT b.user_id, b.changed_at, (SELECT c.changed_at FROM compat_bookmarks c \
             WHERE c.user_id = b.user_id AND c.file_id = ?2) \
             FROM compat_bookmarks b WHERE b.file_id = ?1",
        )?
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
                "SELECT file_id, position_ms, comment, created_at, changed_at \
                 FROM compat_bookmarks WHERE user_id = ?1 AND file_id = ?2",
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
                "DELETE FROM compat_bookmarks WHERE user_id = ?1 AND file_id = ?2",
                params![user, loser],
            )?;
        }
        if loser != Some(old) {
            tx.execute(
                "UPDATE compat_bookmarks SET file_id = ?3 WHERE user_id = ?1 AND file_id = ?2",
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
        .prepare(
            "SELECT m.user_id, m.marked_played_at, m.marked_unplayed_at, \
             n.local_track_id IS NOT NULL, n.marked_played_at, n.marked_unplayed_at \
             FROM library_user_play_marks m LEFT JOIN library_user_play_marks n \
             ON n.user_id = m.user_id AND n.local_track_id = ?2 \
             WHERE m.local_track_id = ?1",
        )?
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
                "UPDATE library_user_play_marks SET local_track_id = ?3 \
                 WHERE user_id = ?1 AND local_track_id = ?2",
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
            "UPDATE library_user_play_marks SET marked_played_at = ?3, marked_unplayed_at = ?4 \
             WHERE user_id = ?1 AND local_track_id = ?2",
            params![user, new, played, unplayed],
        )?;
        tx.execute(
            "DELETE FROM library_user_play_marks WHERE user_id = ?1 AND local_track_id = ?2",
            params![user, old],
        )?;
        marks_merged.push(mark_json(&user, new, theirs));
        marks_deleted.push(mark_json(&user, old, mine));
    }

    let queue: Vec<Value> = tx
        .prepare(
            "SELECT user_id, item_index FROM compat_play_queue_items WHERE file_id = ?1 \
             ORDER BY user_id, item_index",
        )?
        .query_map(params![old], |row| {
            Ok(json!({
                "user_id": row.get::<_, String>(0)?,
                "item_index": row.get::<_, i64>(1)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute(
        "UPDATE compat_play_queue_items SET file_id = ?2 WHERE file_id = ?1",
        params![old, new],
    )?;

    // The new row's own override for a field wins; the old one stays on
    // the old row.
    let overrides: Vec<(String, f64, i64)> = if moves_overrides {
        tx.prepare(
            "SELECT o.id, o.updated_at, o.row_revision FROM library_management_overrides o \
             WHERE o.subject_kind = 'track' AND o.local_track_id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM library_management_overrides w \
             WHERE w.subject_kind = 'track' AND w.local_track_id = ?2 \
             AND w.field_name = o.field_name)",
        )?
        .query_map(params![old, new], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    for (id, _, _) in &overrides {
        tx.execute(
            "UPDATE library_management_overrides SET local_track_id = ?2, updated_at = ?3, \
             row_revision = row_revision + 1 WHERE id = ?1",
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
