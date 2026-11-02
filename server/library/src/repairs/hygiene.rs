//! Catalog cleanup after an upgrade (v2's catalog identity hygiene): three
//! repairs of album rows an earlier version left behind. Catalog rows
//! only; no file is touched. Each runs in the caller's transaction and
//! does nothing unless the evidence is unambiguous.
//!
//! - An empty album shell (no files left) that names the same album as
//!   exactly one live album in the same root is retired into it, once no
//!   running scan is still moving tracks onto that album.
//! - An album an imported (v2 `legacy_import`) identity split off, whose
//!   every file's own tags name another release group, is folded into the
//!   one live album in the same folder that carries that group.
//! - An album row that still names the "Unknown Artist" placeholder while
//!   its files name the artist takes its artist from its tracks. A scan
//!   writes an album's artist only from files it reads, so a row an
//!   earlier version left on the placeholder stays there while its files
//!   are unchanged.
//!
//! A person's edition choice is never lost: an album holding a chosen
//! edition, made now or in v2, is only retired into an album with the same
//! choice. The fold drops the split half's v2 choice; its audit row keeps
//! it, so the fold can be undone ([`crate::identity::undo_fold`]). The
//! artist repair keeps the album's earlier artist and credits the same
//! way ([`undo_artist_from_tracks`]).

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use crate::catalog_revision::bump_catalog;
use crate::corrections::membership::retire;
use crate::identify::artist_links::mark_stale;
use crate::identify::queue::offer_again;
use crate::identify::sqlite::offer_album;
use crate::identity::{self, DecisionSource, Rows};
use crate::operations::decisions::{CatalogAction, record_action};
use crate::scan::naming::{UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID};
use crate::scan::sqlite_store::awaits_retag;
use droppedneedle_kernel::db::fold::fold_text;

const PROVIDER: &str = "musicbrainz";

