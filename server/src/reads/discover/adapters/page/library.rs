//! What the discover page reads from the local library: the identified
//! albums, the genre index over the scanned tags, milestone anniversaries
//! and new releases from followed artists.
//!
//! The genre queries are v2's target-catalog ones over the same tables:
//! a genre counts the distinct artists credited (track, album or album
//! artist) on indexed tracks tagged with it.

use std::collections::HashMap;

use sqlx::SqlitePool;

/// One identified library album.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryAlbum {
    /// Lowercased release-group MBID.
    pub release_group_mbid: String,
    /// Lowercased album-artist MBID, when identified.
    pub artist_mbid: Option<String>,
}

/// One library album shown on a shelf.
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfAlbum {
    /// Native album id.
    pub local_id: String,
    /// Release-group MBID, when identified.
    pub release_group_mbid: Option<String>,
    /// Album title.
    pub title: String,
    /// Album artist name.
    pub artist_name: Option<String>,
    /// Album artist MBID, when identified.
    pub artist_mbid: Option<String>,
    /// Release year, when tagged.
    pub year: Option<i64>,
}

/// One new release from a followed artist.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowedRelease {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Artist MBID.
    pub artist_mbid: String,
    /// First release date, when known.
    pub first_release_date: Option<String>,
}

/// Library reads over the reader pool.
#[derive(Clone, Debug)]
pub struct LibraryReads {
    pool: SqlitePool,
}

/// Lowercase and trim a genre the way the scan folds tag names.
pub fn fold(genre: &str) -> String {
    genre.trim().to_lowercase()
}

/// Indexed tracks per genre, credited to every artist that appears on
/// them. `{filter}` narrows the genres.
const GENRE_CREDITS: &str = "WITH genre_tracks AS (SELECT track.id, track.local_album_id, \
     genre.folded_name AS genre_folded FROM local_tracks track \
     JOIN local_track_genres genre ON genre.local_track_id = track.id \
     WHERE track.availability = 'indexed' {filter}), \
     genre_credits(genre_folded, local_artist_id) AS (\
     SELECT gt.genre_folded, lta.local_artist_id FROM genre_tracks gt \
     JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION \
     SELECT gt.genre_folded, laa.local_artist_id FROM genre_tracks gt \
     JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION \
     SELECT gt.genre_folded, a.album_artist_id FROM genre_tracks gt \
     JOIN local_albums a ON a.id = gt.local_album_id) ";

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

fn dedup_folded(genres: &[String]) -> Vec<String> {
    let mut seen = Vec::new();
    for genre in genres {
        let folded = fold(genre);
        if !folded.is_empty() && !seen.contains(&folded) {
            seen.push(folded);
        }
    }
    seen
}

