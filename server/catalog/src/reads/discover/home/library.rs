//! What home and the genre page read from the local library: the newest
//! albums, the artists with the most albums, the user's recent plays, the
//! owned artists and albums of one genre, and the albums whose art can
//! make up a genre's collage.
//!
//! Only albums with at least one indexed track count, and retired albums
//! and artists never show.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::reads::discover::page::library::fold;
use crate::reads::platform::artwork::local::{art_tracks_column, library_folder};

/// One library album on a shelf.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeAlbum {
    /// Native album id.
    pub local_id: String,
    /// Album title.
    pub title: String,
    /// Album artist name.
    pub artist_name: Option<String>,
    /// Release-group MBID, when identified.
    pub release_group_mbid: Option<String>,
    /// Album artist MBID, when identified.
    pub artist_mbid: Option<String>,
    /// Release year, when tagged.
    pub year: Option<i64>,
    /// Version of the album's own art (a folder image or an embedded
    /// picture), when it has some.
    pub art_version: Option<i64>,
}

/// One library artist on a shelf.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeArtist {
    /// Native artist id.
    pub local_id: String,
    /// Display name.
    pub name: String,
    /// MusicBrainz artist id, when identified.
    pub mbid: Option<String>,
    /// Albums credited to the artist.
    pub album_count: i64,
}

/// One play from the user's history.
#[derive(Debug, Clone, PartialEq)]
pub struct Play {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Release-group MBID, when known.
    pub release_group_mbid: Option<String>,
    /// When it played (RFC 3339).
    pub played_at: String,
}

/// One album whose art could stand for a genre.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtCandidate {
    /// Native album id.
    pub album_id: String,
    /// Album title.
    pub album_title: String,
    /// Album artist name, when known.
    pub album_artist_name: Option<String>,
    /// Album artist id, when known.
    pub album_artist_id: Option<String>,
    /// Indexed tracks of the album tagged with the genre.
    pub match_count: i64,
    /// Art version.
    pub cover_version: i64,
    /// Hash of the art bytes when the scan last read them.
    pub content_hash: Option<String>,
    /// `folder` or `embedded`.
    pub source: String,
    /// The folder image, or the file of the track holding the picture.
    pub path: String,
    /// The library folder the path was scanned under.
    pub root: Option<PathBuf>,
}

/// The collage candidates of one genre.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GenreCandidates {
    /// Folded genre name.
    pub folded: String,
    /// Goes up whenever art or tags of an album in the genre change.
    pub revision: i64,
    /// Albums with servable art, most matching tracks first.
    pub candidates: Vec<ArtCandidate>,
}

/// Library reads over the reader pool.
#[derive(Clone, Debug)]
pub struct HomeLibrary {
    pool: SqlitePool,
}

const ALBUM_SELECT: &str = "SELECT a.id, a.title, \
     COALESCE(an.display_name, a.album_artist_name) AS artist_name, \
     (SELECT ae.release_group_mbid FROM local_album_external_identities ae \
      WHERE ae.local_album_id = a.id AND ae.provider = 'musicbrainz' \
      AND ae.release_group_mbid IS NOT NULL ORDER BY ae.release_group_mbid LIMIT 1) AS rg, \
     (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
      WHERE aie.local_artist_id = a.album_artist_id AND aie.provider = 'musicbrainz' \
      ORDER BY aie.provider_artist_id LIMIT 1) AS artist_mbid, \
     a.year, \
     (SELECT w.version FROM local_album_artwork w WHERE w.local_album_id = a.id \
      AND w.source IN ('folder','embedded')) AS art_version \
     FROM local_albums a LEFT JOIN local_artists an ON an.id = a.album_artist_id \
     WHERE a.retired_into_album_id IS NULL";

type AlbumTuple = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

fn album(row: AlbumTuple) -> HomeAlbum {
    let (local_id, title, artist_name, release_group_mbid, artist_mbid, year, art_version) = row;
    HomeAlbum {
        local_id,
        title,
        artist_name: artist_name.filter(|name| !name.is_empty()),
        release_group_mbid,
        artist_mbid,
        year,
        art_version,
    }
}

