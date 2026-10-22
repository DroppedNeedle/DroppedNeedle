//! Reads of an album's identity: its match row and its files' rows, the
//! revisions an undo is keyed to, the stored release document, and the
//! library copy of one release group, as the catalog and acquisition read
//! it ([`copy_for_group`], which differ only in the [`Pick`] they pass).

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
        .query_row(params![local_album_id, PROVIDER], |row| {
            Ok(PriorAlbumIdentity {
                release_group_mbid: row.get(0)?,
                release_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                matcher_version: row.get(3)?,
                attempt_id: row.get(4)?,
                selected_by_user_id: row.get(5)?,
                selected_at: row.get(6)?,
                row_revision: row.get(7)?,
            })
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
    Ok(PriorTrackIdentity {
        local_track_id: row.get(0)?,
        recording_mbid: row.get(1)?,
        release_mbid: row.get(2)?,
        release_track_mbid: row.get(3)?,
        medium_position: row.get(4)?,
        release_track_position: row.get(5)?,
        decision_source: row.get(6)?,
        attempt_id: row.get(7)?,
        selected_at: row.get(8)?,
    })
}

/// One album's match, on `conn`.
pub(crate) fn read_album_identity(
    conn: &Connection,
    local_album_id: &str,
) -> rusqlite::Result<Option<AlbumIdentity>> {
    conn.prepare_cached(sql::ALBUM_IDENTITY.text())?
        .query_row(params![local_album_id, PROVIDER], |row| {
            Ok(AlbumIdentity {
                local_album_id: local_album_id.to_owned(),
                provider: PROVIDER.to_owned(),
                release_group_mbid: Some(row.get(0)?),
                release_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                row_revision: row.get::<_, i64>(3)? as u64,
            })
        })
        .optional()
}

/// One track's match, on `conn`.
pub(crate) fn read_track_identity(
    conn: &Connection,
    local_track_id: &str,
) -> rusqlite::Result<Option<TrackIdentity>> {
    conn.prepare_cached(sql::TRACK_IDENTITY.text())?
        .query_row(params![local_track_id, PROVIDER], |row| {
            Ok(TrackIdentity {
                local_track_id: local_track_id.to_owned(),
                provider: PROVIDER.to_owned(),
                recording_mbid: Some(row.get(0)?),
                release_track_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                row_revision: row.get::<_, i64>(3)? as u64,
            })
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
    let album: Option<(String, Option<String>, String, i64)> = conn
        .prepare_cached(sql::ALBUM_HASH_ROW.text())?
        .query_row(params![album_id, PROVIDER], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    if let Some((group, release, source, revision)) = album {
        hasher.update(
            format!(
                "album\0{group}\0{}\0{source}\0{revision}\n",
                release.unwrap_or_default()
            )
            .as_bytes(),
        );
    }
    let mut stmt = conn.prepare_cached(sql::TRACK_HASH_ROWS.text())?;
    let mut rows = stmt.query(params![album_id, PROVIDER])?;
    while let Some(row) = rows.next()? {
        let line = format!(
            "track\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\n",
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            row.get::<_, Option<i64>>(4)?.unwrap_or_default(),
            row.get::<_, Option<i64>>(5)?.unwrap_or_default(),
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)?,
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
            Ok((row.get(0)?, row.get(1)?))
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
        .query_row(params![album_id, PROVIDER], |row| {
            Ok((row.get(0)?, row.get(1)?))
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
                oldest,
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

/// Which copy [`copy_for_group`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// The copy the library shows, whatever its release. A copy that names
    /// no release (left unmanaged) still counts.
    Shown,
    /// The first copy that names a release: the one acquisition can fetch.
    WithRelease,
}

/// One copy of a release group, as the edition reads see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyEdition {
    /// The release its identity names, as stored. `None` when it names none.
    pub release_mbid: Option<String>,
    /// Who chose the identity.
    pub source: DecisionSource,
    /// Indexed tracks the copy holds.
    pub file_count: i64,
}

/// The copy of `release_group_mbid` that `pick` names, over live albums.
/// The group matches ignoring ASCII case. A copy picked by hand (`manual`)
/// comes first, then the oldest. `None` when no live copy is identified as
/// the group.
pub fn copy_for_group(
    conn: &Connection,
    release_group_mbid: &str,
    pick: Pick,
) -> rusqlite::Result<Option<CopyEdition>> {
    let statement = match pick {
        Pick::Shown => sql::COPY_SHOWN,
        Pick::WithRelease => sql::COPY_WITH_RELEASE,
    };
    conn.prepare_cached(statement.text())?
        .query_row(
            params![release_group_mbid.to_ascii_lowercase(), PROVIDER],
            |row| {
                Ok(CopyEdition {
                    release_mbid: row.get(0)?,
                    source: row.get(1)?,
                    file_count: row.get(2)?,
                })
            },
        )
        .optional()
}
