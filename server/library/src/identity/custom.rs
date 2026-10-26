//! A custom edition: a person seals the album's files as they are under a
//! release group, with a versioned manifest of every file.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::person::{TrackRow, drop_satellite, seal_persons_album, upsert_track_identity};
use super::sql;
use super::undo::{record_and_keep, snapshot};
use super::{IdentityError, PROVIDER, Refusal};
use crate::catalog_revision::bump_catalog;
use crate::operations::decisions::settle_reviews;

/// What a person seals as a custom edition.
#[derive(Debug, Clone, Copy)]
pub struct Custom<'a> {
    pub release_group_mbid: &'a str,
    /// The review candidate the edition came from, settled as approved.
    pub candidate_key: &'a str,
    /// The artist MusicBrainz credits the release to, when it names one.
    pub artist_mbid: Option<&'a str>,
    /// A recording the evidence supports, per file.
    pub recordings: &'a [(String, String)],
    /// The album's input revision the caller checked; the manifest records
    /// it.
    pub input_revision: &'a str,
}

/// The album and file metadata a custom edition manifest freezes.
#[derive(Serialize)]
struct ManifestAlbum {
    album_artist_sort_name: Option<String>,
    year: Option<i64>,
    original_release_date: Option<String>,
    primary_genre: Option<String>,
    is_compilation: bool,
}

#[derive(Serialize, Deserialize)]
struct ManifestTrackMeta {
    album_artist_name: Option<String>,
    year: Option<i64>,
    genre: Option<String>,
    title_sort: Option<String>,
    artist_sort: Option<String>,
    album_sort: Option<String>,
    album_artist_sort: Option<String>,
    disc_subtitle: Option<String>,
    is_compilation: bool,
}

/// One indexed file as the manifest records it.
struct ManifestTrack {
    id: String,
    row_revision: i64,
    stat_revision: String,
    tag_revision: String,
    title: String,
    artist_name: String,
    album_title: String,
    disc_number: i64,
    track_number: i64,
    artist_mbid: Option<String>,
    file_format: String,
    duration_seconds: Option<f64>,
    meta: ManifestTrackMeta,
}

/// A person seals the album's files as they are under the release group:
/// the album row becomes theirs on the group with no release, each file
/// keeps a supported recording (or the one it had) off any release track,
/// and a new manifest becomes the album's active custom edition. A queued
/// placement and the match state go; "leave unmanaged" stays. The album's
/// open reviews settle as approved and automatic management looks again.
/// Refused, with nothing written, when the release belongs to another
/// release group than the album's match, the artist's match names another
/// artist, or the files lack the names and positions a manifest needs.
pub fn custom_edition(
    tx: &Transaction<'_>,
    album_id: &str,
    custom: &Custom<'_>,
    actor: &str,
    now: f64,
) -> Result<(), IdentityError> {
    let before = snapshot(tx, album_id)?;
    if before.identity.as_ref().is_some_and(|identity| {
        identity
            .release_group_mbid
            .as_deref()
            .is_some_and(|group| !group.eq_ignore_ascii_case(custom.release_group_mbid))
    }) {
        return Err(Refusal::CustomEditionGroupConflict.into());
    }
    let (title, album_artist, artist_mbid, album_meta, album_revision): (
        String,
        String,
        Option<String>,
        ManifestAlbum,
        i64,
    ) = tx
        .prepare_cached(sql::CUSTOM_ALBUM_FACTS.text())?
        .query_row(params![album_id, PROVIDER], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                ManifestAlbum {
                    album_artist_sort_name: row.get(3)?,
                    year: row.get(4)?,
                    original_release_date: row.get(5)?,
                    primary_genre: row.get(6)?,
                    is_compilation: row.get::<_, i64>(7)? != 0,
                },
                row.get(8)?,
            ))
        })?;
    if title.trim().is_empty() || album_artist.trim().is_empty() {
        return Err(Refusal::CustomEditionNamesMissing.into());
    }
    if let (Some(known), Some(offered)) = (artist_mbid.as_deref(), custom.artist_mbid)
        && !known.eq_ignore_ascii_case(offered)
    {
        return Err(Refusal::CustomEditionArtistConflict.into());
    }
    let tracks = manifest_tracks(tx, album_id)?;
    let mut positions: Vec<(i64, i64)> = tracks
        .iter()
        .map(|track| (track.disc_number, track.track_number))
        .collect();
    positions.sort_unstable();
    let unique = positions.windows(2).all(|pair| pair[0] != pair[1]);
    if tracks.is_empty()
        || !unique
        || positions
            .iter()
            .any(|(disc, track)| *disc < 1 || *track < 1)
    {
        return Err(Refusal::CustomEditionPositions.into());
    }

    seal_persons_album(
        tx,
        album_id,
        custom.release_group_mbid,
        None,
        Some(actor),
        now,
    )?;
    // Supported recordings from the evidence; otherwise a file keeps the
    // recording it already had, without an edition.
    for track in &tracks {
        let offered = custom
            .recordings
            .iter()
            .find(|(file, _)| *file == track.id)
            .map(|(_, recording)| recording.clone());
        let kept: Option<String> = tx
            .prepare_cached(sql::TRACK_RECORDING.text())?
            .query_row(params![track.id, PROVIDER], |row| row.get(0))
            .optional()?;
        match offered.or(kept) {
            Some(recording) => {
                upsert_track_identity(
                    tx,
                    &TrackRow {
                        local_track_id: &track.id,
                        recording_mbid: &recording,
                        release_mbid: None,
                        release_track_mbid: None,
                        medium_position: None,
                        release_track_position: None,
                        decision_source: "manual",
                    },
                    now,
                )?;
            }
            None => {
                tx.prepare_cached(sql::DELETE_TRACK_ROW.text())?
                    .execute(params![track.id, PROVIDER])?;
            }
        }
    }
    write_manifest(
        tx,
        album_id,
        custom,
        &Frozen {
            title: &title,
            album_artist: &album_artist,
            artist_mbid: artist_mbid.as_deref(),
            album_meta: &album_meta,
            album_revision,
            tracks: &tracks,
        },
        actor,
        now,
    )?;
    // The match state goes now that the album has its match.
    for statement in [sql::DROP_REMAP, sql::CLEAR_FLAG] {
        drop_satellite(tx, album_id, statement)?;
    }
    // Automatic management looks at the album again, as after a choice.
    crate::management::automatic::queue(tx, album_id, now)?;
    settle_reviews(
        tx,
        album_id,
        "approved",
        Some(custom.candidate_key),
        actor,
        now,
    )?;
    record_and_keep(
        tx,
        album_id,
        &before,
        actor,
        "custom_edition",
        "CUSTOM_EDITION_SEALED",
        false,
        now,
    )?;
    bump_catalog(tx)?;
    Ok(())
}

