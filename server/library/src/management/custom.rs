//! Custom editions: an album sealed by hand as "these files, under this
//! release group", with no MusicBrainz release behind it. Management
//! plans such an album from its sealed manifest, as v2 did: the album and
//! track facts frozen when it was sealed stand in for a release, and no
//! release or release-track MBID is ever written.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::sql::{CustomManifest, CustomTrack};
use crate::matching::{CreditedArtist, Genre, Release, ReleaseMedium, ReleaseTrack};

/// Release ids of custom editions start with this, followed by the
/// manifest id; they are never written to a file.
pub const CUSTOM_PREFIX: &str = "custom:";

/// True for a release built from a custom edition manifest.
pub fn is_custom(release: &Release) -> bool {
    release.id.starts_with(CUSTOM_PREFIX)
}

/// The active custom edition of an album, as a release whose tracks are
/// keyed by library track id. `None` when the album has none, or when its
/// files no longer match the manifest (a file added or gone since it was
/// sealed): it must be sealed again first.
pub fn active(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Release>> {
    let manifest = conn
        .query_row(
            crate::management::sql::ACTIVE_CUSTOM_EDITION.text(),
            params![album_id],
            CustomManifest::read,
        )
        .optional()?;
    let Some(CustomManifest {
        id: manifest_id,
        release_group_mbid: group,
        album_title: title,
        album_artist_name: album_artist,
        artist_mbid,
        album_metadata_json: album_json,
    }) = manifest
    else {
        return Ok(None);
    };
    let album: serde_json::Value = serde_json::from_str(&album_json).unwrap_or_default();
    let mut statement = conn.prepare(crate::management::sql::CUSTOM_EDITION_TRACKS.text())?;
    let rows = statement.query_map(params![manifest_id], CustomTrack::read)?;
    let mut tracks = Vec::new();
    for (index, row) in rows.enumerate() {
        let CustomTrack {
            local_track_id: id,
            title: track_title,
            artist_name: artist,
            artist_mbid: artist_id,
            disc_number: disc,
            track_number: position,
            duration_seconds: seconds,
            metadata_json: meta,
            recording_mbid: recording,
        } = row?;
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap_or_default();
        tracks.push(ReleaseTrack {
            id,
            recording_id: recording.unwrap_or_default(),
            title: track_title,
            artists: vec![CreditedArtist {
                id: artist_id.unwrap_or_default(),
                name: artist,
                sort_name: meta["artist_sort"].as_str().map(str::to_owned),
                join: String::new(),
            }],
            disc: u32::try_from(disc).unwrap_or(1),
            position: u32::try_from(position).unwrap_or(1),
            absolute_position: u32::try_from(index + 1).unwrap_or(u32::MAX),
            length_ms: seconds
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| (seconds * 1000.0).round() as u64),
            isrcs: Vec::new(),
            genres: meta["genre"]
                .as_str()
                .filter(|genre| !genre.trim().is_empty())
                .map(|genre| Genre {
                    name: genre.to_owned(),
                    count: None,
                })
                .into_iter()
                .collect(),
        });
    }
    // The manifest must still be exactly the album's files.
    let mut sealed: Vec<&str> = tracks.iter().map(|track| track.id.as_str()).collect();
    sealed.sort_unstable();
    let mut statement = conn.prepare(crate::management::sql::ALBUM_INDEXED_TRACK_IDS.text())?;
    let current: Vec<String> = statement
        .query_map(params![album_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if tracks.is_empty()
        || current
            .iter()
            .map(String::as_str)
            .ne(sealed.iter().copied())
    {
        return Ok(None);
    }
    let mut discs: Vec<u32> = tracks.iter().map(|track| track.disc).collect();
    discs.sort_unstable();
    discs.dedup();
    let media = discs
        .iter()
        .map(|disc| ReleaseMedium {
            position: *disc,
            track_count: tracks.iter().filter(|track| track.disc == *disc).count() as u32,
            ..ReleaseMedium::default()
        })
        .collect();
    let year = album["year"].as_i64().map(|year| format!("{year:04}"));
    Ok(Some(Release {
        id: format!("{CUSTOM_PREFIX}{manifest_id}"),
        release_group_id: group,
        title,
        artists: vec![CreditedArtist {
            id: artist_mbid.unwrap_or_default(),
            name: album_artist,
            sort_name: album["album_artist_sort_name"].as_str().map(str::to_owned),
            join: String::new(),
        }],
        date: year,
        original_date: album["original_release_date"].as_str().map(str::to_owned),
        primary_type: None,
        media,
        tracks,
        genres: album["primary_genre"]
            .as_str()
            .filter(|genre| !genre.trim().is_empty())
            .map(|genre| Genre {
                name: genre.to_owned(),
                count: None,
            })
            .into_iter()
            .collect(),
        secondary_types: if album["is_compilation"].as_bool() == Some(true) {
            vec!["Compilation".to_owned()]
        } else {
            Vec::new()
        },
        ..Release::default()
    }))
}
