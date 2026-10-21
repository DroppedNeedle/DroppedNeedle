//! Editions waiting for their files to be placed.
//!
//! A queue row names the release the album's files go on. `album_pin` and
//! `group_pin` rows are a person's choice waiting for MusicBrainz's
//! tracklist: running one places the files as a choice would and leaves
//! the album's match, its source, its revision and its undo as they are. A
//! `place_only` row is for an album that already has its edition, one a
//! person chose or v2 carried, and holds files that are not on it yet (a
//! merge, a move, a retag the scan followed): it puts the files on the
//! release and changes nothing else. Whatever its origin, a row runs only
//! while the album's match is still a person's and names that release;
//! otherwise it is dropped unrun.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::person::{Fit, Offer, drop_satellite, fit, place, write_choice_tracks};
use super::reads::indexed_track_ids;
use super::sql;
use super::{IdentityError, PROVIDER, Refusal, TrackRow, upsert_track_identity};
use crate::corrections::continuation::{Scope, continue_tracks};
use crate::corrections::release_join::join_replaced;

/// Why an album waits in the remap queue (the `origin` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemapOrigin {
    /// A person chose this album's edition.
    AlbumPin,
    /// The choice came from a release-group pin fanned out over every
    /// copy, not from a person pinning this album.
    GroupPin,
    /// The album has its edition; only its files are placed on it.
    PlaceOnly,
}

impl RemapOrigin {
    pub const ALL: [Self; 3] = [Self::AlbumPin, Self::GroupPin, Self::PlaceOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlbumPin => "album_pin",
            Self::GroupPin => "group_pin",
            Self::PlaceOnly => "place_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|origin| origin.as_str() == value)
    }
}

/// A stored origin this version does not know is an error, never a
/// default.
impl rusqlite::types::FromSql for RemapOrigin {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::parse(text).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(format!("unknown remap origin {text:?}").into())
        })
    }
}

/// One album whose edition still needs its files placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRemap {
    pub local_album_id: String,
    pub release_mbid: String,
    pub chosen_by_user_id: Option<String>,
    pub attempts: i64,
    pub origin: RemapOrigin,
}

/// First wait, in seconds, after MusicBrainz did not answer; it doubles
/// with each try up to [`REMAP_RETRY_MAX_SECS`].
pub const REMAP_RETRY_SECS: f64 = 300.0;
/// Longest wait between tries while MusicBrainz is down (six hours).
pub const REMAP_RETRY_MAX_SECS: f64 = 21_600.0;

/// Reason code on an album whose chosen edition fits none of its files.
pub const CHOSEN_FITS_NO_FILE: &str = super::person::CHOSEN_FITS_NO_FILE;

/// Reason code on an album whose chosen edition MusicBrainz no longer
/// lists, so its files cannot be placed on it.
pub const CHOSEN_NOT_LISTED: &str = "CHOSEN_EDITION_NOT_LISTED";

/// The next album due for a remap, if any.
pub fn next_remap(conn: &Connection, now: f64) -> rusqlite::Result<Option<PendingRemap>> {
    conn.prepare_cached(sql::NEXT_REMAP.text())?
        .query_row(params![now], |row| {
            Ok(PendingRemap {
                local_album_id: row.get(0)?,
                release_mbid: row.get(1)?,
                chosen_by_user_id: row.get(2)?,
                attempts: row.get(3)?,
                origin: row.get(4)?,
            })
        })
        .optional()
}

