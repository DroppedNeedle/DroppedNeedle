//! An album's history: what the app did by itself to which tracks the
//! album holds, which albums it is made of, which edition it is on, or
//! where people's history lives, and the undos of those changes.
//!
//! Read from `library_catalog_actions`. The audit JSON keeps every row a
//! move touched, other
//! people's included, so only the typed fields below leave this module: no
//! album or track ids, no reason codes, no counts of what moved. The one id
//! is the audit row of a fold or of an artist repair, as its undo handle.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension as _, params, params_from_iter};
use serde::Deserialize;

use crate::corrections::continuation::SAME_FILE;
use crate::identify::models::PriorAlbumIdentity;
use crate::identity;
use crate::repairs::hygiene;
use droppedneedle_kernel::db::statement::ListLen;

/// How far back the history reaches.
const MAX_AGE_SECS: f64 = 180.0 * 86_400.0;
/// Most entries one album shows.
const MAX_ENTRIES: usize = 10;
/// Most track titles one entry names.
pub(super) const MAX_TITLES: usize = 5;

/// What one entry tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    /// Tracks continued into the tracks that took their place.
    TracksContinued,
    /// Tracks replaced by the edition's version continued into it.
    TracksReplaced,
    /// An album retagged outside DroppedNeedle was joined into this one.
    RetagJoined,
    /// Left unmanaged like the album whose files were retagged as this one.
    RetagHalfLeftAlone,
    /// The tracks came back from the album a person kept apart from this
    /// one, so that answer was dropped.
    RetagDismissalEnded,
    /// A leftover album of an earlier version was folded into this one.
    LeftoverJoined,
    /// Other music took the paths of tracks, which stayed behind as missing.
    TracksOtherMusic,
    /// Tracks' files were replaced by new copies of the same recordings.
    TracksNewCopy,
    /// The album took its artist from its tracks instead of the "Unknown
    /// Artist" placeholder.
    ArtistFromTracks,
    /// The app moved the album to another edition.
    EditionChanged,
    /// The app gave the album, matched to a release group only, an edition.
    EditionChosen,
    /// The app took the album's match off: new evidence contradicted it.
    MatchWithdrawn,
    /// A person undid the app's edition change.
    EditionChangeUndone,
    /// The fold of a leftover album into this one was undone: it is its
    /// own album again.
    FoldUndone,
    /// This album was folded into another and brought back out by an undo.
    UnfoldedFrom,
    /// Handing the album back to automatic management stopped: its files
    /// could not be read after every try.
    HandBackGivenUp,
}

/// Who asked for an entry, as the caller may know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryActor {
    /// The caller.
    You,
    /// Another person.
    Person,
}

/// One line of an album's history.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub kind: HistoryKind,
    /// Unix milliseconds.
    pub at: i64,
    /// Who asked, for a join or an undo; `None` for the app.
    pub by: Option<HistoryActor>,
    /// How many tracks the entry is about; 0 for album entries.
    pub track_count: usize,
    /// The first of those tracks' titles, in album order.
    pub track_titles: Vec<String>,
    /// The other album's title, when its row is still there.
    pub other_album_title: Option<String>,
    /// For a fold or an artist repair that can be undone now: the id to
    /// undo it by.
    pub undo_id: Option<String>,
}

/// One pair of a `continue_tracks` row.
#[derive(Deserialize)]
struct Pair {
    new_track_id: String,
    new_pair: bool,
    #[serde(default)]
    replaced: bool,
    /// Absent on rows from before the basis was kept.
    #[serde(default)]
    basis: Option<String>,
}

#[derive(Deserialize)]
struct Continued {
    pairs: Vec<Pair>,
}

/// One pair of an `other_music_at_path` row.
#[derive(Deserialize)]
struct Replaced {
    old_track_id: String,
}

#[derive(Deserialize)]
struct OtherMusic {
    pairs: Vec<Replaced>,
}

