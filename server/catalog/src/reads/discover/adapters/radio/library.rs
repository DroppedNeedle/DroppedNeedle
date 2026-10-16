//! The library and playlist reads behind radio: owned tracks by artist or
//! album (v2 `get_files_by_artist_mbids` and
//! `get_files_by_release_group_mbids`) and the artists on one playlist.

use sqlx::{Row, SqlitePool};

use crate::reads::discover::models::RadioPlanTrack;

/// Owned, indexed tracks with their MusicBrainz ids. The artist and album
/// columns prefer the identified ids over the embedded tags.
const TRACK_SELECT: &str = "SELECT t.id AS id, t.title AS title, \
    COALESCE(NULLIF(t.artist_name, ''), t.album_artist_name, '') AS artist_name, \
    t.album_title AS album_title, t.file_format AS file_format, \
    t.duration_seconds AS duration_seconds, t.disc_number AS disc_number, \
    t.track_number AS track_number, \
    COALESCE(te.recording_mbid, t.embedded_recording_mbid) AS recording_mbid, \
    lower(COALESCE(tae.provider_artist_id, t.embedded_artist_mbid, '')) AS artist_mbid, \
    lower(COALESCE(aae.provider_artist_id, t.embedded_album_artist_mbid, '')) \
        AS album_artist_mbid, \
    lower(COALESCE(ae.release_group_mbid, t.embedded_release_group_mbid)) AS release_group_mbid \
    FROM local_tracks t \
    JOIN local_albums a ON a.id = t.local_album_id \
    LEFT JOIN local_track_artists ta ON ta.local_track_id = t.id AND ta.position = 0 \
    LEFT JOIN local_track_external_identities te ON te.local_track_id = t.id \
    LEFT JOIN local_artist_external_identities tae ON tae.local_artist_id = ta.local_artist_id \
    LEFT JOIN local_artist_external_identities aae ON aae.local_artist_id = a.album_artist_id \
    LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
    WHERE t.availability = 'indexed' AND a.retired_into_album_id IS NULL";

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

fn track(row: &sqlx::sqlite::SqliteRow) -> Result<RadioPlanTrack, sqlx::Error> {
    let title: String = row.try_get("title")?;
    let artist_name: String = row.try_get("artist_name")?;
    let artist_mbid: String = row.try_get("artist_mbid")?;
    let artist_mbid = if artist_mbid.is_empty() {
        row.try_get("album_artist_mbid")?
    } else {
        artist_mbid
    };
    Ok(RadioPlanTrack {
        track_name: if title.is_empty() {
            "Unknown".to_owned()
        } else {
            title
        },
        artist_name: if artist_name.is_empty() {
            "Unknown".to_owned()
        } else {
            artist_name
        },
        artist_mbid,
        recording_mbid: row.try_get("recording_mbid")?,
        album_mbid: row.try_get("release_group_mbid")?,
        album_name: row.try_get("album_title")?,
        in_library: true,
        local_file_id: Some(row.try_get("id")?),
        file_format: row
            .try_get::<Option<String>, _>("file_format")?
            .map(|format| format.to_ascii_lowercase()),
        duration_s: row.try_get("duration_seconds")?,
    })
}

/// Reads over the catalog pool.
#[derive(Clone)]
pub struct RadioLibrary {
    pool: SqlitePool,
}

impl RadioLibrary {
    /// Reads over this pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Up to `limit` owned tracks, in random order, whose track artist or
    /// album artist is one of `artist_mbids`. Tracks of unidentified
    /// albums are left out, as in v2.
    pub async fn tracks_by_artists(
        &self,
        artist_mbids: &[String],
        limit: i64,
    ) -> Result<Vec<RadioPlanTrack>, String> {
        if artist_mbids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = placeholders(artist_mbids.len());
        let sql = format!(
            "SELECT * FROM ({TRACK_SELECT}) WHERE release_group_mbid IS NOT NULL \
             AND (artist_mbid IN ({marks}) OR album_artist_mbid IN ({marks})) \
             ORDER BY RANDOM() LIMIT ?"
        );
        let mut query = sqlx::query(&sql);
        for mbid in artist_mbids.iter().chain(artist_mbids) {
            query = query.bind(mbid.to_ascii_lowercase());
        }
        let rows = query
            .bind(limit.max(1))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("radio library tracks: {error}"))?;
        rows.iter()
            .map(track)
            .collect::<Result<_, _>>()
            .map_err(|error| format!("radio library row: {error}"))
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
        let marks = placeholders(release_group_mbids.len());
        let sql = format!(
            "SELECT * FROM ({TRACK_SELECT}) \
             WHERE release_group_mbid IN ({marks}) \
             ORDER BY disc_number, track_number, id LIMIT ?"
        );
        let mut query = sqlx::query(&sql);
        for mbid in release_group_mbids {
            query = query.bind(mbid.to_ascii_lowercase());
        }
        let rows = query
            .bind(limit.max(1))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("radio album tracks: {error}"))?;
        let mut tracks: Vec<RadioPlanTrack> = rows
            .iter()
            .map(track)
            .collect::<Result<_, _>>()
            .map_err(|error| format!("radio album row: {error}"))?;
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
    /// leaks (v2 `analyse_playlist_profile`).
    pub async fn playlist_artists(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<(Vec<String>, i64)>, String> {
        let visible: Option<String> = sqlx::query_scalar(
            "SELECT id FROM playlists WHERE id = ?1 AND (user_id = ?2 OR is_public = 1)",
        )
        .bind(playlist_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| format!("playlist read: {error}"))?;
        if visible.is_none() {
            return Ok(None);
        }
        let rows: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT artist_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position",
        )
        .bind(playlist_id)
        .fetch_all(&self.pool)
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