/// What the manifest freezes of the album as it was read.
struct Frozen<'a> {
    title: &'a str,
    album_artist: &'a str,
    artist_mbid: Option<&'a str>,
    album_meta: &'a ManifestAlbum,
    album_revision: i64,
    tracks: &'a [ManifestTrack],
}

/// Write the manifest and its tracks, and make it the album's active
/// custom edition. Returns the manifest id.
fn write_manifest(
    tx: &Transaction<'_>,
    album_id: &str,
    custom: &Custom<'_>,
    frozen: &Frozen<'_>,
    actor: &str,
    now: f64,
) -> Result<String, IdentityError> {
    let identity_revision: i64 = tx
        .prepare_cached(sql::IDENTITY_REVISION.text())?
        .query_row(params![album_id, PROVIDER], |row| row.get(0))?;
    let version: i64 = tx
        .prepare_cached(sql::NEXT_MANIFEST_VERSION.text())?
        .query_row(params![album_id], |row| row.get(0))?;
    let manifest_id = uuid::Uuid::new_v4().to_string();
    let album_json = super::undo::to_json(frozen.album_meta)?;
    let mut hasher = Sha256::new();
    hasher.update(
        format!(
            "{album_id}\0{version}\0{}\0{}\0{}\0{album_json}\0{}\n",
            custom.release_group_mbid, frozen.title, frozen.album_artist, custom.input_revision
        )
        .as_bytes(),
    );
    for track in frozen.tracks {
        hasher.update(
            format!(
                "{}\0{}\0{}\0{}\0{}\n",
                track.id,
                track.stat_revision,
                track.tag_revision,
                track.disc_number,
                track.track_number
            )
            .as_bytes(),
        );
    }
    let content_hash: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    tx.prepare_cached(sql::INSERT_MANIFEST.text())?
        .execute(params![
            manifest_id,
            album_id,
            version,
            custom.release_group_mbid,
            frozen.title,
            frozen.album_artist,
            frozen.artist_mbid,
            album_json,
            frozen.album_revision,
            identity_revision,
            custom.input_revision,
            content_hash,
            custom.candidate_key,
            actor,
            now,
        ])?;
    let mut ordered: Vec<&ManifestTrack> = frozen.tracks.iter().collect();
    ordered.sort_by(|a, b| {
        (a.disc_number, a.track_number, &a.id).cmp(&(b.disc_number, b.track_number, &b.id))
    });
    for (ordinal, track) in ordered.into_iter().enumerate() {
        let identity: Option<(String, i64)> = tx
            .prepare_cached(sql::TRACK_RECORDING_REVISION.text())?
            .query_row(params![track.id, PROVIDER], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let meta = super::undo::to_json(&track.meta)?;
        tx.prepare_cached(sql::INSERT_MANIFEST_TRACK.text())?
            .execute(params![
                manifest_id,
                ordinal as i64,
                track.id,
                track.row_revision,
                identity.as_ref().map(|(_, revision)| *revision),
                track.stat_revision,
                track.tag_revision,
                track.title,
                track.artist_name,
                track.album_title,
                frozen.album_artist,
                track.disc_number,
                track.track_number,
                identity.map(|(recording, _)| recording),
                track.artist_mbid,
                frozen.artist_mbid,
                meta,
                track.file_format,
                track.duration_seconds,
            ])?;
    }
    tx.prepare_cached(sql::ACTIVATE_CUSTOM.text())?
        .execute(params![album_id, manifest_id, now])?;
    Ok(manifest_id)
}

fn manifest_tracks(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<ManifestTrack>> {
    conn.prepare_cached(sql::MANIFEST_TRACKS.text())?
        .query_map(params![album_id], |row| {
            Ok(ManifestTrack {
                id: row.get(0)?,
                row_revision: row.get(1)?,
                stat_revision: row.get(2)?,
                tag_revision: row.get(3)?,
                title: row.get(4)?,
                artist_name: row.get(5)?,
                album_title: row.get(6)?,
                disc_number: row.get(7)?,
                track_number: row.get(8)?,
                artist_mbid: row.get(9)?,
                file_format: row.get(10)?,
                duration_seconds: row.get(11)?,
                meta: ManifestTrackMeta {
                    album_artist_name: row.get(12)?,
                    year: row.get(13)?,
                    genre: row.get(14)?,
                    title_sort: row.get(15)?,
                    artist_sort: row.get(16)?,
                    album_sort: row.get(17)?,
                    album_artist_sort: row.get(18)?,
                    disc_subtitle: row.get(19)?,
                    is_compilation: row.get::<_, i64>(20)? != 0,
                },
            })
        })?
        .collect()
}
