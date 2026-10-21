//! The commands automatic work runs: identification's seal and its
//! retraction, and a recheck accepting an exact edition.
//!
//! Each runs inside the caller's transaction, never touches a person's
//! choice, and moves the catalog revision once when it changed a row. The
//! seal and the retraction act only on the files their facts were read
//! from: when the album's files changed since, they write nothing and
//! offer the album to identification again.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::sql;
use super::{DecisionSource, PROVIDER, Rows, delete_album_row};
use crate::catalog_revision::bump_catalog;
use crate::corrections::continuation::{Scope, continue_tracks};
use crate::corrections::release_join::join_replaced;
use crate::identify::models::{AutomaticSeal, MatchFlag, PriorAlbumIdentity, PriorTrackIdentity};
use crate::identify::sqlite::{
    album_identity_row, album_input_revision, offer_album, track_identity_rows,
};

/// What automatic work read the album at.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    /// The album's input revision, read with the facts the work decided on.
    pub input_revision: &'a str,
    /// Now, in seconds.
    pub now: f64,
}

/// How an automatic command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticOutcome {
    /// The album holds what the command decided.
    Done,
    /// A person's choice holds the album: nothing was written.
    PersonsChoice,
    /// The album's files changed since its facts were read: nothing was
    /// written and the album was offered to identification again.
    InputMoved,
}

/// Seal identification's match on the album: the album row, the rows of
/// the supported files it still holds (a person's track row stays), and
/// the match flag (cleared for a confident match, set for a best guess).
/// For an exact edition the rows it replaced are kept for undo. A person's
/// album row is never replaced.
pub fn seal_automatic(
    tx: &Transaction<'_>,
    seal: &AutomaticSeal,
    inputs: Inputs<'_>,
) -> rusqlite::Result<AutomaticOutcome> {
    let album = seal.local_album_id.as_str();
    if input_moved(tx, album, inputs)? {
        return Ok(AutomaticOutcome::InputMoved);
    }
    if album_source(tx, album)?.is_some_and(DecisionSource::is_persons_choice) {
        return Ok(AutomaticOutcome::PersonsChoice);
    }
    let before = tx.total_changes();
    let prior = album_identity_row(tx, album)?;
    let prior_tracks = track_identity_rows(tx, album)?;
    let now = inputs.now;
    let written = tx.prepare_cached(sql::SEAL_ALBUM.text())?.execute(params![
        album,
        PROVIDER,
        seal.release_group_mbid,
        seal.release_mbid,
        now
    ])?;
    if written != 1 {
        return Ok(AutomaticOutcome::PersonsChoice);
    }
    // The flag lands with the seal, so a best guess is never applied
    // without it (and a confident match never keeps a stale one).
    write_match_flag(tx, album, seal.flag.as_ref(), now)?;
    for track in &seal.tracks {
        let Some(recording) = track.recording_mbid.as_deref() else {
            continue;
        };
        tx.prepare_cached(sql::SEAL_TRACK.text())?.execute(params![
            track.local_track_id,
            PROVIDER,
            recording,
            seal.release_mbid,
            track.release_track_mbid,
            now,
            album,
        ])?;
    }
    if seal.release_mbid.is_some() {
        keep_automatic_undo(
            tx,
            album,
            &Kept {
                prior: prior.as_ref(),
                prior_tracks: &prior_tracks,
                job_id: None,
                reason_code: "AUTOMATIC_EXACT_EDITION",
                now,
            },
        )?;
    }
    join_replaced(tx, Scope::Album(album), now)?;
    continue_tracks(tx, Scope::Album(album), now)?;
    bump_if_changed(tx, before)?;
    Ok(AutomaticOutcome::Done)
}

/// Settle an album identification found no match for. When the new
/// evidence `contradicted` the album's revisable match, that match goes
/// (its track rows stay). An album left with no match is flagged
/// unmatched with `flag`. A person's match is never removed.
pub fn retract(
    tx: &Transaction<'_>,
    album_id: &str,
    contradicted: bool,
    flag: &MatchFlag,
    inputs: Inputs<'_>,
) -> rusqlite::Result<AutomaticOutcome> {
    if input_moved(tx, album_id, inputs)? {
        return Ok(AutomaticOutcome::InputMoved);
    }
    let before = tx.total_changes();
    if contradicted {
        delete_album_row(tx, album_id, Rows::Revisable)?;
    }
    let matched: bool = tx
        .prepare_cached(sql::HAS_ALBUM_ROW.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))?;
    if !matched {
        write_match_flag(tx, album_id, Some(flag), inputs.now)?;
    }
    bump_if_changed(tx, before)?;
    Ok(AutomaticOutcome::Done)
}

/// One file placed on a track of the accepted release.
#[derive(Debug, Clone)]
pub struct Placed {
    pub local_track_id: String,
    pub recording_mbid: String,
    pub release_track_mbid: String,
    pub disc: u32,
    pub position: u32,
}

