//! The match an import brings: the album and track rows its download
//! names, written in the publisher's catalog commit that adds its files.
//!
//! The seal reads each file's release identity from the journaled bundle,
//! whether it joined the album it was fetched for from the commit's own
//! join (which recovery rebuilds from the import's provenance record), and
//! the rows already in the database, so a commit that recovery resumes
//! seals the same way. The release document was saved before the publish; here
//! it only says which ids name the import's release.

use droppedneedle_kernel::db::savepoint;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::reads::{read_album_identity, read_track_identity, stored_release};
use super::sql;
use super::{DecisionSource, PROVIDER, write_match_flag};
use crate::corrections::continuation::{Scope, continue_tracks};
use crate::identify::models::{AlbumIdentity, TrackIdentity};
use crate::identify::queue::offer_album;
use crate::publish::planner::ReleaseIdentity;

/// One file an import's bundle added to the catalog.
#[derive(Debug, Clone, Copy)]
pub struct ImportedFile<'a> {
    pub track_id: &'a str,
    /// The release track the import matched the file to. An extra file
    /// has an empty recording.
    pub identity: &'a ReleaseIdentity,
    /// The file joined the library album it was fetched for.
    pub joined: bool,
}

/// The album an import's files make up: the one holding most of them,
/// the first in plan order on a tie. The files normally all land on one
/// album; a file whose tags could not be written is filed by its own and
/// may land elsewhere. `None` when no file is in the catalog.
pub fn import_album(conn: &Connection, track_ids: &[&str]) -> rusqlite::Result<Option<String>> {
    let mut albums = Vec::with_capacity(track_ids.len());
    for track_id in track_ids {
        albums.push(track_album(conn, track_id)?);
    }
    Ok(most_held(&albums))
}