/// Clean one album. Returns what changed, or `None` when nothing did.
pub fn clean_album(
    tx: &Transaction<'_>,
    album_id: &str,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<Option<serde_json::Value>> {
    let Some(album) = album(tx, album_id)? else {
        return Ok(None);
    };
    let changed = match retire_empty_shell(tx, &album, job_id, now)? {
        Some(change) => Some(change),
        None => match fold_split_legacy_album(tx, &album, job_id, now)? {
            Some(change) => Some(change),
            None => artist_from_tracks(tx, &album, job_id, now)?,
        },
    };
    if changed.is_some() {
        bump_catalog(tx)?;
    }
    Ok(changed)
}

#[derive(Debug, Clone)]
struct Album {
    id: String,
    root_id: String,
    title_folded: String,
    artist_name: String,
    artist_folded: String,
    grouping_source: String,
    grouping_locked: bool,
}

fn album(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Album>> {
    conn.query_row(
        crate::repairs::sql::ALBUM.text(),
        params![album_id],
        |row| {
            Ok(Album {
                id: row.get(0)?,
                root_id: row.get(1)?,
                title_folded: row.get(2)?,
                artist_name: row.get(3)?,
                artist_folded: row.get(4)?,
                grouping_source: row.get(5)?,
                grouping_locked: row.get(6)?,
            })
        },
    )
    .optional()
}

/// `(release_group, release, decision_source)` of an album's identity.
type Identity = (String, Option<String>, DecisionSource);

fn identity(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Identity>> {
    conn.query_row(
        crate::repairs::sql::IDENTITY.text(),
        params![album_id, PROVIDER],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

/// Work a person or a job is doing on these albums: leave them alone.
fn busy(conn: &Connection, albums: &[&str], now: f64) -> rusqlite::Result<bool> {
    let marks = (1..=albums.len())
        .map(|number| format!("?{number}"))
        .collect::<Vec<_>>()
        .join(",");
    let live = crate::management::store::live_job_sql(&format!("?{}", albums.len() + 1));
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM library_contribution_drafts WHERE local_album_id IN \
             ({marks}) AND state IN ('draft','ready','seeded','verifying','needs_review')) \
             OR EXISTS(SELECT 1 FROM library_management_plan_items item \
             JOIN library_operation_jobs job ON job.id = item.job_id \
             WHERE item.local_album_id IN ({marks}) AND {live}) \
             OR EXISTS(SELECT 1 FROM library_management_overrides \
             WHERE local_album_id IN ({marks}))"
        ),
        params_from_iter(
            albums
                .iter()
                .map(|album| rusqlite::types::Value::Text((*album).to_owned()))
                .chain([rusqlite::types::Value::Real(now)]),
        ),
        |row| row.get(0),
    )
}

fn audit(
    tx: &Transaction<'_>,
    survivor: &str,
    job_id: &str,
    before: serde_json::Value,
    after: &serde_json::Value,
    reason: &str,
    now: f64,
) -> rusqlite::Result<()> {
    record_action(
        tx,
        CatalogAction {
            actor: "system",
            kind: "merge_album",
            album_id: survivor,
            job_id: Some(job_id),
            before,
            after: after.clone(),
            reason,
        },
        now,
    )?;
    Ok(())
}

fn retire_empty_shell(
    tx: &Transaction<'_>,
    album: &Album,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<Option<serde_json::Value>> {
    if album.grouping_source != "automatic" || album.grouping_locked {
        return Ok(None);
    }
    let has_tracks: bool = tx.query_row(
        crate::repairs::sql::ALBUM_HAS_TRACKS.text(),
        params![album.id],
        |row| row.get(0),
    )?;
    if has_tracks {
        return Ok(None);
    }
    let mut stmt = tx.prepare(crate::repairs::sql::SHELL_TARGET.text())?;
    let targets = stmt
        .query_map(
            params![
                album.id,
                album.root_id,
                album.title_folded,
                album.artist_folded
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let [target] = targets.as_slice() else {
        return Ok(None);
    };
    if busy(tx, &[&album.id], now)? {
        return Ok(None);
    }
    // The receiver may be the new row of an album a running scan is half
    // way through retagging. A row retired into it would stop the old row
    // taking it over.
    if awaits_retag(tx, target)? {
        return Ok(None);
    }
    let source = identity(tx, &album.id)?;
    if let Some((group, release, decision)) = &source {
        let Some((target_group, target_release, target_decision)) = identity(tx, target)? else {
            return Ok(None);
        };
        let same_group = *group == target_group;
        let same_release = release.is_none() || *release == target_release;
        // A chosen edition, made now or in v2, only goes into an album
        // with that very choice.
        let choice_kept = !decision.is_persons_choice()
            || (target_decision.is_persons_choice() && *release == target_release);
        if !(same_group && same_release && choice_kept) {
            return Ok(None);
        }
        identity::delete_album_row(tx, &album.id, Rows::Any)?;
    }
    retire(tx, &album.id, target, now)?;
    let after = serde_json::json!({
        "kind": "retire_empty_album_shell",
        "surviving_album_id": target,
        "retired_album_ids": [album.id],
        "filesystem_writes": 0,
    });
    audit(
        tx,
        target,
        job_id,
        serde_json::json!({ "retired_album_id": album.id }),
        &after,
        "AUTOMATIC_EMPTY_ALBUM_SHELL_CONVERGENCE",
        now,
    )?;
    Ok(Some(after))
}

#[derive(Debug, Clone)]
struct HygieneTrack {
    id: String,
    relative_path: String,
    disc: i64,
    number: i64,
    tag_title: Option<String>,
    tag_artist: Option<String>,
    embedded_group: Option<String>,
    release_track: Option<String>,
    identity_release: Option<String>,
    identity_source: Option<String>,
    indexed: bool,
}

fn tracks(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<HygieneTrack>> {
    let mut stmt = conn.prepare(crate::repairs::sql::TRACKS.text())?;
    stmt.query_map(params![album_id, PROVIDER], |row| {
        Ok(HygieneTrack {
            id: row.get(0)?,
            relative_path: row.get(1)?,
            disc: row.get(2)?,
            number: row.get(3)?,
            tag_title: row.get(4)?,
            tag_artist: row.get(5)?,
            embedded_group: row.get(6)?,
            release_track: row.get(7)?,
            identity_release: row.get(8)?,
            identity_source: row.get(9)?,
            indexed: row.get(10)?,
        })
    })?
    .collect()
}

/// The one value every track agrees on, folded; `None` when any track
/// lacks it or two disagree.
fn agreed(values: impl Iterator<Item = Option<String>>, fold: bool) -> Option<String> {
    let mut seen = HashSet::new();
    for value in values {
        let value = value.filter(|value| !value.trim().is_empty())?;
        seen.insert(if fold { fold_text(&value) } else { value });
    }
    (seen.len() == 1).then(|| seen.into_iter().next()).flatten()
}

fn fold_split_legacy_album(
    tx: &Transaction<'_>,
    album: &Album,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<Option<serde_json::Value>> {
    let Some((group, Some(release), decision)) = identity(tx, &album.id)? else {
        return Ok(None);
    };
    // A fold a person undid stays undone.
    if decision != DecisionSource::LegacyImport
        || album.grouping_source != "legacy_import"
        || identity::fold_undone(tx, &album.id)?
    {
        return Ok(None);
    }
    let source = tracks(tx, &album.id)?;
    if source.is_empty() || source.iter().any(|track| !track.indexed) {
        return Ok(None);
    }
    let later_decision = source.iter().any(|track| {
        track
            .identity_source
            .as_deref()
            .is_some_and(|source| source != "legacy_import")
    });
    let incomplete_map = source.iter().any(|track| {
        track.release_track.is_none() || track.identity_release.as_deref() != Some(&release)
    });
    let (Some(embedded), Some(tag_title), Some(tag_artist)) = (
        agreed(source.iter().map(|t| t.embedded_group.clone()), false),
        agreed(source.iter().map(|t| t.tag_title.clone()), true),
        agreed(source.iter().map(|t| t.tag_artist.clone()), true),
    ) else {
        return Ok(None);
    };
    if later_decision
        || !incomplete_map
        || uuid::Uuid::parse_str(&embedded).is_err()
        || embedded == group
        || tag_title != album.title_folded
    {
        return Ok(None);
    }
    let mut stmt = tx.prepare(crate::repairs::sql::LEGACY_SPLIT_TARGET.text())?;
    let targets = stmt
        .query_map(
            params![album.id, album.root_id, tag_title, tag_artist, embedded],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let [target] = targets.as_slice() else {
        return Ok(None);
    };
    let target_tracks = tracks(tx, target)?;
    let target_agrees = !target_tracks.is_empty()
        && target_tracks.iter().all(|track| {
            track.indexed
                && track.tag_title.as_deref().map(fold_text) == Some(tag_title.clone())
                && track.tag_artist.as_deref().map(fold_text) == Some(tag_artist.clone())
                && track.embedded_group.as_deref() == Some(&embedded)
        });
    let all: Vec<&HygieneTrack> = source.iter().chain(&target_tracks).collect();
    let folders: HashSet<String> = all
        .iter()
        .map(|track| {
            Path::new(&track.relative_path)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    let positions: HashSet<(i64, i64)> = all.iter().map(|t| (t.disc, t.number)).collect();
    let positions_unique =
        all.iter().all(|t| t.disc > 0 && t.number > 0) && positions.len() == all.len();
    if !target_agrees
        || folders.len() != 1
        || !positions_unique
        || busy(tx, &[&album.id, target], now)?
    {
        return Ok(None);
    }
    let moved: Vec<String> = source.iter().map(|track| track.id.clone()).collect();
    let marks = vec!["?"; moved.len()].join(",");
    // The v2 choice is kept in the audit row, so the fold can be undone.
    let folded = identity::fold_legacy(tx, &album.id, &moved)?;
    tx.execute(
        &format!(
            "UPDATE local_tracks SET local_album_id = ?, row_revision = row_revision + 1 \
             WHERE id IN ({marks})"
        ),
        params_from_iter(std::iter::once(target).chain(&moved)),
    )?;
    retire(tx, &album.id, target, now)?;
    tx.execute(
        crate::repairs::sql::BUMP_ALBUM_REVISION.text(),
        params![target, now],
    )?;
    offer_album(tx, target, crate::clock::ms(now))?;
    let after = serde_json::json!({
        "kind": "repair_contradictory_legacy_identity",
        "surviving_album_id": target,
        "retired_album_ids": [album.id],
        "moved_track_ids": moved,
        "rejected_release_group_mbid": group,
        "proven_release_group_mbid": embedded,
        "filesystem_writes": 0,
    });
    audit(
        tx,
        target,
        job_id,
        serde_json::to_value(&folded)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
        &after,
        identity::FOLD_REASON,
        now,
    )?;
    Ok(Some(after))
}

/// Replace the album's credits and mark its artist links for another
/// look. A plain delete and insert, so the read models that follow
/// `local_album_artists` see each row go and come.
fn write_album_credits(
    tx: &Transaction<'_>,
    album_id: &str,
    credits: &[CreditRow],
) -> rusqlite::Result<()> {
    tx.execute(
        crate::repairs::sql::WRITE_ALBUM_CREDITS.text(),
        params![album_id],
    )?;
    for (position, credit) in credits.iter().enumerate() {
        tx.execute(
            INSERT_ALBUM_CREDIT.text(),
            params![
                album_id,
                position as i64,
                credit.artist_id,
                credit.role,
                credit.credited_name,
                credit.join_phrase,
                credit.artist_mbid
            ],
        )?;
    }
    mark_stale(tx, album_id)
}

use crate::repairs::sql::INSERT_ALBUM_CREDIT;

/// One artist credit as `local_track_artists` and `local_album_artists`
/// hold it, in position order. A track credit reads as role `main`, the
/// role an album credit made from it takes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct CreditRow {
    artist_id: String,
    credited_name: Option<String>,
    join_phrase: String,
    artist_mbid: Option<String>,
    /// Audit rows written before the role was kept hold `main` credits.
    #[serde(default = "main_role")]
    role: String,
}

fn main_role() -> String {
    "main".to_owned()
}

use crate::repairs::sql::TRACK_CREDITS;

use crate::repairs::sql::ALBUM_CREDITS;

fn credit_rows(conn: &Connection, sql: &str, subject: &str) -> rusqlite::Result<Vec<CreditRow>> {
    conn.prepare_cached(sql)?
        .query_map(params![subject], |row| {
            Ok(CreditRow {
                artist_id: row.get(0)?,
                credited_name: row.get(1)?,
                join_phrase: row.get(2)?,
                artist_mbid: row.get(3)?,
                role: row.get(4)?,
            })
        })?
        .collect()
}

/// An indexed track as [`artist_from_tracks`] reads it.
struct NamedTrack {
    id: String,
    album_artist: String,
    album_artist_from_tag: bool,
    album_artist_sort: Option<String>,
    artist: String,
}

/// Give an album row that names the "Unknown Artist" placeholder the
/// artist its files name. Every indexed track's album artist must come
/// from its tags and be one real name, and the tracks credited under that
/// same name must share one credit list. The album takes that list, so it
/// points at the artist rows its own tracks already do. No artist row is
/// made, merged or retired, and the move is not recorded as a merge
/// candidate: the placeholder row stands for no artist.
fn artist_from_tracks(
    tx: &Transaction<'_>,
    album: &Album,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<Option<serde_json::Value>> {
    if album.artist_name != UNKNOWN_ARTIST
        || album.grouping_locked
        || album.grouping_source == "manual"
    {
        return Ok(None);
    }
    let tracks = tx
        .prepare(crate::repairs::sql::TRACK_ARTIST_TAGS.text())?
        .query_map(params![album.id], |row| {
            Ok(NamedTrack {
                id: row.get(0)?,
                album_artist: row.get(1)?,
                album_artist_from_tag: row.get(2)?,
                album_artist_sort: row.get(3)?,
                artist: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if tracks.iter().any(|track| !track.album_artist_from_tag) {
        return Ok(None);
    }
    let Some(name) = agreed(tracks.iter().map(|t| Some(t.album_artist.clone())), true) else {
        return Ok(None);
    };
    if name == fold_text(UNKNOWN_ARTIST) {
        return Ok(None);
    }
    let mut found: Option<(&NamedTrack, Vec<CreditRow>)> = None;
    for track in tracks
        .iter()
        .filter(|track| fold_text(&track.artist) == name)
    {
        let credits = credit_rows(tx, TRACK_CREDITS.text(), &track.id)?;
        match &found {
            None => found = Some((track, credits)),
            Some((_, seen)) if *seen == credits => {}
            Some(_) => return Ok(None),
        }
    }
    let Some((source, credits)) = found else {
        return Ok(None);
    };
    let Some(primary) = credits.first() else {
        return Ok(None);
    };
    if credits
        .iter()
        .any(|credit| credit.artist_id == UNKNOWN_ARTIST_ID)
        || busy(tx, &[&album.id], now)?
    {
        return Ok(None);
    }
    // A person's edit to the album is left alone. A running identification
    // read the old artist and would land on it; the album stays a
    // candidate and the cleanup after that identification comes back.
    let held: bool = tx.query_row(
        crate::repairs::sql::ALBUM_HAS_PERSON_CHOICE.text(),
        params![album.id],
        |row| row.get(0),
    )?;
    if held || artist_repair_undone(tx, &album.id)? {
        return Ok(None);
    }
    let (previous_artist, previous_sort): (String, Option<String>) = tx.query_row(
        crate::repairs::sql::ALBUM_ARTIST_ID_AND_SORT.text(),
        params![album.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let before = serde_json::json!({
        "album_artist_id": previous_artist,
        "album_artist_name": album.artist_name,
        "album_artist_name_folded": album.artist_folded,
        "album_artist_sort_name": previous_sort,
        "credits": credit_rows(tx, ALBUM_CREDITS.text(), &album.id)?,
    });
    tx.execute(
        crate::repairs::sql::WRITE_ALBUM_ARTIST.text(),
        params![
            album.id,
            source.album_artist,
            name,
            primary.artist_id,
            source.album_artist_sort,
            now
        ],
    )?;
    write_album_credits(tx, &album.id, &credits)?;
    // An album the placeholder kept unmatched is looked up again under
    // its artist's name. The undo takes that look back while it waits.
    let offered_at_ms = (now * 1000.0) as i64;
    let offered = identity(tx, &album.id)?.is_none() && offer_again(tx, &album.id, offered_at_ms)?;
    let after = serde_json::json!({
        "kind": "album_artist_from_tracks",
        "album_artist_id": primary.artist_id,
        "album_artist_name": source.album_artist,
        "album_artist_name_folded": name,
        "album_artist_sort_name": source.album_artist_sort,
        "credits": credits,
        "identification_offered_at_ms": offered.then_some(offered_at_ms),
        "filesystem_writes": 0,
    });
    record_action(
        tx,
        CatalogAction {
            actor: "system",
            kind: "album_artist",
            album_id: &album.id,
            job_id: Some(job_id),
            before,
            after: after.clone(),
            reason: ARTIST_REPAIR_REASON,
        },
        now,
    )?;
    Ok(Some(after))
}

/// The reason code of the audit row [`artist_from_tracks`] writes.
pub const ARTIST_REPAIR_REASON: &str = "PLACEHOLDER_ARTIST_FROM_TRACKS";

/// The audit kind of an undone placeholder-artist repair, recorded on the
/// album. The repair never runs again on that album.
pub const ARTIST_REPAIR_UNDONE: &str = "album_artist_undone";

/// Whether a placeholder-artist repair of the album was undone.
fn artist_repair_undone(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(crate::repairs::sql::ARTIST_REPAIR_UNDONE.text())?
        .query_row(params![album_id, ARTIST_REPAIR_UNDONE], |row| row.get(0))
}

/// The album's artist fields and credits before the repair, as its audit
/// row's `before_json` keeps them.
#[derive(serde::Deserialize)]
struct ArtistBefore {
    album_artist_id: String,
    album_artist_name: String,
    /// Absent from audit rows written before it was kept.
    album_artist_name_folded: Option<String>,
    album_artist_sort_name: Option<String>,
    credits: Vec<CreditRow>,
}

/// What the repair gave the album, as its audit row's `after_json` keeps
/// it.
#[derive(serde::Deserialize)]
struct ArtistAfter {
    album_artist_id: String,
    album_artist_name: String,
    /// The folded and sort names, absent from audit rows written before
    /// they were kept.
    album_artist_name_folded: Option<String>,
    album_artist_sort_name: Option<String>,
    credits: Vec<CreditRow>,
    /// When the repair queued a fresh identification of the album.
    identification_offered_at_ms: Option<i64>,
}

/// How [`undo_artist_from_tracks`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtistRepairUndo {
    /// The album's earlier artist and credits are back.
    Restored,
    /// The album's artist or credits changed since the repair, an artist
    /// row it named is gone or merged away, the album was retired into
    /// another, or the repair was undone already: nothing changed.
    Stale,
    /// `action_id` is not a placeholder-artist repair, or its album row is
    /// gone: nothing changed.
    Unknown,
}

/// The repair recorded as `action_id`, its album, what it replaced and
/// what it gave, or why [`undo_artist_from_tracks`] would refuse it.
fn artist_standing(
    conn: &Connection,
    action_id: &str,
) -> rusqlite::Result<Result<(String, ArtistBefore, ArtistAfter), ArtistRepairUndo>> {
    let action: Option<(Option<String>, String, String)> = conn
        .prepare_cached(crate::repairs::sql::ARTIST_REPAIR_ACTION.text())?
        .query_row(params![action_id, ARTIST_REPAIR_REASON], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let Some((Some(album_id), before, after)) = action else {
        return Ok(Err(ArtistRepairUndo::Unknown));
    };
    let (Ok(before), Ok(after)) = (
        serde_json::from_str::<ArtistBefore>(&before),
        serde_json::from_str::<ArtistAfter>(&after),
    ) else {
        return Ok(Err(ArtistRepairUndo::Unknown));
    };
    type Names = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let current: Option<Names> = conn
        .prepare_cached(crate::repairs::sql::LIVE_ALBUM_ARTIST.text())?
        .query_row(params![album_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    let unchanged = current.is_some_and(|(id, name, folded, sort)| {
        id.as_deref() == Some(after.album_artist_id.as_str())
            && name.as_deref() == Some(after.album_artist_name.as_str())
            && after.album_artist_name_folded.as_ref().is_none_or(|kept| {
                folded.as_ref() == Some(kept) && sort == after.album_artist_sort_name
            })
    }) && credit_rows(conn, ALBUM_CREDITS.text(), &album_id)? == after.credits;
    if !unchanged || artist_repair_undone(conn, &album_id)? {
        return Ok(Err(ArtistRepairUndo::Stale));
    }
    let named = std::iter::once(&before.album_artist_id)
        .chain(before.credits.iter().map(|credit| &credit.artist_id));
    for artist_id in named {
        let live: bool = conn
            .prepare_cached(crate::repairs::sql::ARTIST_IS_LIVE.text())?
            .query_row(params![artist_id], |row| row.get(0))?;
        if !live {
            return Ok(Err(ArtistRepairUndo::Stale));
        }
    }
    Ok(Ok((album_id, before, after)))
}

/// Whether [`undo_artist_from_tracks`] would undo the repair recorded as
/// audit row `action_id` now. Reads only.
pub fn artist_repair_undoable(conn: &Connection, action_id: &str) -> rusqlite::Result<bool> {
    Ok(artist_standing(conn, action_id)?.is_ok())
}

/// Undo the placeholder-artist repair recorded as audit row `action_id`,
/// for `actor`: the album's artist id, name, folded name and sort name and
/// its credits are what they were before it, an audit row
/// ([`ARTIST_REPAIR_UNDONE`]) keeps the repair from running on the album
/// again, and the catalog revision moves once. An identification the
/// repair queued is taken back while it still waits. Only while the album
/// still holds what the repair gave it. No artist row is made or deleted.
pub fn undo_artist_from_tracks(
    tx: &Transaction<'_>,
    action_id: &str,
    actor: &str,
    now: f64,
) -> rusqlite::Result<ArtistRepairUndo> {
    let (album_id, before, after) = match artist_standing(tx, action_id)? {
        Ok(repair) => repair,
        Err(outcome) => return Ok(outcome),
    };
    let folded = before
        .album_artist_name_folded
        .clone()
        .unwrap_or_else(|| fold_text(&before.album_artist_name));
    tx.execute(
        crate::repairs::sql::WRITE_ALBUM_ARTIST.text(),
        params![
            album_id,
            before.album_artist_name,
            folded,
            before.album_artist_id,
            before.album_artist_sort_name,
            now
        ],
    )?;
    write_album_credits(tx, &album_id, &before.credits)?;
    // The identification the repair queued goes too while it still waits:
    // it would look the album up under the name just taken back.
    if let Some(offered_ms) = after.identification_offered_at_ms {
        let dropped = tx
            .prepare_cached(crate::repairs::sql::DROP_REPAIR_IDENTIFY_JOB.text())?
            .execute(params![album_id, offered_ms])?;
        if dropped > 0 {
            crate::identify::queue::bump_identification_stream(tx)?;
        }
    }
    record_action(
        tx,
        CatalogAction {
            actor,
            kind: ARTIST_REPAIR_UNDONE,
            album_id: &album_id,
            job_id: None,
            before: serde_json::json!({ "repair_action_id": action_id }),
            after: serde_json::json!({
                "album_artist_id": before.album_artist_id,
                "album_artist_name": before.album_artist_name,
                "credits": before.credits,
            }),
            reason: "PLACEHOLDER_ARTIST_UNDONE",
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(ArtistRepairUndo::Restored)
}
