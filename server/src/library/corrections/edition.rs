//! What a membership change does to album editions.
//!
//! An album's edition is its MusicBrainz identity row; a person's choice
//! (a pin) is that row with `decision_source = 'manual'`. The rules:
//!
//! - an album that keeps its id keeps its edition (a split or move leaves
//!   the source's pin where it is);
//! - an album emptied by the change hands its edition to the album its
//!   tracks went to, when that album has none and nothing competes;
//! - when the editions of the albums being combined differ, the receiving
//!   album keeps its own (`retain_manual`) or every one is dropped
//!   (`detach`); with no edition of its own and several on offer, none
//!   wins and all are dropped;
//! - a dropped edition leaves a pending review carrying the reason, and the
//!   album is offered to identification again;
//! - a moved track keeps its track identity only when it sits on the
//!   receiving album's release;
//! - an album keeping a chosen edition that gains tracks queues them to be
//!   placed on that release.

use std::collections::BTreeSet;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use super::models::{EditionChange, EditionChangeKind, IdentityChoice};
use super::reasons;
use crate::library::operations::choice::queue_remap;
use crate::library::operations::reasons::Reason;

const PROVIDER: &str = "musicbrainz";

/// An album's edition as this module needs it.
#[derive(Debug, Clone)]
pub(super) struct Identity {
    pub album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub decision_source: String,
    pub row_revision: i64,
    /// A custom edition is active: it lists this album's own tracks.
    pub custom: bool,
}

impl Identity {
    /// The edition this identity names: the release, else the group.
    fn key(&self) -> String {
        self.release_mbid
            .as_deref()
            .unwrap_or(&self.release_group_mbid)
            .to_lowercase()
    }
}