/// Whether the queued remap is still the album's latest ask. A person's
/// newer choice or hand-back removes or replaces the queue row, and the
/// worker must then leave the album alone.
pub fn remap_still_wanted(conn: &Connection, pending: &PendingRemap) -> rusqlite::Result<bool> {
    let queued: Option<(String, RemapOrigin)> = conn
        .prepare_cached(sql::REMAP_ASK.text())?
        .query_row(params![pending.local_album_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(queued.is_some_and(|(release, origin)| {
        release.eq_ignore_ascii_case(&pending.release_mbid) && origin == pending.origin
    }))
}

/// Whether the album's match is still a person's (chosen by hand or
/// carried from v2) and names the queued release. When it is not, a
/// person changed the album since the row was queued ("leave unmanaged",
/// a custom edition, "Let DroppedNeedle choose"), and that later word
/// stands: the row has nothing left to do.
pub fn remap_still_holds(conn: &Connection, pending: &PendingRemap) -> rusqlite::Result<bool> {
    conn.prepare_cached(sql::REMAP_HOLDS.text())?.query_row(
        params![pending.local_album_id, PROVIDER, pending.release_mbid],
        |row| row.get(0),
    )
}

/// Drop a queued remap that no longer holds ([`remap_still_holds`]),
/// writing nothing else.
pub fn drop_remap(tx: &Transaction<'_>, pending: &PendingRemap) -> rusqlite::Result<()> {
    tracing::info!(
        album = pending.local_album_id,
        release = pending.release_mbid,
        origin = pending.origin.as_str(),
        "the album's edition changed since its files were queued to be placed; dropped"
    );
    drop_satellite(tx, &pending.local_album_id, sql::DROP_REMAP)
}

/// MusicBrainz did not answer, or the album's files are briefly out of
/// reach (a scan moving them): try again later, waiting longer each time
/// (capped). This never gives the remap up.
pub fn retry_remap(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let steps = pending.attempts.clamp(0, 16) as i32;
    let wait = (REMAP_RETRY_SECS * 2f64.powi(steps)).min(REMAP_RETRY_MAX_SECS);
    tx.prepare_cached(sql::RETRY_REMAP.text())?
        .execute(params![pending.local_album_id, now + wait, code])?;
    Ok(())
}

/// The carried choice cannot be honoured: MusicBrainz does not know the
/// release, or a release-group pin's release fits none of this copy's
/// files. So the album goes back to automatic best fit, its unplaced
/// tracks with it, and identification is queued. Nothing a person changed
/// since is touched: only the carried choice and its own unplaced track
/// rows.
pub fn give_up_remap(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tracing::warn!(
        album = pending.local_album_id,
        release = pending.release_mbid,
        code,
        "the carried edition choice cannot hold this album's files; back to automatic choice"
    );
    let handed_back = tx
        .prepare_cached(sql::GIVE_UP_ALBUM.text())?
        .execute(params![
            pending.local_album_id,
            now,
            PROVIDER,
            pending.release_mbid
        ])?;
    if handed_back == 1 {
        tx.prepare_cached(sql::GIVE_UP_TRACKS.text())?
            .execute(params![pending.local_album_id, now, PROVIDER])?;
        crate::identify::queue::offer_album(tx, &pending.local_album_id, (now * 1000.0) as i64)?;
    }
    drop_satellite(tx, &pending.local_album_id, sql::DROP_REMAP)
}

/// A person chose this exact edition but its files cannot be placed on it
/// (`code`, logged). Their choice stays and the track placements stay as
/// they were; the album shows why (`state_code`), so they can pick another
/// edition or let DroppedNeedle choose.
pub fn keep_unplaceable_choice(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    code: &str,
    state_code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tracing::warn!(
        album = pending.local_album_id,
        release = pending.release_mbid,
        code,
        "the album's files cannot be placed on the chosen edition; the choice is kept"
    );
    tx.prepare_cached(sql::KEEP_UNPLACEABLE.text())?
        .execute(params![
            pending.local_album_id,
            state_code,
            pending.release_mbid,
            now
        ])?;
    drop_satellite(tx, &pending.local_album_id, sql::DROP_REMAP)
}

/// Queue the album's files to be placed on the release a person just
/// chose for it. It replaces any row queued for the album before, which
/// becomes this person's ask.
pub fn queue_remap(
    tx: &Transaction<'_>,
    album_id: &str,
    release_mbid: &str,
    chosen_by: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::QUEUE_REMAP.text())?
        .execute(params![
            album_id,
            release_mbid,
            chosen_by,
            now,
            RemapOrigin::AlbumPin.as_str()
        ])?;
    Ok(())
}

/// Queue the album's files to be placed on the edition it already has
/// (`place_only`), when all of these hold: its match is a person's (chosen
/// by hand or carried from v2) and names a release, no custom edition is
/// active on it, and one of its indexed files is not placed on that
/// release. A row already queued for the album, of any origin, places the
/// files too and is left as it is. Returns true when those three hold,
/// whether the row was written now or was there already.
pub fn queue_placement(tx: &Transaction<'_>, album_id: &str, now: f64) -> rusqlite::Result<bool> {
    let release: Option<String> = tx
        .prepare_cached(sql::PLACEMENT_RELEASE.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))
        .optional()?;
    let Some(release) = release else {
        return Ok(false);
    };
    let unplaced: bool = tx
        .prepare_cached(sql::HAS_UNPLACED_FILE.text())?
        .query_row(params![album_id, PROVIDER, release], |row| row.get(0))?;
    if !unplaced {
        return Ok(false);
    }
    tx.prepare_cached(sql::QUEUE_PLACEMENT.text())?
        .execute(params![
            album_id,
            release,
            now,
            RemapOrigin::PlaceOnly.as_str()
        ])?;
    Ok(true)
}

