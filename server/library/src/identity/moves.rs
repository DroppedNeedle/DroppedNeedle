//! Identity writes other areas' work makes: an edition moving between
//! albums when tracks are regrouped, files placed on an album's edition by
//! a scan, a repair or an import, a contribution's link, and the row of a
//! file an import replaced.
//!
//! Each runs in the caller's transaction, which moves the catalog revision
//! as its own work requires; the rules of when to call them stay with the
//! callers.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::sql;
use super::{DecisionSource, PROVIDER, Rows, delete_album_row};

/// Move `from`'s edition, with its match state and pending remap, onto
/// `to`. What `to` held goes first: its edition and both its undo rows
/// (they name revisions the moved row could reach), and its match state
/// or queued placement where `from` has one to take its place.
pub(crate) fn move_edition(tx: &Transaction<'_>, from: &str, to: &str) -> rusqlite::Result<()> {
    delete_album_row(tx, to, Rows::Any)?;
    tx.prepare_cached(sql::MOVE_EDITION_ROW.text())?
        .execute(params![from, to, PROVIDER])?;
    for (room, moved) in [
        (sql::MAKE_ROOM_FOR_MATCH_STATE, sql::MOVE_MATCH_STATE),
        (sql::MAKE_ROOM_FOR_REMAP, sql::MOVE_REMAP),
    ] {
        tx.prepare_cached(room.text())?.execute(params![from, to])?;
        tx.prepare_cached(moved.text())?
            .execute(params![from, to])?;
    }
    Ok(())
}

/// Take each of `track_ids` off any release track that is not on
/// `release`; with no release, off every release track.
pub(crate) fn settle_tracks(
    tx: &Transaction<'_>,
    release: Option<&str>,
    track_ids: &[String],
) -> rusqlite::Result<()> {
    let mut statement = tx.prepare_cached(sql::SETTLE_TRACK.text())?;
    for track_id in track_ids {
        statement.execute(params![PROVIDER, release, track_id])?;
    }
    Ok(())
}

/// One file placed on a release track by its own ids.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Spot<'a> {
    pub local_track_id: &'a str,
    pub recording_mbid: &'a str,
    pub release_track_mbid: &'a str,
    pub disc: Option<u32>,
    pub position: Option<u32>,
}

/// Place files a scan filed onto the album on their tracks of `release`,
/// the album's edition. The rows are the person's when a person chose the
/// edition (now or in v2), else automatic. A flag left by a choice that
/// fitted no file goes.
pub(crate) fn place_scanned(
    tx: &Transaction<'_>,
    album_id: &str,
    release: &str,
    spots: &[Spot<'_>],
    now: f64,
) -> rusqlite::Result<()> {
    let source: Option<DecisionSource> = tx
        .prepare_cached(sql::ALBUM_SOURCE.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))
        .optional()?;
    let source = if source.is_some_and(DecisionSource::is_persons_choice) {
        DecisionSource::Manual
    } else {
        DecisionSource::Automatic
    };
    for spot in spots {
        write_track(tx, spot, release, source, now)?;
    }
    tx.prepare_cached(sql::CLEAR_FLAG_WITH_REASON.text())?
        .execute(params![album_id, super::person::CHOSEN_FITS_NO_FILE])?;
    Ok(())
}

/// Place files a repair mapped onto `release`, keeping the album's
/// decision source on each row: an automatic album stays automatic and a
/// chosen one stays chosen. `false`, writing nothing, when the album's
/// match no longer names `release`.
pub(crate) fn place_mapping(
    tx: &Transaction<'_>,
    album_id: &str,
    release: &str,
    spots: &[Spot<'_>],
    now: f64,
) -> rusqlite::Result<bool> {
    let source: Option<DecisionSource> = tx
        .prepare_cached(sql::SOURCE_NAMING_RELEASE.text())?
        .query_row(params![album_id, release, PROVIDER], |row| row.get(0))
        .optional()?;
    let Some(source) = source else {
        return Ok(false);
    };
    for spot in spots {
        write_track(tx, spot, release, source, now)?;
    }
    Ok(true)
}

fn write_track(
    tx: &Transaction<'_>,
    spot: &Spot<'_>,
    release: &str,
    source: DecisionSource,
    now: f64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::UPSERT_TRACK.text())?
        .execute(params![
            spot.local_track_id,
            PROVIDER,
            spot.recording_mbid,
            release,
            spot.release_track_mbid,
            spot.disc,
            spot.position,
            source,
            now,
        ])?;
    Ok(())
}

/// Clear the flag a chosen edition's placement left (`CHOSEN_EDITION_*`)
/// once an import placed files on that edition.
pub(crate) fn clear_chosen_flag(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::CLEAR_CHOSEN_FLAG.text())?
        .execute(params![album_id])?;
    Ok(())
}

/// A contribution's link of an album to a release, as a person's choice.
pub(crate) struct Link<'a> {
    pub album_id: &'a str,
    pub release_group_mbid: &'a str,
    pub release_mbid: &'a str,
    pub matcher_version: &'a str,
    pub attempt_id: &'a str,
    pub selected_by: Option<&'a str>,
}

/// One file of a linked album and its track on the release, as v2 wrote
/// it.
pub(crate) struct LinkedTrack<'a> {
    pub local_track_id: &'a str,
    pub recording_mbid: &'a str,
    pub release_track_mbid: Option<&'a str>,
    pub disc: Option<i64>,
    pub position: Option<i64>,
}

/// Write a contribution's link: the album's match as the person's, when
/// it has none (`album` true), and each paired file's track on the
/// release, a person's decision. A file that left the album is skipped;
/// any file it still holds is written, indexed or not, as v2 did.
pub(crate) fn link(
    tx: &Transaction<'_>,
    link: &Link<'_>,
    album: bool,
    tracks: &[LinkedTrack<'_>],
    now: f64,
) -> rusqlite::Result<()> {
    if album {
        tx.prepare_cached(sql::LINK_ALBUM.text())?.execute(params![
            link.album_id,
            PROVIDER,
            link.release_group_mbid,
            link.release_mbid,
            link.matcher_version,
            link.attempt_id,
            link.selected_by,
            now,
        ])?;
    }
    let mut statement = tx.prepare_cached(sql::LINK_TRACK.text())?;
    for track in tracks {
        statement.execute(params![
            track.local_track_id,
            PROVIDER,
            track.recording_mbid,
            link.release_mbid,
            track.release_track_mbid,
            track.disc,
            track.position,
            link.attempt_id,
            now,
            link.album_id,
        ])?;
    }
    Ok(())
}

/// Forget the match of the file a track row held before an import put a
/// new file in its place: it described the file that was replaced.
pub(crate) fn release_track(conn: &Connection, local_track_id: &str) -> rusqlite::Result<()> {
    conn.prepare_cached(sql::DELETE_TRACK_ROW.text())?
        .execute(params![local_track_id, PROVIDER])?;
    Ok(())
}
