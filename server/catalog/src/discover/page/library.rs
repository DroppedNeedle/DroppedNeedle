//! What the discover page reads from the local library: the owned
//! albums and the genre index over the scanned tags.
//!
//! A genre counts the distinct artists credited (track, album or album
//! artist) on indexed tracks tagged with it.

use std::collections::HashMap;

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::db::statement::ListLen;

use crate::discover::{read_all, sql};

/// One library album that owns a release group.
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

/// Library reads over the reader pool.
#[derive(Clone, Debug)]
pub struct LibraryReads {
    db: Db,
}

/// Lowercase and trim a genre the way the scan folds tag names.
pub fn fold(genre: &str) -> String {
    genre.trim().to_lowercase()
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
    /// Read over `db`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// The database.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Every album that owns a release group, by the shared ownership rule.
    /// Radio hides their groups.
    pub async fn owned_albums(&self) -> Result<Vec<LibraryAlbum>, String> {
        let rows = read_all(
            &self.db,
            "discover.library.owned_albums",
            sql::LIBRARY_ALBUMS.text(),
            (),
            sql::LibraryAlbumColumns::read,
        )
        .await
        .map_err(|error| format!("library albums read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|row| LibraryAlbum {
                release_group_mbid: row.release_group_mbid,
                artist_mbid: row.artist_mbid,
            })
            .collect())
    }

    /// The genres with the most credited artists, as (folded genre, count).
    pub async fn top_genres(&self, limit: i64) -> Result<Vec<(String, i64)>, String> {
        let rows = read_all(
            &self.db,
            "discover.library.top_genres",
            sql::TOP_GENRES.text(),
            (limit.max(1),),
            sql::GenreCount::read,
        )
        .await
        .map_err(|error| format!("top genres read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|row| (row.genre_folded, row.cnt))
            .collect())
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
        let rows = read_all(
            &self.db,
            "discover.library.genre_artists",
            sql::artists_for_genres(ListLen::new(wanted.len())),
            rusqlite::params_from_iter(wanted),
            sql::GenreArtist::read,
        )
        .await
        .map_err(|error| format!("genre artists read: {error}"))?;
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            grouped
                .entry(row.genre_folded)
                .or_default()
                .push(row.artist_mbid);
        }
        Ok(grouped)
    }

    /// Genres per artist, keyed by lowercased artist MBID: the tags on the
    /// library's indexed tracks credited to the artist, most used first,
    /// then the genres discover builds learned from providers
    /// (`catalog_artist_genres`). A genre appears once per artist.
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
        let mut found: HashMap<String, Vec<String>> = HashMap::new();
        for chunk in wanted.chunks(400) {
            let len = ListLen::new(chunk.len());
            let reads = [
                (sql::library_artist_genres(len), "library"),
                (sql::learned_artist_genres(len), "learned"),
            ];
            for (statement, what) in reads {
                let rows = read_all(
                    &self.db,
                    "discover.library.artist_genres",
                    statement,
                    rusqlite::params_from_iter(chunk.to_vec()),
                    sql::ArtistGenre::read,
                )
                .await
                .map_err(|error| format!("{what} artist genres read: {error}"))?;
                for row in rows {
                    let name = row.name.trim();
                    let genres = found.entry(row.mbid).or_default();
                    if !name.is_empty() && !genres.iter().any(|known| fold(known) == row.folded) {
                        genres.push(name.to_owned());
                    }
                }
            }
        }
        Ok(found)
    }

    /// Display names of library artists, keyed by lowercased MBID.
    pub async fn artist_names(
        &self,
        artist_mbids: &[String],
    ) -> Result<HashMap<String, String>, String> {
        let mut names = HashMap::new();
        for chunk in artist_mbids.chunks(400) {
            let wanted: Vec<String> = chunk.iter().map(|mbid| mbid.to_lowercase()).collect();
            let rows = read_all(
                &self.db,
                "discover.library.artist_names",
                sql::library_artist_names(ListLen::new(wanted.len())),
                rusqlite::params_from_iter(wanted),
                sql::LibraryArtistName::read,
            )
            .await
            .map_err(|error| format!("artist names read: {error}"))?;
            names.extend(rows.into_iter().map(|row| (row.mbid, row.name)));
        }
        Ok(names)
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
        let rows = read_all(
            &self.db,
            "discover.library.genre_albums",
            sql::ALBUMS_BY_GENRE.text(),
            (folded, limit.max(1)),
            sql::ShelfAlbumColumns::read,
        )
        .await
        .map_err(|error| format!("genre albums read: {error}"))?;
        Ok(rows.into_iter().map(shelf_album).collect())
    }
}

fn shelf_album(row: sql::ShelfAlbumColumns) -> ShelfAlbum {
    ShelfAlbum {
        local_id: row.id,
        release_group_mbid: row.release_group_mbid,
        title: row.title,
        artist_name: row.album_artist_name,
        artist_mbid: row.artist_mbid,
        year: row.year,
    }
}
