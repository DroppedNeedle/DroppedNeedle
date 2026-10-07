//! Split, merge and move: tracks change albums by hand.
//!
//! Only catalog rows change; no file is moved or retagged. Moved tracks
//! are locked to their new album, so a rescan leaves them there. An
//! album the change empties is retired into the album its tracks went to
//! (old links to it keep working through an alias). Everything here runs
//! inside the caller's transaction; a preview runs the same code and
//! rolls back.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value;
use rusqlite::{OptionalExtension as _, Transaction, params, params_from_iter};
use serde::Serialize;

use super::edition::{self, Settle, placeholders};
use super::models::{
    AlbumGroup, CorrectionError, EditionChange, EditionChangeKind, MembershipKind,
    MembershipOutcome, MembershipRequest,
};
use super::reasons;
use crate::library::identify::sqlite::offer_album;

/// One stored track the change reads.
#[derive(Debug, Clone)]
pub(super) struct TrackRow {
    pub id: String,
    pub album_id: String,
    pub row_revision: i64,
}

/// One stored album the change reads.
#[derive(Debug, Clone)]
pub(super) struct AlbumRow {
    pub id: String,
    pub title: String,
    pub album_artist_name: String,
    pub row_revision: i64,
    pub grouping_source: String,
}

/// A change as run: what it did and the material its token covers.
pub(super) struct Run {
    pub outcome: MembershipOutcome,
    pub material: String,
}

/// What a preview token covers: the request, the state read before the
/// change, and where every track ended up. A new album's id is random, so
/// it stands in as `new:<n>`.
#[derive(Serialize)]
struct Material<'a> {
    kind: &'static str,
    album_id: Option<&'a str>,
    track_ids: Vec<&'a str>,
    target_album_id: Option<&'a str>,
    title: Option<&'a str>,
    album_artist_name: Option<&'a str>,
    albums: Vec<(String, i64, i64)>,
    tracks: Vec<(String, String, i64)>,
    groups: Vec<(String, Vec<String>)>,
    retired: Vec<(String, Option<String>)>,
}

pub(super) fn material(
    request: &MembershipRequest,
    albums: &[(String, i64, i64)],
    tracks: &[TrackRow],
    outcome: &MembershipOutcome,
) -> String {
    let mut created = BTreeMap::new();
    for group in outcome.groups.iter().filter(|group| group.created) {
        let next = format!("new:{}", created.len());
        created.insert(group.album_id.clone(), next);
    }
    let name = |id: &String| created.get(id).cloned().unwrap_or_else(|| id.clone());
    let mut track_ids: Vec<&str> = request.track_ids.iter().map(String::as_str).collect();
    track_ids.sort_unstable();
    track_ids.dedup();
    let mut albums = albums.to_vec();
    albums.sort();
    let mut tracks: Vec<(String, String, i64)> = tracks
        .iter()
        .map(|track| (track.id.clone(), track.album_id.clone(), track.row_revision))
        .collect();
    tracks.sort();
    let mut groups: Vec<(String, Vec<String>)> = outcome
        .groups
        .iter()
        .map(|group| {
            let mut ids = group.track_ids.clone();
            ids.sort();
            (name(&group.album_id), ids)
        })
        .collect();
    groups.sort();
    let mut retired: Vec<(String, Option<String>)> = outcome
        .retired
        .iter()
        .map(|(album, to)| (album.clone(), to.as_ref().map(name)))
        .collect();
    retired.sort();
    let material = Material {
        kind: request.kind.as_str(),
        album_id: request.album_id.as_deref(),
        track_ids,
        target_album_id: request.target_album_id.as_deref(),
        title: request.title.as_deref(),
        album_artist_name: request.album_artist_name.as_deref(),
        albums,
        tracks,
        groups,
        retired,
    };
    serde_json::to_string(&material).unwrap_or_default()
}

/// The selected tracks, deduplicated, in a stable order.
pub(super) fn selected(request: &MembershipRequest) -> Result<Vec<String>, CorrectionError> {
    let ids: BTreeSet<String> = request
        .track_ids
        .iter()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return Err(CorrectionError::Invalid(reasons::NO_TRACKS));
    }
    Ok(ids.into_iter().collect())
}

