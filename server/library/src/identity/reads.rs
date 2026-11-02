//! Reads of an album's identity: its match row and its files' rows, the
//! revisions an undo is keyed to, the stored release document, and the
//! library copy of one release group, which the catalog and acquisition
//! read alike ([`copy_for_group`]).

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest as _, Sha256};

use super::sql;
use super::{DecisionSource, PROVIDER};
use crate::identify::models::{
    AlbumIdentity, PriorAlbumIdentity, PriorTrackIdentity, TrackIdentity,
};
use crate::matching::Release;

/// The entity kind release documents are stored under.
pub(crate) const RELEASE_KIND: &str = "release";

/// The shape of a stored release document; a new shape is a new hash, so
/// older documents are fetched again rather than misread.
pub(crate) const RELEASE_DOCUMENT_VERSION: &str = "matching-release-v2";

/// The input hash every stored release document carries.
pub(crate) fn release_input_hash() -> String {
    Sha256::digest(RELEASE_DOCUMENT_VERSION.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The raw album identity row, every column the undo puts back.
pub fn album_identity_row(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Option<PriorAlbumIdentity>> {
    conn.prepare_cached(sql::ALBUM_ROW.text())?
        .query_row(params![local_album_id, PROVIDER], sql::PriorAlbumRow::read)
        .map(|row| PriorAlbumIdentity {
            release_group_mbid: row.release_group_mbid,
            release_mbid: row.release_mbid,
            decision_source: row.decision_source,
            matcher_version: row.matcher_version,
            attempt_id: row.attempt_id,
            selected_by_user_id: row.selected_by_user_id,
            selected_at: row.selected_at.map(|at| crate::clock::secs(at.0)),
            row_revision: row.row_revision,
        })
        .optional()
}

/// The raw identity rows of the album's indexed tracks, by track id.
pub fn track_identity_rows(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Vec<PriorTrackIdentity>> {
    conn.prepare_cached(sql::TRACK_ROWS.text())?
        .query_map(params![local_album_id, PROVIDER], prior_track)?
        .collect()
}

/// One file's raw identity row, if it has one.
pub(crate) fn track_row(
    conn: &Connection,
    local_track_id: &str,
) -> rusqlite::Result<Option<PriorTrackIdentity>> {
    conn.prepare_cached(sql::TRACK_ROW.text())?
        .query_row(params![local_track_id, PROVIDER], prior_track)
        .optional()
}

fn prior_track(row: &rusqlite::Row<'_>) -> rusqlite::Result<PriorTrackIdentity> {
    let row = sql::PriorTrackRow::read(row)?;
    Ok(PriorTrackIdentity {
        local_track_id: row.local_track_id,
        recording_mbid: row.recording_mbid,
        release_mbid: row.release_mbid,
        release_track_mbid: row.release_track_mbid,
        medium_position: row.medium_position,
        release_track_position: row.release_track_position,
        decision_source: row.decision_source,
        attempt_id: row.attempt_id,
        selected_at: row.selected_at.map(|at| crate::clock::secs(at.0)),
    })
}

/// One album's match, on `conn`.
pub(crate) fn read_album_identity(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Option<AlbumIdentity>> {
    conn.prepare_cached(sql::ALBUM_IDENTITY.text())?
        .query_row(
            params![local_album_id, PROVIDER],
            sql::AlbumIdentityRow::read,
        )
        .map(|row| AlbumIdentity {
            local_album_id: local_album_id.to_owned(),
            provider: PROVIDER.to_owned(),
            release_group_mbid: Some(row.release_group_mbid),
            release_mbid: row.release_mbid,
            decision_source: row.decision_source,
            row_revision: row.row_revision as u64,
        })
        .optional()
}

/// One track's match, on `conn`.
pub(crate) fn read_track_identity(
    conn: &Connection,
    local_track_id: &str,
) -> rusqlite::Result<Option<TrackIdentity>> {
    conn.prepare_cached(sql::TRACK_IDENTITY.text())?
        .query_row(
            params![local_track_id, PROVIDER],
            sql::TrackIdentityRow::read,
        )
        .map(|row| TrackIdentity {
            local_track_id: local_track_id.to_owned(),
            provider: PROVIDER.to_owned(),
            recording_mbid: Some(row.recording_mbid),
            release_track_mbid: row.release_track_mbid,
            decision_source: row.decision_source,
            row_revision: row.row_revision as u64,
        })
        .optional()
}

/// The album identity's row revision, 0 when the album has none.
pub fn identity_revision(conn: &Connection, album_id: &str) -> rusqlite::Result<i64> {
    Ok(conn
        .prepare_cached(sql::IDENTITY_REVISION.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))
        .optional()?
        .unwrap_or(0))
}

/// A hash over the album's match and its indexed files' matches, every
/// column of each: any seal, retraction or person's decision moves it. A
/// repair keeps it as the identity it checked.
pub fn identity_hash(conn: &Connection, album_id: &str) -> rusqlite::Result<String> {
    let mut hasher = Sha256::new();
    let album = conn
        .prepare_cached(sql::ALBUM_HASH_ROW.text())?
        .query_row(params![album_id, PROVIDER], sql::AlbumHashRow::read)
        .optional()?;
    if let Some(album) = album {
        hasher.update(
            format!(
                "album\0{}\0{}\0{}\0{}\n",
                album.release_group_mbid,
                album.release_mbid.unwrap_or_default(),
                album.decision_source.as_str(),
                album.row_revision,
            )
            .as_bytes(),
        );
    }
    let mut stmt = conn.prepare_cached(sql::TRACK_HASH_ROWS.text())?;
    for row in stmt.query_map(params![album_id, PROVIDER], sql::TrackHashRow::read)? {
        let row = row?;
        let line = format!(
            "track\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\n",
            row.id,
            row.recording_mbid,
            row.release_mbid.unwrap_or_default(),
            row.release_track_mbid.unwrap_or_default(),
            row.medium_position.unwrap_or_default(),
            row.release_track_position.unwrap_or_default(),
            row.decision_source.as_str(),
            row.row_revision,
        );
        hasher.update(line.as_bytes());
    }
    Ok(hasher
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The album's indexed files, by id.
pub(crate) fn indexed_track_ids(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(sql::INDEXED_TRACKS.text())?
        .query_map(params![album_id], |row| row.get(0))?
        .collect()
}

/// The album's indexed files placed on a track of `release_mbid`, the
/// files writing that edition's tags reaches.
pub(crate) fn mapped_track_ids(
    conn: &Connection,
    album_id: &str,
    release_mbid: &str,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(sql::MAPPED_TRACKS.text())?
        .query_map(params![album_id, release_mbid, PROVIDER], |row| row.get(0))?
        .collect()
}

/// The album's indexed files whose match by hand stands, with the release
/// track each holds.
pub(crate) fn hand_matches_standing(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached(sql::HAND_MATCHES_STANDING.text())?
        .query_map(params![album_id, PROVIDER], |row| {
            let found = sql::HandMatchStanding::read(row)?;
            Ok((found.local_track_id, found.release_track_mbid))
        })?
        .collect()
}

/// The revisions an administrator echoes to undo the album's automatic
/// edition, while the undo still applies.
pub fn live_automatic_edition_undo(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<(i64, i64)>> {
    conn.prepare_cached(sql::LIVE_AUTOMATIC_UNDO.text())?
        .query_row(params![album_id, PROVIDER], sql::LiveAutomaticUndo::read)
        .map(|undo| {
            (
                undo.expected_post_album_revision,
                undo.expected_post_identity_revision,
            )
        })
        .optional()
}

/// Whether the album has an edition change that can still be undone.
pub fn undo_available(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(sql::UNDO_AVAILABLE.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))
}

/// The release document on file for `mbid`, fetched at or after `oldest`
/// (seconds). `None` when there is none, or it does not decode.
pub(crate) fn stored_release(
    conn: &Connection,
    mbid: &str,
    oldest: f64,
) -> rusqlite::Result<Option<Release>> {
    let payload: Option<String> = conn
        .prepare_cached(sql::STORED_RELEASE.text())?
        .query_row(
            params![
                PROVIDER,
                RELEASE_KIND,
                mbid.trim().to_ascii_lowercase(),
                release_input_hash(),
                crate::clock::ms(oldest),
            ],
            |row| row.get(0),
        )
        .optional()?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    match serde_json::from_str(&payload) {
        Ok(release) => Ok(Some(release)),
        Err(error) => {
            tracing::warn!(%error, release_mbid = mbid, "stored release document did not decode");
            Ok(None)
        }
    }
}

/// The copy of a release group the edition reads see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyEdition {
    /// The library album that is the copy.
    pub album_id: String,
    /// The release its identity names, as stored. `None` when it names none
    /// (left unmanaged).
    pub release_mbid: Option<String>,
    /// Who chose the identity.
    pub source: DecisionSource,
    /// Indexed tracks the copy holds.
    pub file_count: i64,
}

/// The copy of `release_group_mbid` the library shows, over live albums, so
/// a download joins the copy a person sees: a copy with files first, then
/// one picked by hand (`manual`), then the oldest. The group matches
/// ignoring ASCII case. `None` when no live copy is identified as the group.
pub fn copy_for_group(
    conn: &Connection,
    release_group_mbid: &str,
) -> rusqlite::Result<Option<CopyEdition>> {
    conn.prepare_cached(sql::COPY_SHOWN.text())?
        .query_row(
            params![release_group_mbid.to_ascii_lowercase(), PROVIDER],
            sql::CopyRow::read,
        )
        .map(|copy| CopyEdition {
            album_id: copy.local_album_id,
            release_mbid: copy.release_mbid,
            source: copy.decision_source,
            file_count: copy.file_count,
        })
        .optional()
}