/// Run an `album_pin` or `group_pin` row: place the album's files on the
/// chosen release as a choice places them (every placed file the person's,
/// with its position, naming the release as the album's identity row
/// spells it; the rest keep their recording, unplaced; files matched by
/// hand stay). The album's match, both undos, "leave
/// unmanaged" and the custom edition are left as they are; a match state
/// left by an earlier placement that failed (`CHOSEN_EDITION_*`) and the
/// queue row go, and automatic management looks at the album again. The
/// caller has checked the row still holds ([`remap_still_holds`]).
/// Returns the number of files placed and left unplaced.
pub fn place_choice(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    offer: &Offer,
    now: f64,
) -> Result<(usize, usize), IdentityError> {
    let album_id = pending.local_album_id.as_str();
    let fitted = fit(tx, album_id, offer)?;
    // The files name the release as the album's match spells it, which
    // MusicBrainz may list under a newer id now.
    let release: String = tx
        .prepare_cached(sql::ALBUM_RELEASE.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))?;
    write_choice_tracks(tx, album_id, &release, &fitted, now)?;
    tx.prepare_cached(sql::CLEAR_CHOSEN_FLAG.text())?
        .execute(params![album_id])?;
    drop_satellite(tx, album_id, sql::DROP_REMAP)?;
    crate::management::automatic::queue(tx, album_id, now)?;
    join_replaced(tx, Scope::Album(album_id), now)?;
    continue_tracks(tx, Scope::Album(album_id), now)?;
    let Fit { placed, extra, .. } = fitted;
    tracing::info!(
        album = album_id,
        placed = placed.len(),
        extra = extra.len(),
        "the album's files were placed on its chosen edition"
    );
    Ok((placed.len(), extra.len()))
}