impl LibraryReads {
    /// Read over `pool`.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The reader pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Up to `limit` identified albums, newest changes first (v2
    /// `get_home_albums`, which Missing Essentials counts per artist).
    pub async fn identified_albums(&self, limit: i64) -> Result<Vec<LibraryAlbum>, String> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT lower(ae.release_group_mbid), lower(aie.provider_artist_id) \
             FROM local_albums a \
             JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
               AND ae.provider = 'musicbrainz' AND ae.release_group_mbid IS NOT NULL \
             LEFT JOIN local_artist_external_identities aie \
               ON aie.local_artist_id = a.album_artist_id AND aie.provider = 'musicbrainz' \
             WHERE a.retired_into_album_id IS NULL \
             GROUP BY a.id ORDER BY a.updated_at DESC, a.id LIMIT ?1",
        )
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("library albums read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|(release_group_mbid, artist_mbid)| LibraryAlbum {
                release_group_mbid,
                artist_mbid,
            })
            .collect())
    }

    /// The genres with the most credited artists, as (folded genre, count).
    pub async fn top_genres(&self, limit: i64) -> Result<Vec<(String, i64)>, String> {
        let sql = format!(
            "{} SELECT genre_folded, COUNT(DISTINCT local_artist_id) AS cnt \
             FROM genre_credits GROUP BY genre_folded ORDER BY cnt DESC, genre_folded LIMIT ?",
            GENRE_CREDITS.replace("{filter}", "")
        );
        sqlx::query_as(&sql)
            .bind(limit.max(1))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("top genres read: {error}"))
    }

    /// Credited artists per genre, keyed by folded genre.
    pub async fn genre_artist_counts(
        &self,
        genres: &[String],
    ) -> Result<HashMap<String, i64>, String> {
        let wanted = dedup_folded(genres);
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let filter = format!("AND genre.folded_name IN ({})", placeholders(wanted.len()));
        let sql = format!(
            "{} SELECT genre_folded, COUNT(DISTINCT local_artist_id) FROM genre_credits \
             GROUP BY genre_folded",
            GENRE_CREDITS.replace("{filter}", &filter)
        );
        let mut query = sqlx::query_as::<_, (String, i64)>(&sql);
        for genre in &wanted {
            query = query.bind(genre);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("genre counts read: {error}"))?;
        Ok(rows.into_iter().collect())
    }

    /// Lowercased artist MBIDs per folded genre.
    pub async fn artists_for_genres(
        &self,
        genres: &[String],
    ) -> Result<HashMap<String, Vec<String>>, String> {
        let wanted = dedup_folded(genres);
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let filter = format!("AND genre.folded_name IN ({})", placeholders(wanted.len()));
        let sql = format!(
            "{} SELECT DISTINCT gc.genre_folded, lower(aie.provider_artist_id) AS artist_mbid \
             FROM genre_credits gc JOIN local_artist_external_identities aie \
             ON aie.local_artist_id = gc.local_artist_id AND aie.provider = 'musicbrainz' \
             ORDER BY gc.genre_folded, artist_mbid",
            GENRE_CREDITS.replace("{filter}", &filter)
        );
        let mut query = sqlx::query_as::<_, (String, String)>(&sql);
        for genre in &wanted {
            query = query.bind(genre);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("genre artists read: {error}"))?;
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for (genre, artist) in rows {
            grouped.entry(genre).or_default().push(artist);
        }
        Ok(grouped)
    }

    /// Genres stored per artist (the artist genre cache), keyed by
    /// lowercased artist MBID.
    pub async fn genres_for_artists(
        &self,
        artist_mbids: &[String],
    ) -> Result<HashMap<String, Vec<String>>, String> {
        let mut wanted: Vec<String> = artist_mbids
            .iter()
            .map(|mbid| mbid.trim().to_lowercase())
            .filter(|mbid| !mbid.is_empty())
            .collect();
        wanted.sort();
        wanted.dedup();
        let mut found = HashMap::new();
        for chunk in wanted.chunks(400) {
            let sql = format!(
                "SELECT artist_mbid_lower, genres_json FROM artist_genres \
                 WHERE artist_mbid_lower IN ({})",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query_as::<_, (String, String)>(&sql);
            for mbid in chunk {
                query = query.bind(mbid);
            }
            let rows = query
                .fetch_all(&self.pool)
                .await
                .map_err(|error| format!("artist genres read: {error}"))?;
            for (mbid, json) in rows {
                let Ok(genres) = serde_json::from_str::<Vec<serde_json::Value>>(&json) else {
                    continue;
                };
                let genres: Vec<String> = genres
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|genre| !genre.is_empty())
                    .map(str::to_owned)
                    .collect();
                found.insert(mbid, genres);
            }
        }
        Ok(found)
    }

    /// Library albums tagged with one genre, recently changed first.
    pub async fn albums_by_genre(
        &self,
        genre: &str,
        limit: i64,
    ) -> Result<Vec<ShelfAlbum>, String> {
        let folded = fold(genre);
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<ShelfTuple> = sqlx::query_as(&format!(
            "{SHELF_SELECT} WHERE a.retired_into_album_id IS NULL \
             AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
               AND t.availability = 'indexed' AND EXISTS (SELECT 1 FROM local_track_genres g \
               WHERE g.local_track_id = t.id AND g.folded_name = ?1)) \
             GROUP BY a.id ORDER BY a.updated_at DESC, a.title COLLATE NOCASE, a.id LIMIT ?2"
        ))
        .bind(folded)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("genre albums read: {error}"))?;
        Ok(rows.into_iter().map(shelf_album).collect())
    }

    /// Library albums released exactly one of `ages` years before
    /// `this_year`, oldest first.
    pub async fn anniversary_albums(
        &self,
        this_year: i64,
        ages: &[i64],
        limit: i64,
    ) -> Result<Vec<ShelfAlbum>, String> {
        if ages.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "{SHELF_SELECT} WHERE a.retired_into_album_id IS NULL AND a.year IN ({}) \
             AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
               AND t.availability = 'indexed') \
             GROUP BY a.id ORDER BY a.year ASC, a.title COLLATE NOCASE, a.id LIMIT ?",
            placeholders(ages.len())
        );
        let mut query = sqlx::query_as::<_, ShelfTuple>(&sql);
        for age in ages {
            query = query.bind(this_year - age);
        }
        let rows = query
            .bind(limit.max(1))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| format!("anniversary albums read: {error}"))?;
        Ok(rows.into_iter().map(shelf_album).collect())
    }

    /// The newest releases from artists the user follows that the library
    /// does not hold yet.
    pub async fn followed_releases(
        &self,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<FollowedRelease>, String> {
        let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT n.release_group_mbid, n.title, n.artist_name, f.artist_mbid, \
             n.first_release_date FROM new_release_feed n JOIN user_followed_artists f \
             ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ?1 \
             WHERE n.release_group_mbid_lower NOT IN \
               (SELECT lower(release_group_mbid) FROM local_album_external_identities \
                WHERE release_group_mbid IS NOT NULL) \
             ORDER BY n.first_release_date DESC, n.discovered_at DESC LIMIT ?2",
        )
        .bind(user_id)
        .bind(limit.max(1))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("followed releases read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(
                |(release_group_mbid, title, artist_name, artist_mbid, first_release_date)| {
                    FollowedRelease {
                        release_group_mbid,
                        title,
                        artist_name,
                        artist_mbid,
                        first_release_date,
                    }
                },
            )
            .collect())
    }
}

const SHELF_SELECT: &str = "SELECT a.id, MIN(ae.release_group_mbid), a.title, \
     a.album_artist_name, MIN(aie.provider_artist_id), a.year FROM local_albums a \
     LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
       AND ae.provider = 'musicbrainz' \
     LEFT JOIN local_artist_external_identities aie \
       ON aie.local_artist_id = a.album_artist_id AND aie.provider = 'musicbrainz'";

type ShelfTuple = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
);

fn shelf_album(row: ShelfTuple) -> ShelfAlbum {
    let (local_id, release_group_mbid, title, artist_name, artist_mbid, year) = row;
    ShelfAlbum {
        local_id,
        release_group_mbid,
        title,
        artist_name,
        artist_mbid,
        year,
    }
}