pub(super) fn load_tracks(
    tx: &Transaction<'_>,
    ids: &[String],
) -> Result<Vec<TrackRow>, CorrectionError> {
    let marks = placeholders(ids.len());
    let rows: Vec<TrackRow> = tx
        .prepare(&format!(
            "SELECT id, local_album_id, row_revision FROM local_tracks WHERE id IN ({marks}) \
             ORDER BY relative_path, id"
        ))?
        .query_map(params_from_iter(ids.iter()), |row| {
            Ok(TrackRow {
                id: row.get(0)?,
                album_id: row.get(1)?,
                row_revision: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rows.len() != ids.len() {
        return Err(CorrectionError::NotFound(reasons::TRACK_NOT_FOUND));
    }
    Ok(rows)
}

/// A live (not merged away) album.
pub(super) fn load_album(
    tx: &Transaction<'_>,
    album_id: &str,
) -> Result<Option<AlbumRow>, CorrectionError> {
    Ok(tx
        .query_row(
            "SELECT id, title, COALESCE(album_artist_name, ''), row_revision, grouping_source \
             FROM local_albums WHERE id = ?1 AND retired_into_album_id IS NULL",
            params![album_id],
            |row| {
                Ok(AlbumRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    album_artist_name: row.get(2)?,
                    row_revision: row.get(3)?,
                    grouping_source: row.get(4)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn album_track_ids(
    tx: &Transaction<'_>,
    album_id: &str,
) -> rusqlite::Result<Vec<String>> {
    tx.prepare("SELECT id FROM local_tracks WHERE local_album_id = ?1 ORDER BY relative_path, id")?
        .query_map(params![album_id], |row| row.get(0))?
        .collect()
}

/// Refuse when an album the page showed moved on. Zero means the page did
/// not know the revision.
pub(super) fn check_expected(
    tx: &Transaction<'_>,
    request: &MembershipRequest,
) -> Result<(), CorrectionError> {
    for (album_id, expected) in &request.expected_album_revisions {
        if *expected <= 0 {
            continue;
        }
        let album =
            load_album(tx, album_id)?.ok_or(CorrectionError::NotFound(reasons::ALBUM_NOT_FOUND))?;
        if album.row_revision != *expected {
            return Err(CorrectionError::Conflict(reasons::REVISION_STALE));
        }
    }
    Ok(())
}

/// Albums with their revision and their edition's revision (0 for none).
pub(super) fn album_state(
    tx: &Transaction<'_>,
    albums: &[&AlbumRow],
) -> Result<Vec<(String, i64, i64)>, CorrectionError> {
    let mut state = Vec::with_capacity(albums.len());
    for album in albums {
        let identity = edition::identity(tx, &album.id)?.map_or(0, |found| found.row_revision);
        state.push((album.id.clone(), album.row_revision, identity));
    }
    Ok(state)
}

/// Retire an emptied album into `successor`, with an alias so links to the
/// old id land on the new album. Its pending reviews close and its queued
/// identification goes.
pub(super) fn retire(
    tx: &Transaction<'_>,
    album_id: &str,
    successor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE local_albums SET retired_into_album_id = ?2, grouping_locked = 1, \
         updated_at = ?3, row_revision = row_revision + 1 WHERE id = ?1",
        params![album_id, successor, now],
    )?;
    tx.execute(
        "UPDATE local_album_aliases SET local_album_id = ?2 WHERE local_album_id = ?1",
        params![album_id, successor],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO local_album_aliases (alias, local_album_id, kind, created_at) \
         VALUES (?1, ?2, 'merged_album', ?3)",
        params![album_id, successor, now],
    )?;
    tx.execute(
        "UPDATE library_identify_reviews SET state = 'rejected', updated_ms = ?2 \
         WHERE local_album_id = ?1 AND state = 'pending'",
        params![album_id, crate::library::clock::now_ms() as i64],
    )?;
    tx.execute(
        "DELETE FROM library_identify_jobs WHERE local_album_id = ?1 \
         AND state IN ('queued', 'deferred')",
        params![album_id],
    )?;
    Ok(())
}

/// Lock tracks to `album_id` by hand.
fn lock_to(tx: &Transaction<'_>, album_id: &str, tracks: &[String]) -> rusqlite::Result<()> {
    let marks = placeholders(tracks.len());
    let mut values = vec![Value::Text(album_id.to_owned())];
    values.extend(tracks.iter().cloned().map(Value::Text));
    tx.execute(
        &format!(
            "UPDATE local_tracks SET local_album_id = ?1, membership_source = 'manual', \
             membership_locked = 1, row_revision = row_revision + 1 WHERE id IN ({marks})"
        ),
        params_from_iter(values),
    )?;
    Ok(())
}

/// Offer albums whose contents changed to identification: an automatic
/// edition gets another look, a new album gets its first. A chosen
/// edition is never replaced by this.
pub(super) fn offer(tx: &Transaction<'_>, albums: &[String]) -> rusqlite::Result<()> {
    let now_ms = crate::library::clock::now_ms() as i64;
    for album in albums {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
             AND retired_into_album_id IS NULL)",
            params![album],
            |row| row.get(0),
        )?;
        if live {
            offer_album(tx, album, now_ms)?;
        }
    }
    Ok(())
}

/// Each live source that lost tracks keeps its edition; say so.
pub(super) fn kept_editions(
    tx: &Transaction<'_>,
    albums: &[String],
) -> rusqlite::Result<Vec<EditionChange>> {
    let mut kept = Vec::new();
    for album in albums {
        if let Some(found) = edition::identity(tx, album)? {
            kept.push(EditionChange {
                album_id: album.clone(),
                album_title: edition::album_title(tx, album)?,
                change: EditionChangeKind::Kept,
                from_album_id: None,
                release_mbid: found.release_mbid,
                reason: reasons::EDITION_KEPT,
            });
        }
    }
    Ok(kept)
}

/// Split, merge or move, in the caller's transaction.
pub(super) fn regroup(
    tx: &Transaction<'_>,
    request: &MembershipRequest,
    actor: &str,
    now: f64,
) -> Result<Run, CorrectionError> {
    let kind = request.kind;
    let mut track_ids = selected(request)?;
    check_expected(tx, request)?;
    let mut tracks = load_tracks(tx, &track_ids)?;
    let target = match request
        .target_album_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        Some(id) => {
            Some(load_album(tx, id)?.ok_or(CorrectionError::NotFound(reasons::TARGET_NOT_FOUND))?)
        }
        None if kind == MembershipKind::Split => None,
        None => return Err(CorrectionError::Invalid(reasons::TARGET_REQUIRED)),
    };
    if kind == MembershipKind::Split {
        let album_id = request.album_id.as_deref().unwrap_or_default();
        if tracks.iter().any(|track| track.album_id != album_id) {
            return Err(CorrectionError::Invalid(reasons::TRACK_OUTSIDE_ALBUM));
        }
    }
    let target_id = target.as_ref().map(|album| album.id.clone());
    // Tracks already on the target stay put.
    tracks.retain(|track| Some(&track.album_id) != target_id.as_ref());
    if tracks.is_empty() {
        return Err(CorrectionError::Invalid(reasons::TARGET_IS_SOURCE));
    }
    let source_ids: Vec<String> = tracks
        .iter()
        .map(|track| track.album_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut sources = Vec::with_capacity(source_ids.len());
    for id in &source_ids {
        sources
            .push(load_album(tx, id)?.ok_or(CorrectionError::NotFound(reasons::ALBUM_NOT_FOUND))?);
    }
    // A merge takes whole albums: every track, found or missing.
    if kind == MembershipKind::Merge {
        let mut everything = Vec::new();
        for id in &source_ids {
            everything.extend(album_track_ids(tx, id)?);
        }
        tracks = load_tracks(tx, &everything)?;
    }
    track_ids = tracks.iter().map(|track| track.id.clone()).collect();
    if kind == MembershipKind::Split && target.is_none() {
        let remaining = album_track_ids(tx, &source_ids[0])?;
        if remaining.len() <= track_ids.len() {
            return Err(CorrectionError::Invalid(reasons::SPLIT_TAKES_EVERYTHING));
        }
    }
    let state = {
        let mut involved: Vec<&AlbumRow> = sources.iter().collect();
        involved.extend(target.iter());
        album_state(tx, &involved)?
    };

    // The receiving album.
    let (dest, created) = match &target {
        Some(album) => (album.clone(), false),
        None => (create_split_album(tx, &sources[0], request, now)?, true),
    };
    lock_to(tx, &dest.id, &track_ids)?;

    let mut retired = Vec::new();
    let mut live_sources = Vec::new();
    for id in &source_ids {
        if album_track_ids(tx, id)?.is_empty() {
            retire(tx, id, &dest.id, now)?;
            retired.push(id.clone());
        } else {
            live_sources.push(id.clone());
        }
    }
    for album in live_sources.iter().chain(std::iter::once(&dest.id)) {
        tx.execute(
            "UPDATE local_albums SET grouping_source = 'manual', grouping_locked = 1, \
             updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1",
            params![album, now],
        )?;
    }
    let settled = edition::settle(
        tx,
        &Settle {
            dest: &dest.id,
            retired_into: &retired,
            moved_tracks: &track_ids,
            choice: request.identity_choice,
            actor,
            now,
        },
    )?;
    let mut changes = kept_editions(tx, &live_sources)?;
    changes.extend(settled.changes);
    let mut changed = live_sources.clone();
    changed.push(dest.id.clone());
    offer(tx, &changed)?;

    let outcome = MembershipOutcome {
        track_ids: track_ids.clone(),
        source_album_ids: source_ids,
        target_album_id: target_id,
        groups: vec![AlbumGroup {
            album_id: dest.id.clone(),
            title: dest.title.clone(),
            album_artist_name: dest.album_artist_name.clone(),
            track_ids,
            created,
            reason_code: match kind {
                MembershipKind::Split => "MANUAL_SPLIT",
                MembershipKind::Merge => "MANUAL_MERGE",
                MembershipKind::Move | MembershipKind::Reset => "MANUAL_MOVE",
            },
        }],
        retired: retired
            .into_iter()
            .map(|id| (id, Some(dest.id.clone())))
            .collect(),
        identity_conflicts: settled.conflicts,
        edition_changes: changes,
    };
    let material = material(request, &state, &tracks, &outcome);
    Ok(Run { outcome, material })
}

/// The new album a split makes: the source's names (or the ones given),
/// grouped by hand so a scan never folds it back.
fn create_split_album(
    tx: &Transaction<'_>,
    source: &AlbumRow,
    request: &MembershipRequest,
    now: f64,
) -> Result<AlbumRow, CorrectionError> {
    let id = uuid::Uuid::new_v4().to_string();
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(&source.title)
        .to_owned();
    let artist = request
        .album_artist_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&source.album_artist_name)
        .to_owned();
    tx.execute(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, album_artist_sort_name, \
         year, original_release_date, primary_genre, is_compilation, grouping_source, \
         grouping_locked, created_at, updated_at) \
         SELECT ?1, root_id, 'manual:' || ?1, ?2, ?3, ?4, ?5, album_artist_id, \
         album_artist_sort_name, year, original_release_date, primary_genre, is_compilation, \
         'manual', 1, ?6, ?6 FROM local_albums WHERE id = ?7",
        params![
            id,
            title,
            crate::db::fold_text(&title),
            artist,
            crate::db::fold_text(&artist),
            now,
            source.id,
        ],
    )?;
    tx.execute(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase) SELECT ?1, position, local_artist_id, role, credited_name, \
         join_phrase FROM local_album_artists WHERE local_album_id = ?2",
        params![id, source.id],
    )?;
    Ok(AlbumRow {
        id,
        title,
        album_artist_name: artist,
        row_revision: 1,
        grouping_source: "manual".to_owned(),
    })
}