/// One track of a `new_copy_at_path` row.
#[derive(Deserialize)]
struct Copied {
    track_id: String,
}

#[derive(Deserialize)]
struct NewCopies {
    tracks: Vec<Copied>,
}

/// The other album of a join, under whichever name its writer used.
#[derive(Deserialize)]
struct OtherAlbum {
    retired_album_id: Option<String>,
    from_album_id: Option<String>,
    source_album_id: Option<String>,
}

struct Row {
    id: String,
    actor: Option<String>,
    kind: String,
    reason: Option<String>,
    before: String,
    after: String,
    created_at: f64,
}

/// The album's history, newest first. `None` when `album_id` is not a
/// live album (unknown, or retired into another).
pub fn album_history(
    conn: &Connection,
    album_id: &str,
    caller_user_id: &str,
    now: f64,
) -> rusqlite::Result<Option<Vec<HistoryEntry>>> {
    let live: bool = conn.query_row(
        crate::operations::sql::ALBUM_LIVE.text(),
        params![album_id],
        |row| row.get(0),
    )?;
    if !live {
        return Ok(None);
    }
    let rows: Vec<Row> = conn
        .prepare_cached(crate::operations::sql::ALBUM_HISTORY_ROWS.text())?
        .query_map(params![album_id, now - MAX_AGE_SECS], |row| {
            Ok(Row {
                id: row.get(0)?,
                actor: row.get(1)?,
                kind: row.get(2)?,
                reason: row.get(3)?,
                before: row.get(4)?,
                after: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let since = now - MAX_AGE_SECS;
    let mut entries = Vec::new();
    for row in rows {
        let at = (row.created_at * 1000.0) as i64;
        if let Some(kind) = album_event(&row) {
            let other_album_title = match kind {
                HistoryKind::UnfoldedFrom => {
                    let surviving = serde_json::from_str::<Unfolded>(&row.before)
                        .ok()
                        .map(|unfolded| unfolded.surviving_album_id);
                    album_title(conn, surviving.as_deref())?
                }
                _ => None,
            };
            entries.push(HistoryEntry {
                kind,
                at,
                by: person(row.actor.as_deref(), caller_user_id),
                track_count: 0,
                track_titles: Vec::new(),
                other_album_title,
                undo_id: None,
            });
            continue;
        }
        if row.kind == "continue_tracks" {
            let Ok(continued) = serde_json::from_str::<Continued>(&row.after) else {
                tracing::warn!(
                    action_id = row.id,
                    "album history skips an unreadable action"
                );
                continue;
            };
            for (kind, replaced) in [
                (HistoryKind::TracksContinued, false),
                (HistoryKind::TracksReplaced, true),
            ] {
                let tracks: BTreeSet<&str> = continued
                    .pairs
                    .iter()
                    .filter(|pair| {
                        pair.new_pair
                            && pair.replaced == replaced
                            && pair.basis.as_deref() != Some(SAME_FILE)
                    })
                    .map(|pair| pair.new_track_id.as_str())
                    .collect();
                if tracks.is_empty() {
                    continue;
                }
                entries.push(HistoryEntry {
                    kind,
                    at,
                    by: None,
                    track_count: tracks.len(),
                    track_titles: titles(conn, &tracks)?,
                    other_album_title: None,
                    undo_id: None,
                });
            }
            continue;
        }
        if row.kind == "other_music_at_path" {
            let Ok(replaced) = serde_json::from_str::<OtherMusic>(&row.after) else {
                tracing::warn!(
                    action_id = row.id,
                    "album history skips an unreadable action"
                );
                continue;
            };
            let tracks: BTreeSet<&str> = replaced
                .pairs
                .iter()
                .map(|pair| pair.old_track_id.as_str())
                .collect();
            if tracks.is_empty() {
                continue;
            }
            entries.push(HistoryEntry {
                kind: HistoryKind::TracksOtherMusic,
                at,
                by: None,
                track_count: tracks.len(),
                track_titles: titles(conn, &tracks)?,
                other_album_title: None,
                undo_id: None,
            });
            continue;
        }
        if row.kind == "new_copy_at_path" {
            let Ok(copies) = serde_json::from_str::<NewCopies>(&row.after) else {
                tracing::warn!(
                    action_id = row.id,
                    "album history skips an unreadable action"
                );
                continue;
            };
            let tracks: BTreeSet<&str> = copies
                .tracks
                .iter()
                .map(|copy| copy.track_id.as_str())
                .collect();
            if tracks.is_empty() {
                continue;
            }
            entries.push(HistoryEntry {
                kind: HistoryKind::TracksNewCopy,
                at,
                by: None,
                track_count: tracks.len(),
                track_titles: titles(conn, &tracks)?,
                other_album_title: None,
                undo_id: None,
            });
            continue;
        }
        let kind = match (row.kind.as_str(), row.reason.as_deref()) {
            ("merge_album", Some("RETAG_JOINED_ACROSS_SCANS" | "RETAG_JOINED_SAME_RELEASE")) => {
                HistoryKind::RetagJoined
            }
            ("retag_first_half", Some("RETAG_FIRST_HALF_LEFT_AS_IT_WAS")) => {
                HistoryKind::RetagHalfLeftAlone
            }
            ("retag_dismissal_ended", _) => HistoryKind::RetagDismissalEnded,
            (
                "merge_album",
                Some(
                    "AUTOMATIC_EMPTY_ALBUM_SHELL_CONVERGENCE"
                    | "AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR",
                ),
            ) => HistoryKind::LeftoverJoined,
            ("album_artist", Some(hygiene::ARTIST_REPAIR_REASON)) => {
                let undo_id = hygiene::artist_repair_undoable(conn, &row.id)?.then_some(row.id);
                entries.push(HistoryEntry {
                    kind: HistoryKind::ArtistFromTracks,
                    at,
                    by: None,
                    track_count: 0,
                    track_titles: Vec::new(),
                    other_album_title: None,
                    undo_id,
                });
                continue;
            }
            _ => continue,
        };
        let Ok(other) = serde_json::from_str::<OtherAlbum>(&row.before) else {
            tracing::warn!(
                action_id = row.id,
                "album history skips an unreadable action"
            );
            continue;
        };
        let other_id = other
            .retired_album_id
            .or(other.from_album_id)
            .or(other.source_album_id);
        let other_album_title = album_title(conn, other_id.as_deref())?;
        let by = match (kind, row.actor.as_deref()) {
            (HistoryKind::RetagJoined, Some(actor)) if actor == caller_user_id => {
                Some(HistoryActor::You)
            }
            (HistoryKind::RetagJoined, Some(_)) => Some(HistoryActor::Person),
            _ => None,
        };
        let fold = row.reason.as_deref() == Some(identity::FOLD_REASON);
        if fold && let Some(source) = other_id.as_deref() {
            entries.extend(fold_undone(
                conn,
                source,
                &row.id,
                other_album_title.clone(),
                caller_user_id,
                since,
            )?);
        }
        let undo_id = (fold && identity::fold_undoable(conn, &row.id)?).then_some(row.id);
        entries.push(HistoryEntry {
            kind,
            at,
            by,
            track_count: 0,
            track_titles: Vec::new(),
            other_album_title,
            undo_id,
        });
    }
    // Newest first, as the rows were read; entries read beside them slot in.
    entries.sort_by(|a, b| b.at.cmp(&a.at));
    entries.truncate(MAX_ENTRIES);
    Ok(Some(entries))
}

/// The other album of an undone fold, as the undo's row keeps it.
#[derive(Deserialize)]
struct Unfolded {
    surviving_album_id: String,
}

/// The kind of an album-level entry told by its row's kind alone, or
/// `None` for the kinds read further below.
fn album_event(row: &Row) -> Option<HistoryKind> {
    Some(match row.kind.as_str() {
        "automatic_exact_edition" => {
            match serde_json::from_str::<Option<PriorAlbumIdentity>>(&row.before) {
                Ok(Some(PriorAlbumIdentity {
                    release_mbid: Some(_),
                    ..
                })) => HistoryKind::EditionChanged,
                _ => HistoryKind::EditionChosen,
            }
        }
        "automatic_match_sealed" => return sealed_edition(row),
        "automatic_match_withdrawn" => HistoryKind::MatchWithdrawn,
        "undo_automatic_edition" => HistoryKind::EditionChangeUndone,
        identity::FOLD_UNDONE => HistoryKind::UnfoldedFrom,
        "management_hand_back_given_up" => HistoryKind::HandBackGivenUp,
        _ => return None,
    })
}

/// Who did a step a person may have asked for, as the caller may know it;
/// `None` for the app.
fn person(actor: Option<&str>, caller_user_id: &str) -> Option<HistoryActor> {
    match actor {
        None | Some("" | "system") => None,
        Some(actor) if actor == caller_user_id => Some(HistoryActor::You),
        Some(_) => Some(HistoryActor::Person),
    }
}

fn album_title(conn: &Connection, album_id: Option<&str>) -> rusqlite::Result<Option<String>> {
    let Some(album_id) = album_id else {
        return Ok(None);
    };
    conn.query_row(
        crate::operations::sql::ALBUM_TITLE.text(),
        params![album_id],
        |row| row.get(0),
    )
    .optional()
}

/// The undo of fold `fold_id`, recorded on the album it brought back
/// (`source`), as an entry of the album that had kept its files.
fn fold_undone(
    conn: &Connection,
    source: &str,
    fold_id: &str,
    source_title: Option<String>,
    caller_user_id: &str,
    since: f64,
) -> rusqlite::Result<Option<HistoryEntry>> {
    conn.prepare_cached(crate::operations::sql::FOLD_UNDONE.text())?
        .query_row(
            params![source, identity::FOLD_UNDONE, fold_id, since],
            |row| {
                let actor: Option<String> = row.get(0)?;
                let created_at: f64 = row.get(1)?;
                Ok(HistoryEntry {
                    kind: HistoryKind::FoldUndone,
                    at: (created_at * 1000.0) as i64,
                    by: person(actor.as_deref(), caller_user_id),
                    track_count: 0,
                    track_titles: Vec::new(),
                    other_album_title: source_title,
                    undo_id: None,
                })
            },
        )
        .optional()
}

/// The edition a seal moved the album to, as its audit row keeps it.
#[derive(Deserialize)]
struct Sealed {
    release_mbid: Option<String>,
}

/// The entry for an identification seal that changed the album row. Only
/// a change of edition tells something: a first match, a group-only seal
/// and the same release spelled otherwise have none.
fn sealed_edition(row: &Row) -> Option<HistoryKind> {
    let prior = serde_json::from_str::<Option<PriorAlbumIdentity>>(&row.before);
    let sealed = serde_json::from_str::<Sealed>(&row.after);
    let (Ok(prior), Ok(sealed)) = (prior, sealed) else {
        tracing::warn!(
            action_id = row.id,
            "album history skips an unreadable action"
        );
        return None;
    };
    let prior = prior.filter(|prior| prior.release_group_mbid.is_some())?;
    let release = sealed.release_mbid?;
    match prior.release_mbid {
        None => Some(HistoryKind::EditionChosen),
        Some(before) if before.eq_ignore_ascii_case(&release) => None,
        Some(_) => Some(HistoryKind::EditionChanged),
    }
}

/// The first titles of `tracks`, by disc and track number.
fn titles(conn: &Connection, tracks: &BTreeSet<&str>) -> rusqlite::Result<Vec<String>> {
    conn.prepare(&crate::operations::sql::titles_of_tracks(ListLen::new(
        tracks.len(),
    )))?
    .query_map(params_from_iter(tracks.iter()), |row| row.get(0))?
    .collect()
}
