//! Reset grouping: hand selected tracks back to automatic grouping.
//!
//! Each track is unlocked and filed where a scan would file it, read from
//! the names its catalog row carries (the same rules a scan uses). A
//! split followed by a reset brings the original album back under its old
//! id. An album the reset empties is retired into the album that took most
//! of its tracks; its edition follows only when that album is a clear
//! winner.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Transaction, params};

use super::edition::{self, Settle};
use super::membership::{
    AlbumRow, Run, album_state, album_track_ids, check_expected, identity_state, kept_editions,
    load_album, load_tracks, material, offer, retire, selected,
};
use super::models::{AlbumGroup, CorrectionError, MembershipOutcome, MembershipRequest};
use super::reasons;
use crate::library::scan::sqlite_store::automatic_album_for;

pub(super) fn reset(
    tx: &Transaction<'_>,
    request: &MembershipRequest,
    actor: &str,
    now: f64,
) -> Result<Run, CorrectionError> {
    let track_ids = selected(request)?;
    check_expected(tx, request)?;
    let album_id = request.album_id.as_deref().unwrap_or_default();
    let album =
        load_album(tx, album_id)?.ok_or(CorrectionError::NotFound(reasons::ALBUM_NOT_FOUND))?;
    let tracks = load_tracks(tx, &track_ids)?;
    if tracks.iter().any(|track| track.album_id != album.id) {
        return Err(CorrectionError::Invalid(reasons::TRACK_OUTSIDE_ALBUM));
    }
    if !held_by_hand(tx, &album, &track_ids)? {
        return Err(CorrectionError::Invalid(reasons::NOTHING_TO_RESET));
    }
    let mut state = album_state(tx, &[&album])?;

    tx.execute(
        &format!(
            "UPDATE local_tracks SET membership_locked = 0, membership_source = 'automatic', \
             row_revision = row_revision + 1 WHERE id IN ({})",
            edition::placeholders(track_ids.len())
        ),
        rusqlite::params_from_iter(track_ids.iter()),
    )?;
    // Where each track lands, in path order so names resolve the way a
    // scan of the folder would.
    let mut landing: BTreeMap<String, (Vec<String>, bool)> = BTreeMap::new();
    for track in &tracks {
        let Some((dest, created)) = automatic_album_for(tx, &track.id, now)? else {
            return Err(CorrectionError::NotFound(reasons::TRACK_NOT_FOUND));
        };
        if dest != track.album_id {
            tx.execute(
                "UPDATE local_tracks SET local_album_id = ?2 WHERE id = ?1",
                params![track.id, dest],
            )?;
        }
        let entry = landing.entry(dest).or_insert_with(|| (Vec::new(), false));
        entry.0.push(track.id.clone());
        entry.1 |= created;
    }

    let mut retired = Vec::new();
    let mut scattered = None;
    if album_track_ids(tx, &album.id)?.is_empty() {
        let mut shares: Vec<(usize, &String)> = landing
            .iter()
            .map(|(dest, (ids, _))| (ids.len(), dest))
            .collect();
        shares.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        let unique = shares.len() == 1 || shares.get(1).is_none_or(|next| next.0 < shares[0].0);
        if let Some((_, successor)) = shares.first() {
            let successor = (*successor).clone();
            if !unique {
                scattered = edition::drop_scattered(tx, &album.id, &successor)?;
            }
            retire(tx, &album.id, &successor, now)?;
            retired.push((album.id.clone(), Some(successor)));
        }
    } else {
        let locked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1 \
             AND membership_locked = 1)",
            params![album.id],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE local_albums SET grouping_source = CASE WHEN ?2 THEN grouping_source \
             ELSE 'automatic' END, grouping_locked = ?2, updated_at = ?3, \
             row_revision = row_revision + 1 WHERE id = ?1",
            params![album.id, locked, now],
        )?;
    }

    // The editions the landing albums hold decide what the reset does to
    // them, so the token covers them too.
    let existing: Vec<String> = landing
        .iter()
        .filter(|(_, (_, created))| !created)
        .map(|(id, _)| id.clone())
        .collect();
    state.extend(identity_state(tx, &existing)?);
    let mut changes = Vec::new();
    let mut conflicts = BTreeSet::new();
    let mut groups = Vec::new();
    for (dest, (ids, created)) in &landing {
        if dest == &album.id {
            groups.push(group(tx, dest, ids, *created)?);
            continue;
        }
        let emptied: Vec<String> = retired
            .iter()
            .filter(|(_, to)| to.as_deref() == Some(dest.as_str()) && scattered.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        let settled = edition::settle(
            tx,
            &Settle {
                dest,
                retired_into: &emptied,
                moved_tracks: ids,
                choice: request.identity_choice,
                actor,
                now,
            },
        )?;
        conflicts.extend(settled.conflicts);
        changes.extend(settled.changes);
        groups.push(group(tx, dest, ids, *created)?);
    }
    if retired.is_empty() {
        changes.splice(0..0, kept_editions(tx, std::slice::from_ref(&album.id))?);
    }
    changes.extend(scattered);
    let mut changed: Vec<String> = landing.keys().cloned().collect();
    changed.push(album.id.clone());
    offer(tx, &changed)?;

    let outcome = MembershipOutcome {
        track_ids,
        source_album_ids: vec![album.id.clone()],
        target_album_id: None,
        groups,
        retired,
        identity_conflicts: conflicts.into_iter().collect(),
        edition_changes: changes,
    };
    let material = material(request, &state, &tracks, &outcome);
    Ok(Run { outcome, material })
}

/// Some selected track (or the album itself) is grouped by hand or kept
/// from an earlier version's grouping.
fn held_by_hand(
    tx: &Transaction<'_>,
    album: &AlbumRow,
    track_ids: &[String],
) -> rusqlite::Result<bool> {
    if album.grouping_source != "automatic" {
        return Ok(true);
    }
    let held: i64 = tx.query_row(
        &format!(
            "SELECT COUNT(*) FROM local_tracks WHERE id IN ({}) \
             AND (membership_locked = 1 OR membership_source <> 'automatic')",
            edition::placeholders(track_ids.len())
        ),
        rusqlite::params_from_iter(track_ids.iter()),
        |row| row.get(0),
    )?;
    Ok(held > 0)
}

fn group(
    tx: &Transaction<'_>,
    album_id: &str,
    track_ids: &[String],
    created: bool,
) -> Result<AlbumGroup, CorrectionError> {
    let album = load_album(tx, album_id)?;
    Ok(AlbumGroup {
        album_id: album_id.to_owned(),
        title: album
            .as_ref()
            .map(|album| album.title.clone())
            .unwrap_or_default(),
        album_artist_name: album
            .map(|album| album.album_artist_name)
            .unwrap_or_default(),
        track_ids: track_ids.to_vec(),
        created,
        reason_code: if created {
            "AUTOMATIC_NEW_ALBUM"
        } else {
            "AUTOMATIC_GROUPING"
        },
    })
}