/// Artists credited on the albums (track, album or album artist) of
/// indexed tracks tagged with one genre (v2's target genre index).
const GENRE_ARTISTS: &str = "WITH genre_tracks AS (SELECT track.id, track.local_album_id \
     FROM local_tracks track JOIN local_track_genres genre ON genre.local_track_id = track.id \
     WHERE track.availability = 'indexed' AND genre.folded_name = ?1), \
     genre_credits(local_artist_id, local_album_id) AS (\
     SELECT lta.local_artist_id, gt.local_album_id FROM genre_tracks gt \
     JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION \
     SELECT laa.local_artist_id, gt.local_album_id FROM genre_tracks gt \
     JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION \
     SELECT a.album_artist_id, gt.local_album_id FROM genre_tracks gt \
     JOIN local_albums a ON a.id = gt.local_album_id) \
     SELECT ar.id, ar.display_name, \
     (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
      WHERE aie.local_artist_id = ar.id AND aie.provider = 'musicbrainz' \
      ORDER BY aie.provider_artist_id LIMIT 1) AS mbid, \
     COUNT(DISTINCT gc.local_album_id) AS album_count \
     FROM genre_credits gc JOIN local_artists ar ON ar.id = gc.local_artist_id \
     WHERE ar.retired_into_artist_id IS NULL \
     GROUP BY ar.id ORDER BY ar.display_name COLLATE NOCASE, ar.id LIMIT ?2";