pub(super) fn identity(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Identity>> {
    conn.query_row(
        "SELECT release_group_mbid, release_mbid, decision_source, row_revision, \
         EXISTS(SELECT 1 FROM library_custom_edition_active c WHERE c.local_album_id = ?1) \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
        |row| {
            Ok(Identity {
                album_id: album_id.to_owned(),
                release_group_mbid: row.get(0)?,
                release_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                row_revision: row.get(3)?,
                custom: row.get(4)?,
            })
        },
    )
    .optional()
}

pub(super) fn album_title(conn: &Connection, album_id: &str) -> rusqlite::Result<String> {
    Ok(conn
        .query_row(
            "SELECT title FROM local_albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// One receiving album's settlement.
pub(super) struct Settle<'a> {
    /// The album that receives tracks and keeps its id.
    pub dest: &'a str,
    /// Albums emptied into `dest` by this change.
    pub retired_into: &'a [String],
    /// Tracks that joined `dest`.
    pub moved_tracks: &'a [String],
    pub choice: IdentityChoice,
    pub actor: &'a str,
    pub now: f64,
}

/// Competing editions and what happened to each album's edition.
#[derive(Debug, Default)]
pub(super) struct Settled {
    pub conflicts: Vec<String>,
    pub changes: Vec<EditionChange>,
}

pub(super) fn settle(tx: &Transaction<'_>, s: &Settle<'_>) -> rusqlite::Result<Settled> {
    let own = identity(tx, s.dest)?;
    let mut incoming = Vec::new();
    for album in s.retired_into {
        if let Some(found) = identity(tx, album)? {
            incoming.push(found);
        }
    }
    let keys: BTreeSet<String> = own
        .iter()
        .chain(incoming.iter())
        .map(Identity::key)
        .collect();
    let conflicts: Vec<String> = if keys.len() > 1 {
        keys.iter().cloned().collect()
    } else {
        Vec::new()
    };
    let title = album_title(tx, s.dest)?;
    let change = |kind: EditionChangeKind,
                  from: Option<&str>,
                  release: Option<&str>,
                  reason: Reason| EditionChange {
        album_id: s.dest.to_owned(),
        album_title: title.clone(),
        change: kind,
        from_album_id: from.map(str::to_owned),
        release_mbid: release.map(str::to_owned),
        reason,
    };
    let mut changes = Vec::new();
    // The edition the album ends with, and why one was dropped.
    let mut kept: Option<Identity> = None;
    let mut dropped: Option<Reason> = None;
    match own {
        Some(own) if conflicts.is_empty() => {
            changes.push(change(
                EditionChangeKind::Kept,
                None,
                own.release_mbid.as_deref(),
                reasons::EDITION_KEPT,
            ));
            kept = Some(own);
        }
        Some(own) => match s.choice {
            IdentityChoice::RetainManual => {
                changes.push(change(
                    EditionChangeKind::Kept,
                    None,
                    own.release_mbid.as_deref(),
                    reasons::EDITION_CONFLICT_KEPT,
                ));
                kept = Some(own);
            }
            IdentityChoice::Detach => dropped = Some(reasons::EDITION_CONFLICT_CLEARED),
        },
        None if keys.len() == 1 => {
            let from = &incoming[0];
            if from.custom {
                dropped = Some(reasons::EDITION_CUSTOM_CLEARED);
            } else {
                move_edition(tx, &from.album_id, s.dest)?;
                changes.push(change(
                    EditionChangeKind::Moved,
                    Some(&from.album_id),
                    from.release_mbid.as_deref(),
                    reasons::EDITION_MOVED,
                ));
                kept = identity(tx, s.dest)?;
            }
        }
        None if keys.len() > 1 => dropped = Some(reasons::EDITION_AMBIGUOUS),
        None => {}
    }
    for album in s.retired_into {
        drop_edition(tx, album)?;
    }
    if let Some(reason) = dropped {
        drop_edition(tx, s.dest)?;
        tx.execute(
            "DELETE FROM local_track_external_identities WHERE provider = ?2 AND local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            params![s.dest, PROVIDER],
        )?;
        open_review(tx, s.dest, reason)?;
        changes.push(change(EditionChangeKind::Cleared, None, None, reason));
    } else if !s.moved_tracks.is_empty() {
        let release = kept
            .as_ref()
            .and_then(|identity| identity.release_mbid.clone());
        let marks = placeholders(s.moved_tracks.len());
        let mut values = vec![
            Value::Text(PROVIDER.to_owned()),
            release.map_or(Value::Null, Value::Text),
        ];
        values.extend(s.moved_tracks.iter().cloned().map(Value::Text));
        tx.execute(
            &format!(
                "DELETE FROM local_track_external_identities WHERE provider = ?1 \
                 AND release_mbid IS NOT NULL AND (?2 IS NULL OR lower(release_mbid) <> lower(?2)) \
                 AND local_track_id IN ({marks})"
            ),
            params_from_iter(values),
        )?;
        if let Some(chosen) = kept
            .as_ref()
            .filter(|identity| identity.decision_source == "manual" && !identity.custom)
            && let Some(release) = chosen.release_mbid.as_deref()
            && unplaced(tx, s.moved_tracks, release)?
        {
            queue_remap(tx, s.dest, release, Some(s.actor), s.now)?;
            changes.push(change(
                EditionChangeKind::RemapQueued,
                None,
                Some(release),
                reasons::EDITION_REMAP,
            ));
        }
    }
    Ok(Settled { conflicts, changes })
}

/// An album emptied with nowhere clear to go loses its edition.
pub(super) fn drop_scattered(
    tx: &Transaction<'_>,
    album_id: &str,
) -> rusqlite::Result<Option<EditionChange>> {
    let Some(found) = identity(tx, album_id)? else {
        return Ok(None);
    };
    drop_edition(tx, album_id)?;
    Ok(Some(EditionChange {
        album_id: album_id.to_owned(),
        album_title: album_title(tx, album_id)?,
        change: EditionChangeKind::Cleared,
        from_album_id: None,
        release_mbid: found.release_mbid,
        reason: reasons::EDITION_ALBUM_GONE,
    }))
}

/// Move `from`'s edition, with its match state and pending remap, onto
/// `to` (which has none).
fn move_edition(tx: &Transaction<'_>, from: &str, to: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE local_album_external_identities SET local_album_id = ?2, \
         row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?3",
        params![from, to, PROVIDER],
    )?;
    for table in ["library_album_match_state", "library_edition_remap_queue"] {
        tx.execute(
            &format!("UPDATE OR REPLACE {table} SET local_album_id = ?2 WHERE local_album_id = ?1"),
            params![from, to],
        )?;
    }
    Ok(())
}

/// Remove an album's edition and everything that hangs off it.
fn drop_edition(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
    )?;
    for table in [
        "library_album_match_state",
        "library_edition_choice_undo",
        "library_automatic_edition_undo",
        "library_edition_remap_queue",
        "library_custom_edition_active",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE local_album_id = ?1"),
            params![album_id],
        )?;
    }
    Ok(())
}

/// Leave a pending review saying why the edition went.
fn open_review(tx: &Transaction<'_>, album_id: &str, reason: Reason) -> rusqlite::Result<()> {
    let now_ms = crate::library::clock::now_ms() as i64;
    tx.execute(
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, candidates_json, \
         state, created_ms, updated_ms) VALUES (?1, ?2, ?3, '[]', 'pending', ?4, ?4)",
        params![
            uuid::Uuid::new_v4().to_string(),
            album_id,
            reason.code,
            now_ms
        ],
    )?;
    Ok(())
}

/// Some moved track is not placed on `release`.
fn unplaced(tx: &Transaction<'_>, tracks: &[String], release: &str) -> rusqlite::Result<bool> {
    let marks = placeholders(tracks.len());
    let placed: i64 = tx.query_row(
        &format!(
            "SELECT COUNT(*) FROM local_track_external_identities WHERE provider = 'musicbrainz' \
             AND lower(release_mbid) = lower(?1) AND release_track_mbid IS NOT NULL \
             AND local_track_id IN ({marks})"
        ),
        params_from_iter(std::iter::once(release).chain(tracks.iter().map(String::as_str))),
        |row| row.get(0),
    )?;
    Ok(placed < tracks.len() as i64)
}

pub(super) fn placeholders(n: usize) -> String {
    vec!["?"; n.max(1)].join(",")
}