/// Seal what an import's download names, on the commit's transaction.
///
/// - The album the files make up ([`import_album`]) gets an automatic row
///   on the import's release and its match flag is cleared, unless a
///   person decided its match, it already names another release (it keeps
///   it, and the files keep only their recordings), or the files joined it
///   (a few fetched tracks never confirm the album's match).
/// - Each file with a recording gets an automatic track row where no
///   person decided it. A file on another album keeps only its recording,
///   and that album is offered to identification.
/// - Every album that took a file is queued for automatic management.
///
/// The seal runs under a savepoint. A row it reads that does not decode
/// rolls back only the seal: the files stay, the failure is logged and the
/// album is offered to identification. Any other error fails the commit,
/// and with it the whole import. The caller's commit already moved the
/// catalog revision, so nothing here moves it again.
pub fn adopt_import(
    tx: &Transaction<'_>,
    files: &[ImportedFile<'_>],
    now: f64,
) -> rusqlite::Result<()> {
    let mut albums = Vec::with_capacity(files.len());
    for file in files {
        albums.push(track_album(tx, file.track_id)?);
    }
    let Some(album) = most_held(&albums) else {
        return Ok(());
    };
    let now_ms = (now * 1000.0) as i64;
    let mut queued: Vec<&str> = Vec::new();
    for other in albums.iter().flatten() {
        if queued.contains(&other.as_str()) {
            continue;
        }
        queued.push(other);
        crate::management::automatic::queue(tx, other, now)?;
        if *other != album {
            offer_album(tx, other, now_ms)?;
        }
    }
    let on_album: Vec<bool> = albums
        .iter()
        .map(|on| on.as_deref() == Some(album.as_str()))
        .collect();
    let sealed = savepoint(tx, "adopt_import", || {
        seal(tx, &album, files, &on_album, now)
    })?;
    match sealed {
        Ok(()) => Ok(()),
        Err(error) if does_not_decode(&error) => {
            tracing::error!(
                %error,
                album,
                "the import could not read the album's match; nothing of it is written \
                 and the album is offered to identification"
            );
            offer_album(tx, &album, now_ms)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// The seal itself; see [`adopt_import`]. `on_album[i]` says whether
/// `files[i]` is on `album`.
fn seal(
    tx: &Transaction<'_>,
    album: &str,
    files: &[ImportedFile<'_>],
    on_album: &[bool],
    now: f64,
) -> rusqlite::Result<()> {
    let Some(first) = files.first() else {
        return Ok(());
    };
    let release = first.identity;
    let current = read_album_identity(tx, album)?;
    let mut tracks = Vec::with_capacity(files.len());
    for file in files {
        tracks.push(if file.identity.recording_mbid.is_empty() {
            None
        } else {
            Some(read_track_identity(tx, file.track_id)?)
        });
    }
    let stored = stored_release(tx, &release.release_mbid, 0.0)?;
    if current
        .as_ref()
        .is_some_and(|row| !row.decision_source.is_revisable())
    {
        return Ok(());
    }
    let answers = |held: &str| match &stored {
        Some(document) => document.answers_to(held),
        None => release.release_mbid.eq_ignore_ascii_case(held),
    };
    // An album that already has an exact edition keeps it: a download
    // matched to another edition adds its files without flipping the
    // album. A whole-album download verified against the album's own
    // edition confirms an unsure match.
    let held = current
        .as_ref()
        .and_then(|row| row.release_mbid.clone())
        .filter(|held| !answers(held));
    let joined = files
        .iter()
        .zip(on_album)
        .any(|(file, on)| *on && file.joined);
    let mut wrote = false;
    if held.is_some() {
        tracing::info!(
            album,
            release = release.release_mbid,
            "download matched another edition; the album keeps its own"
        );
    } else if !joined {
        wrote |= save_album(
            tx,
            &AlbumIdentity {
                local_album_id: album.to_owned(),
                provider: PROVIDER.to_owned(),
                release_group_mbid: Some(release.release_group_mbid.clone()),
                release_mbid: Some(release.release_mbid.clone()),
                decision_source: DecisionSource::Automatic,
                row_revision: current.as_ref().map_or(1, |row| row.row_revision + 1),
            },
            now,
        )? > 0;
        wrote |= write_match_flag(tx, album, None, now)? > 0;
    }
    for ((file, current), on) in files.iter().zip(&tracks).zip(on_album) {
        let Some(current) = current else {
            continue;
        };
        if current
            .as_ref()
            .is_some_and(|row| !row.decision_source.is_revisable())
        {
            continue;
        }
        wrote |= save_track(
            tx,
            &TrackIdentity {
                local_track_id: file.track_id.to_owned(),
                provider: PROVIDER.to_owned(),
                recording_mbid: Some(file.identity.recording_mbid.clone()),
                // A track of another edition, or on another album, keeps
                // only its recording.
                release_track_mbid: (held.is_none() && *on)
                    .then(|| file.identity.release_track_mbid.clone()),
                decision_source: DecisionSource::Automatic,
                row_revision: current.as_ref().map_or(1, |row| row.row_revision + 1),
            },
            now,
        )? > 0;
    }
    // Once, after the last identity and flag write.
    if wrote {
        continue_tracks(tx, Scope::Album(album), now)?;
    }
    Ok(())
}

/// Save an album's match at `now`, never an automatic or embedded row
/// over a person's (the statement's own guard). Rows written: 0 when a
/// person's row holds the album.
pub(crate) fn save_album(
    tx: &Transaction<'_>,
    identity: &AlbumIdentity,
    now: f64,
) -> rusqlite::Result<usize> {
    let Some(group) = identity.release_group_mbid.as_deref() else {
        tracing::warn!(
            album = identity.local_album_id,
            "album identity without a release group not stored"
        );
        return Ok(0);
    };
    let written = tx
        .prepare_cached(sql::SAVE_ALBUM_IDENTITY.text())?
        .execute(params![
            identity.local_album_id,
            PROVIDER,
            group,
            identity.release_mbid,
            identity.decision_source.as_str(),
            now,
            identity.row_revision.max(1) as i64,
        ])?;
    if written == 0 {
        tracing::warn!(
            album = identity.local_album_id,
            "automatic identity not written over a person's album identity"
        );
    }
    Ok(written)
}

/// Save a track's match at `now`, guarded like the album's.
pub(crate) fn save_track(
    tx: &Transaction<'_>,
    identity: &TrackIdentity,
    now: f64,
) -> rusqlite::Result<usize> {
    let Some(recording) = identity.recording_mbid.as_deref() else {
        tracing::warn!(
            track = identity.local_track_id,
            "track identity without a recording not stored"
        );
        return Ok(0);
    };
    let written = tx
        .prepare_cached(sql::SAVE_TRACK_IDENTITY.text())?
        .execute(params![
            identity.local_track_id,
            PROVIDER,
            recording,
            identity.release_track_mbid,
            identity.decision_source.as_str(),
            now,
            identity.row_revision.max(1) as i64,
        ])?;
    if written == 0 {
        tracing::warn!(
            track = identity.local_track_id,
            "automatic identity not written over a person's track identity"
        );
    }
    Ok(written)
}

/// The album a track is on, if it is in the catalog.
pub(crate) fn track_album(conn: &Connection, track_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(sql::TRACK_ALBUM.text())?
        .query_row(params![track_id], |row| row.get(0))
        .optional()
}

/// The album most of `albums` name, the first named on a tie.
fn most_held(albums: &[Option<String>]) -> Option<String> {
    let mut best: Option<(&str, usize)> = None;
    for album in albums.iter().flatten() {
        let count = albums
            .iter()
            .filter(|other| other.as_deref() == Some(album.as_str()))
            .count();
        if best.is_none_or(|(_, most)| count > most) {
            best = Some((album, count));
        }
    }
    best.map(|(album, _)| album.to_owned())
}

/// A value read back that does not decode: a column of the wrong type, or
/// a value no Rust type here admits (an unknown decision source).
fn does_not_decode(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::FromSqlConversionFailure(..) | rusqlite::Error::InvalidColumnType(..)
    )
}