impl HomeLibrary {
    /// Read over `pool`.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The newest albums in the library.
    pub async fn recent_albums(&self, limit: i64) -> Result<Vec<HomeAlbum>, String> {
        let rows: Vec<AlbumTuple> = sqlx::query_as(&format!(
            "{ALBUM_SELECT} AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
             ORDER BY a.created_at DESC, a.id LIMIT ?1"
        ))
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("recent albums read: {error}"))?;
        Ok(rows.into_iter().map(album).collect())
    }

    /// The artists with the most albums in the library.
    pub async fn top_artists(&self, limit: i64) -> Result<Vec<HomeArtist>, String> {
        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT ar.id, ar.display_name, \
             (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
              WHERE aie.local_artist_id = ar.id AND aie.provider = 'musicbrainz' \
              ORDER BY aie.provider_artist_id LIMIT 1) AS mbid, \
             COUNT(DISTINCT credit.local_album_id) AS album_count \
             FROM local_album_artists credit \
             JOIN local_albums album ON album.id = credit.local_album_id \
               AND album.retired_into_album_id IS NULL \
             JOIN local_artists ar ON ar.id = credit.local_artist_id \
               AND ar.retired_into_artist_id IS NULL \
             WHERE EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = album.id \
               AND t.availability = 'indexed') \
             GROUP BY ar.id \
             ORDER BY album_count DESC, ar.display_name COLLATE NOCASE, ar.id LIMIT ?1",
        )
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("library artists read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|(local_id, name, mbid, album_count)| HomeArtist {
                local_id,
                name,
                mbid,
                album_count,
            })
            .collect())
    }

    /// The user's latest plays, newest first.
    pub async fn recent_plays(&self, user_id: &str, limit: i64) -> Result<Vec<Play>, String> {
        type PlayTuple = (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        );
        let rows: Vec<PlayTuple> = sqlx::query_as(
            "SELECT track_name, artist_name, album_name, recording_mbid, \
             release_group_mbid, played_at FROM library_play_history WHERE user_id = ?1 \
             ORDER BY played_at DESC, id LIMIT ?2",
        )
        .bind(user_id)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("play history read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    track_name,
                    artist_name,
                    album_name,
                    recording_mbid,
                    release_group_mbid,
                    played_at,
                )| Play {
                    track_name,
                    artist_name,
                    album_name,
                    recording_mbid,
                    release_group_mbid,
                    played_at,
                },
            )
            .collect())
    }

    /// Owned artists credited on a genre's tracks, by name.
    pub async fn genre_artists(&self, genre: &str, limit: i64) -> Result<Vec<HomeArtist>, String> {
        let folded = fold(genre);
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(GENRE_ARTISTS)
            .bind(folded)
            .bind(limit.max(1))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("genre artists read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|(local_id, name, mbid, album_count)| HomeArtist {
                local_id,
                name,
                mbid,
                album_count,
            })
            .collect())
    }

    /// Owned albums with tracks tagged with one genre, recently changed
    /// first.
    pub async fn genre_albums(&self, genre: &str, limit: i64) -> Result<Vec<HomeAlbum>, String> {
        let folded = fold(genre);
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<AlbumTuple> = sqlx::query_as(&format!(
            "{ALBUM_SELECT} AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND EXISTS (SELECT 1 FROM local_track_genres g \
             WHERE g.local_track_id = t.id AND g.folded_name = ?1)) \
             ORDER BY a.updated_at DESC, a.title COLLATE NOCASE, a.id LIMIT ?2"
        ))
        .bind(folded)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("genre albums read: {error}"))?;
        Ok(rows.into_iter().map(album).collect())
    }

    /// The artwork revision of each folded genre (0 when it has none).
    pub async fn revisions(&self, folded: &[String]) -> Result<HashMap<String, i64>, String> {
        if folded.is_empty() {
            return Ok(HashMap::new());
        }
        let sql = format!(
            "SELECT genre_folded, value FROM library_genre_artwork_revisions \
             WHERE genre_folded IN ({})",
            vec!["?"; folded.len()].join(",")
        );
        let mut query = sqlx::query_as::<_, (String, i64)>(&sql);
        for genre in folded {
            query = query.bind(genre);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("genre artwork revisions read: {error}"))?;
        Ok(rows.into_iter().collect())
    }

    /// Collage candidates for each genre (folded, deduplicated, at most
    /// 20), in the order asked: albums whose own art the artwork route
    /// can serve, most matching tracks first.
    pub async fn art_candidates(&self, genres: &[String]) -> Result<Vec<GenreCandidates>, String> {
        let mut folded: Vec<String> = Vec::new();
        for genre in genres {
            let name = fold(genre);
            if !name.is_empty() && !folded.contains(&name) && folded.len() < 20 {
                folded.push(name);
            }
        }
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let values = vec!["(?,?)"; folded.len()].join(",");
        let sql = format!(
            "WITH requested(genre_folded, ordinal) AS (VALUES {values}), \
             candidates AS (SELECT r.genre_folded, a.id AS album_id, a.title AS album_title, \
               a.album_artist_name, a.album_artist_id, COUNT(*) AS match_count, \
               w.version AS cover_version, w.content_hash, w.source, \
               CASE WHEN w.source = 'embedded' THEN emb.file_path ELSE w.source_locator END AS path, \
               {tracks} AS tracks \
             FROM requested r JOIN local_track_genres g ON g.folded_name = r.genre_folded \
             JOIN local_tracks t ON t.id = g.local_track_id AND t.availability = 'indexed' \
             JOIN local_albums a ON a.id = t.local_album_id AND a.retired_into_album_id IS NULL \
             JOIN local_album_artwork w ON w.local_album_id = a.id \
               AND w.source IN ('folder','embedded') \
             LEFT JOIN local_tracks emb ON emb.id = w.source_locator AND w.source = 'embedded' \
               AND emb.availability = 'indexed' \
             GROUP BY r.genre_folded, a.id) \
             SELECT r.genre_folded, COALESCE(rev.value, 0), c.album_id, c.album_title, \
               c.album_artist_name, c.album_artist_id, c.match_count, c.cover_version, \
               c.content_hash, c.source, c.path, c.tracks \
             FROM requested r \
             LEFT JOIN library_genre_artwork_revisions rev ON rev.genre_folded = r.genre_folded \
             LEFT JOIN candidates c ON c.genre_folded = r.genre_folded \
             ORDER BY r.ordinal, c.match_count DESC, c.album_id",
            tracks = art_tracks_column("emb")
        );
        type Row = (
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let mut query = sqlx::query_as::<_, Row>(&sql);
        for (ordinal, genre) in folded.iter().enumerate() {
            query = query.bind(genre).bind(ordinal as i64);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("genre artwork read: {error}"))?;
        let mut out: Vec<GenreCandidates> = Vec::new();
        for row in rows {
            let (
                genre,
                revision,
                album_id,
                title,
                artist_name,
                artist_id,
                matches,
                version,
                hash,
                source,
                path,
                tracks,
            ) = row;
            if out.last().is_none_or(|last| last.folded != genre) {
                out.push(GenreCandidates {
                    folded: genre,
                    revision,
                    candidates: Vec::new(),
                });
            }
            let (
                Some(album_id),
                Some(album_title),
                Some(cover_version),
                Some(source),
                Some(path),
                Some(last),
            ) = (album_id, title, version, source, path, out.last_mut())
            else {
                continue;
            };
            let root = library_folder(Path::new(&path), tracks.as_deref());
            last.candidates.push(ArtCandidate {
                album_id,
                album_title,
                album_artist_name: artist_name,
                album_artist_id: artist_id,
                match_count: matches.unwrap_or(0),
                cover_version,
                content_hash: hash,
                source,
                path,
                root,
            });
        }
        Ok(out)
    }
}