/// An exact edition a recheck accepts for the album.
#[derive(Debug, Clone)]
pub struct Accepted<'a> {
    pub release_group_mbid: &'a str,
    pub release_mbid: &'a str,
    /// The album's files placed on the release.
    pub placed: &'a [Placed],
    /// The album's other files: they keep their recording, off any
    /// release track.
    pub extra: &'a [String],
    pub job_id: Option<&'a str>,
    pub reason_code: &'a str,
    pub now: f64,
}

/// Accept an exact edition a recheck found, as confirmed: the album row
/// and the placed files' rows become automatic on the release, the other
/// files leave their release tracks, a waiting placement and any match
/// flag go, and the rows it replaced are kept as the automatic-edition
/// undo. A person's choice, made now or carried over from v2, is never
/// replaced.
pub fn accept_automatic(
    tx: &Transaction<'_>,
    album_id: &str,
    accepted: &Accepted<'_>,
) -> rusqlite::Result<AutomaticOutcome> {
    if album_source(tx, album_id)?.is_some_and(DecisionSource::is_persons_choice) {
        return Ok(AutomaticOutcome::PersonsChoice);
    }
    let before = tx.total_changes();
    let prior = album_identity_row(tx, album_id)?;
    let prior_tracks = track_identity_rows(tx, album_id)?;
    let now = accepted.now;
    tx.prepare_cached(sql::ACCEPT_ALBUM.text())?
        .execute(params![
            album_id,
            PROVIDER,
            accepted.release_group_mbid,
            accepted.release_mbid,
            now
        ])?;
    for placement in accepted.placed {
        tx.prepare_cached(sql::PLACE_TRACK.text())?
            .execute(params![
                placement.local_track_id,
                PROVIDER,
                placement.recording_mbid,
                accepted.release_mbid,
                placement.release_track_mbid,
                placement.disc,
                placement.position,
                now,
                album_id,
            ])?;
    }
    for track_id in accepted.extra {
        tx.prepare_cached(sql::UNPLACE_TRACK.text())?
            .execute(params![track_id, now, PROVIDER, album_id])?;
    }
    tx.prepare_cached(sql::DROP_REMAP.text())?
        .execute(params![album_id])?;
    // A confident match: applied fully, so no unconfirmed flag remains.
    write_match_flag(tx, album_id, None, now)?;
    keep_automatic_undo(
        tx,
        album_id,
        &Kept {
            prior: prior.as_ref(),
            prior_tracks: &prior_tracks,
            job_id: accepted.job_id,
            reason_code: accepted.reason_code,
            now,
        },
    )?;
    bump_if_changed(tx, before)?;
    Ok(AutomaticOutcome::Done)
}

/// Whether the album's files changed since `inputs` were read. When they
/// did, the album is offered to identification again on this transaction
/// (a job already waiting takes the new files).
fn input_moved(tx: &Transaction<'_>, album_id: &str, inputs: Inputs<'_>) -> rusqlite::Result<bool> {
    if album_input_revision(tx, album_id)? == inputs.input_revision {
        return Ok(false);
    }
    offer_album(tx, album_id, (inputs.now * 1000.0) as i64)?;
    Ok(true)
}

/// Who decided the album's match; `None` when it has none. A value no
/// source has is a decode error.
fn album_source(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<Option<DecisionSource>> {
    tx.prepare_cached(sql::ALBUM_SOURCE.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))
        .optional()
}

/// Record (or with `None`, clear) the album's match flag.
pub(crate) fn write_match_flag(
    tx: &Transaction<'_>,
    album_id: &str,
    flag: Option<&MatchFlag>,
    now: f64,
) -> rusqlite::Result<usize> {
    match flag {
        None => tx
            .prepare_cached(sql::CLEAR_FLAG.text())?
            .execute(params![album_id]),
        Some(flag) => {
            let candidates = serde_json::to_string(&flag.candidates)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
            tx.prepare_cached(sql::SET_FLAG.text())?.execute(params![
                album_id,
                flag.state.as_str(),
                flag.reason_code,
                flag.release_mbid,
                candidates,
                now,
            ])
        }
    }
}

/// What an automatic edition replaced, kept for its undo.
struct Kept<'a> {
    prior: Option<&'a PriorAlbumIdentity>,
    prior_tracks: &'a [PriorTrackIdentity],
    job_id: Option<&'a str>,
    reason_code: &'a str,
    now: f64,
}

fn keep_automatic_undo(
    tx: &Transaction<'_>,
    album_id: &str,
    kept: &Kept<'_>,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::KEEP_AUTOMATIC_UNDO.text())?
        .execute(params![
            uuid::Uuid::new_v4().to_string(),
            kept.prior.map(to_json).transpose()?,
            to_json(&kept.prior_tracks)?,
            kept.now,
            album_id,
            PROVIDER,
            kept.job_id,
            kept.reason_code,
        ])?;
    Ok(())
}

fn to_json<T: serde::Serialize + ?Sized>(value: &T) -> rusqlite::Result<String> {
    serde_json::to_string(value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))
}

fn bump_if_changed(tx: &Transaction<'_>, before: u64) -> rusqlite::Result<()> {
    if tx.total_changes() != before {
        bump_catalog(tx)?;
    }
    Ok(())
}