/// Run a `place_only` row: put the album's files on the edition it
/// already has, and change nothing else. The caller has checked that the
/// album's match is still a person's and names the queued release
/// ([`remap_still_holds`]). An album with an active custom edition has
/// nothing to place: the row is dropped.
///
/// A file already placed on the release keeps its row as it is, and the
/// release track it holds is never given to another file (a second copy
/// of it, merged in, stays unplaced). The other files the offer weighed
/// are placed as an edition choice places them ([`place`]), each upserted
/// as a `manual` placement naming the release as the album's identity row
/// spells it; the rest keep their recording, unplaced. A match state left
/// by an earlier placement that failed (`CHOSEN_EDITION_*`) goes. A file
/// that joined the album after the offer was made keeps its row, and the
/// queue row stays so the next try places it. Otherwise the queue row goes
/// and automatic management looks at the album again, keeping a scan's
/// stamp; it still honours "leave unmanaged", the taken-over mark and the
/// stamp before it writes anything.
///
/// The album identity row (source, chooser, time, release and revision),
/// both undo records, "leave unmanaged", the custom edition, the taken-over
/// mark and the stamp are left as they are, and no audit row is written.
/// No credit proof is written either, as for any person's match: the proof
/// for a person's match is the album's accepted release, which reconcile
/// can use once the files are placed on it.
///
/// `EditionFitsNoFile` when no file is on the release, before or after,
/// and none waits to be weighed; `AlbumNotFound` when the album has no
/// indexed file. Returns the number of files placed now and of files left
/// unplaced.
pub fn place_files(
    tx: &Transaction<'_>,
    pending: &PendingRemap,
    offer: &Offer,
    now: f64,
) -> Result<(usize, usize), IdentityError> {
    let album_id = pending.local_album_id.as_str();
    let custom: bool = tx
        .prepare_cached(sql::HAS_CUSTOM.text())?
        .query_row(params![album_id], |row| row.get(0))?;
    if custom {
        // A custom edition lists the album's own tracks; nothing is placed.
        drop_remap(tx, pending)?;
        return Ok((0, 0));
    }
    let release: String = tx
        .prepare_cached(sql::ALBUM_RELEASE.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))?;
    let indexed = indexed_track_ids(tx, album_id)?;
    if indexed.is_empty() {
        return Err(Refusal::AlbumNotFound.into());
    }
    let held: Vec<(String, String)> = tx
        .prepare_cached(sql::HELD_ON_RELEASE.text())?
        .query_map(params![album_id, PROVIDER, release], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    // A file that joined the album after MusicBrainz was asked was not
    // weighed: it keeps its row, and the queue row stays for the next try.
    let (others, unseen): (Vec<String>, Vec<String>) = indexed
        .into_iter()
        .filter(|track| !held.iter().any(|(placed, _)| placed == track))
        .partition(|track| offer.weighed.iter().any(|weighed| weighed == track));
    let (offered, mut extra) = place(&others, &offer.spots);
    let mut placed = Vec::with_capacity(offered.len());
    for placement in offered {
        if held
            .iter()
            .any(|(_, taken)| taken.eq_ignore_ascii_case(&placement.release_track_mbid))
        {
            extra.push(placement.local_track_id);
        } else {
            placed.push(placement);
        }
    }
    if placed.is_empty() && held.is_empty() && unseen.is_empty() {
        return Err(Refusal::EditionFitsNoFile.into());
    }
    for placement in &placed {
        upsert_track_identity(
            tx,
            &TrackRow {
                local_track_id: &placement.local_track_id,
                recording_mbid: &placement.recording_mbid,
                release_mbid: Some(&release),
                release_track_mbid: Some(&placement.release_track_mbid),
                medium_position: Some(placement.disc),
                release_track_position: Some(placement.position),
                decision_source: "manual",
            },
            now,
        )?;
    }
    for track_id in &extra {
        tx.prepare_cached(sql::UNPLACE_CHOSEN_TRACK.text())?
            .execute(params![track_id, now, PROVIDER, album_id])?;
    }
    tx.prepare_cached(sql::CLEAR_CHOSEN_FLAG.text())?
        .execute(params![album_id])?;
    if unseen.is_empty() {
        drop_satellite(tx, album_id, sql::DROP_REMAP)?;
        crate::management::automatic::queue(tx, album_id, now)?;
    }
    join_replaced(tx, Scope::Album(album_id), now)?;
    continue_tracks(tx, Scope::Album(album_id), now)?;
    tracing::info!(
        album = album_id,
        placed = placed.len(),
        extra = extra.len(),
        waiting = unseen.len(),
        "the album's files were placed on its edition"
    );
    Ok((placed.len(), extra.len()))
}
