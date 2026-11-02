//! The library and playlist reads behind radio: owned tracks by artist or
//! album and the artists on one playlist.

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::{Db, DbError};
use rusqlite::OptionalExtension as _;
use rusqlite::types::Value;

use crate::discover::models::RadioPlanTrack;
use crate::discover::{read_all, sql};

fn track(row: sql::RadioTrackColumns) -> RadioPlanTrack {
    RadioPlanTrack {
        track_name: if row.title.is_empty() {
            "Unknown".to_owned()
        } else {
            row.title
        },
        artist_name: if row.artist_name.is_empty() {
            "Unknown".to_owned()
        } else {
            row.artist_name
        },
        artist_mbid: if row.artist_mbid.is_empty() {
            row.album_artist_mbid
        } else {
            row.artist_mbid
        },
        recording_mbid: row.recording_mbid,
        album_mbid: row.release_group_mbid,
        album_name: row.album_title,
        in_library: true,
        local_file_id: Some(row.id),
        file_format: row.file_format.map(|format| format.to_ascii_lowercase()),
        duration_s: row.duration_seconds,
    }
}

/// Lowercased ids, then the limit.
fn id_binds<'a>(ids: impl Iterator<Item = &'a String>, limit: i64) -> Vec<Value> {
    ids.map(|id| Value::Text(id.to_ascii_lowercase()))
        .chain([Value::Integer(limit.max(1))])
        .collect()
}

/// Reads over the catalog database.
#[derive(Clone)]
pub struct RadioLibrary {
    db: Db,
}

impl RadioLibrary {
    /// Reads over this database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Up to `limit` owned tracks, in random order, whose track artist or
    /// album artist is one of `artist_mbids`. Tracks of unidentified
    /// albums are left out.
    pub async fn tracks_by_artists(
        &self,
        artist_mbids: &[String],
        limit: i64,
    ) -> Result<Vec<RadioPlanTrack>, String> {
        if artist_mbids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = read_all(
            &self.db,
            "discover.radio.tracks_by_artists",
            sql::radio_tracks_by_artists(ListLen::new(artist_mbids.len())),
            rusqlite::params_from_iter(id_binds(artist_mbids.iter().chain(artist_mbids), limit)),
            sql::RadioTrackColumns::read,
        )
        .await
        .map_err(|error| format!("radio library tracks: {error}"))?;
        Ok(rows.into_iter().map(track).collect())
    }

    /// Up to `limit` owned tracks of these albums, album by album in the
    /// given order, then disc and track order.
    pub async fn tracks_of_albums(
        &self,
        release_group_mbids: &[String],
        limit: i64,
    ) -> Result<Vec<RadioPlanTrack>, String> {
        if release_group_mbids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = read_all(
            &self.db,
            "discover.radio.tracks_of_albums",
            sql::radio_tracks_of_albums(ListLen::new(release_group_mbids.len())),
            rusqlite::params_from_iter(id_binds(release_group_mbids.iter(), limit)),
            sql::RadioTrackColumns::read,
        )
        .await
        .map_err(|error| format!("radio album tracks: {error}"))?;
        let mut tracks: Vec<RadioPlanTrack> = rows.into_iter().map(track).collect();
        let order = |track: &RadioPlanTrack| {
            let album = track.album_mbid.as_deref().unwrap_or("");
            release_group_mbids
                .iter()
                .position(|mbid| mbid.eq_ignore_ascii_case(album))
                .unwrap_or(release_group_mbids.len())
        };
        // A stable sort keeps the disc and track order inside each album.
        tracks.sort_by_key(order);
        Ok(tracks)
    }

    /// The playlist's distinct artist MBIDs and its track count, when the
    /// caller may read it: their own playlists and public ones. Anything
    /// else reads as missing, admins included, so a private playlist never
    /// leaks.
    pub async fn playlist_artists(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<(Vec<String>, i64)>, String> {
        let (playlist, user) = (playlist_id.to_owned(), user_id.to_owned());
        let visible: Option<String> = self
            .db
            .read("discover.radio.playlist", move |conn| {
                let visible = conn
                    .query_row(
                        sql::VISIBLE_PLAYLIST.text(),
                        rusqlite::params![playlist, user],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(visible)
            })
            .await
            .map_err(|error: DbError| format!("playlist read: {error}"))?;
        if visible.is_none() {
            return Ok(None);
        }
        let rows: Vec<Option<String>> = read_all(
            &self.db,
            "discover.radio.playlist_tracks",
            sql::PLAYLIST_TRACK_ARTISTS.text(),
            (playlist_id.to_owned(),),
            |row| row.get(0),
        )
        .await
        .map_err(|error| format!("playlist tracks read: {error}"))?;
        let count = rows.len() as i64;
        let mut artists: Vec<String> = Vec::new();
        for artist in rows.into_iter().flatten() {
            let artist = artist.trim().to_ascii_lowercase();
            if !artist.is_empty() && !artists.contains(&artist) {
                artists.push(artist);
            }
        }
        Ok(Some((artists, count)))
    }
}
