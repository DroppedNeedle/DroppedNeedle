//! Catalog cleanup after an upgrade (v2's catalog identity hygiene): two
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
//!
//! A person's edition choice is never lost: an album holding a chosen
//! edition is only retired into an album with the same choice.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use crate::catalog_revision::bump_catalog;
use crate::corrections::membership::retire;
use crate::identify::sqlite::offer_album;
use crate::operations::decisions::{CatalogAction, record_action};
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
        None => fold_split_legacy_album(tx, &album, job_id, now)?,
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
    artist_folded: String,
    grouping_source: String,
    grouping_locked: bool,
}

fn album(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Album>> {
    conn.query_row(
        "SELECT id, root_id, title_folded, COALESCE(album_artist_name_folded, ''), \
         grouping_source, grouping_locked FROM local_albums \
         WHERE id = ?1 AND retired_into_album_id IS NULL",
        params![album_id],
        |row| {
            Ok(Album {
                id: row.get(0)?,
                root_id: row.get(1)?,
                title_folded: row.get(2)?,
                artist_folded: row.get(3)?,
                grouping_source: row.get(4)?,
                grouping_locked: row.get(5)?,
            })
        },
    )
    .optional()
}

/// `(release_group, release, decision_source)` of an album's identity.
type Identity = (String, Option<String>, String);

fn identity(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Identity>> {
    conn.query_row(
        "SELECT release_group_mbid, release_mbid, decision_source \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
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
        "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1)",
        params![album.id],
        |row| row.get(0),
    )?;
    if has_tracks {
        return Ok(None);
    }
    let mut stmt = tx.prepare(
        "SELECT target.id FROM local_albums target WHERE target.id <> ?1 \
         AND target.retired_into_album_id IS NULL AND target.root_id = ?2 \
         AND target.title_folded = ?3 AND COALESCE(target.album_artist_name_folded, '') = ?4 \
         AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = target.id \
         AND t.availability = 'indexed') ORDER BY target.id LIMIT 2",
    )?;
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
        // A chosen edition only goes into an album with that very choice.
        let choice_kept =
            decision != "manual" || (target_decision == "manual" && *release == target_release);
        if !(same_group && same_release && choice_kept) {
            return Ok(None);
        }
        tx.execute(
            "DELETE FROM local_album_external_identities WHERE local_album_id = ?1",
            params![album.id],
        )?;
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
    let mut stmt = conn.prepare(
        "SELECT t.id, t.relative_path, COALESCE(t.disc_number, 0), COALESCE(t.track_number, 0), \
         t.tag_album_title, t.tag_album_artist_name, t.embedded_release_group_mbid, \
         ti.release_track_mbid, ti.release_mbid, ti.decision_source, \
         t.availability = 'indexed' FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         AND ti.provider = ?2 WHERE t.local_album_id = ?1 ORDER BY t.id",
    )?;
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
    if decision != "legacy_import" || album.grouping_source != "legacy_import" {
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
    let mut stmt = tx.prepare(
        "SELECT target.id FROM local_albums target JOIN local_album_external_identities ti \
         ON ti.local_album_id = target.id AND ti.provider = 'musicbrainz' \
         WHERE target.id <> ?1 AND target.retired_into_album_id IS NULL \
         AND target.root_id = ?2 AND target.title_folded = ?3 \
         AND COALESCE(target.album_artist_name_folded, '') = ?4 \
         AND ti.release_group_mbid = ?5 AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = target.id AND t.availability = 'indexed') \
         ORDER BY target.id LIMIT 2",
    )?;
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
    tx.execute(
        &format!(
            "DELETE FROM local_track_external_identities WHERE local_track_id IN ({marks}) \
             AND decision_source = 'legacy_import'"
        ),
        params_from_iter(&moved),
    )?;
    tx.execute(
        "DELETE FROM local_album_external_identities WHERE local_album_id = ?1 \
         AND decision_source = 'legacy_import'",
        params![album.id],
    )?;
    tx.execute(
        &format!(
            "UPDATE local_tracks SET local_album_id = ?, row_revision = row_revision + 1 \
             WHERE id IN ({marks})"
        ),
        params_from_iter(std::iter::once(target).chain(&moved)),
    )?;
    retire(tx, &album.id, target, now)?;
    tx.execute(
        "UPDATE local_albums SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1",
        params![target, now],
    )?;
    offer_album(tx, target, crate::clock::now_ms() as i64)?;
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
        serde_json::json!({
            "source_album_id": album.id,
            "release_group_mbid": group,
            "release_mbid": release,
        }),
        &after,
        "AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR",
        now,
    )?;
    Ok(Some(after))
}
