//! What home and the genre page read from the local library: the newest
//! albums, the artists with the most albums, the user's recent plays, the
//! owned artists and albums of one genre, and the albums whose art can
//! make up a genre's collage.
//!
//! Only albums with at least one indexed track count, and retired albums
//! and artists never show.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::types::Value;

use crate::artwork::local::library_folder;
use crate::discover::page::library::fold;
use crate::discover::{read_all, sql};

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
    db: Db,
}

fn album(row: sql::HomeAlbumColumns) -> HomeAlbum {
    HomeAlbum {
        local_id: row.id,
        title: row.title,
        artist_name: row.artist_name.filter(|name| !name.is_empty()),
        release_group_mbid: row.rg,
        artist_mbid: row.artist_mbid,
        year: row.year,
        art_version: row.art_version,
    }
}

fn artist(row: sql::HomeArtistColumns) -> HomeArtist {
    HomeArtist {
        local_id: row.id,
        name: row.display_name,
        mbid: row.mbid,
        album_count: row.album_count,
    }
}

impl HomeLibrary {
    /// Read over `db`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// The newest albums in the library.
    pub async fn recent_albums(&self, limit: i64) -> Result<Vec<HomeAlbum>, String> {
        let rows = read_all(
            &self.db,
            "discover.home.recent_albums",
            sql::RECENT_ALBUMS.text(),
            (limit.max(1),),
            sql::HomeAlbumColumns::read,
        )
        .await
        .map_err(|error| format!("recent albums read: {error}"))?;
        Ok(rows.into_iter().map(album).collect())
    }

    /// The artists with the most albums in the library.
    pub async fn top_artists(&self, limit: i64) -> Result<Vec<HomeArtist>, String> {
        let rows = read_all(
            &self.db,
            "discover.home.top_artists",
            sql::TOP_ARTISTS.text(),
            (limit.max(1),),
            sql::HomeArtistColumns::read,
        )
        .await
        .map_err(|error| format!("library artists read: {error}"))?;
        Ok(rows.into_iter().map(artist).collect())
    }

    /// The user's latest plays, newest first.
    pub async fn recent_plays(&self, user_id: &str, limit: i64) -> Result<Vec<Play>, String> {
        let rows = read_all(
            &self.db,
            "discover.home.recent_plays",
            sql::RECENT_PLAYS.text(),
            (user_id.to_owned(), limit.max(1)),
            sql::PlayColumns::read,
        )
        .await
        .map_err(|error| format!("play history read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|row| Play {
                track_name: row.track_name,
                artist_name: row.artist_name,
                album_name: row.album_name,
                recording_mbid: row.recording_mbid,
                release_group_mbid: row.release_group_mbid,
                played_at: row.played_at,
            })
            .collect())
    }

    /// Owned artists credited on a genre's tracks, by name.
    pub async fn genre_artists(&self, genre: &str, limit: i64) -> Result<Vec<HomeArtist>, String> {
        let folded = fold(genre);
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let rows = read_all(
            &self.db,
            "discover.home.genre_artists",
            sql::GENRE_ARTISTS.text(),
            (folded, limit.max(1)),
            sql::HomeArtistColumns::read,
        )
        .await
        .map_err(|error| format!("genre artists read: {error}"))?;
        Ok(rows.into_iter().map(artist).collect())
    }

    /// Owned albums with tracks tagged with one genre, recently changed
    /// first.
    pub async fn genre_albums(&self, genre: &str, limit: i64) -> Result<Vec<HomeAlbum>, String> {
        let folded = fold(genre);
        if folded.is_empty() {
            return Ok(Vec::new());
        }
        let rows = read_all(
            &self.db,
            "discover.home.genre_albums",
            sql::GENRE_ALBUMS.text(),
            (folded, limit.max(1)),
            sql::HomeAlbumColumns::read,
        )
        .await
        .map_err(|error| format!("genre albums read: {error}"))?;
        Ok(rows.into_iter().map(album).collect())
    }

    /// The artwork revision of each folded genre (0 when it has none).
    pub async fn revisions(&self, folded: &[String]) -> Result<HashMap<String, i64>, String> {
        if folded.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = read_all(
            &self.db,
            "discover.home.genre_revisions",
            sql::genre_revisions(ListLen::new(folded.len())),
            rusqlite::params_from_iter(folded.to_vec()),
            sql::GenreRevision::read,
        )
        .await
        .map_err(|error| format!("genre artwork revisions read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|row| (row.genre_folded, row.value))
            .collect())
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
        let statement = sql::art_candidates(ListLen::new(folded.len()));
        let binds: Vec<Value> = folded
            .into_iter()
            .enumerate()
            .flat_map(|(ordinal, genre)| [Value::Text(genre), Value::Integer(ordinal as i64)])
            .collect();
        let rows = read_all(
            &self.db,
            "discover.home.art_candidates",
            statement,
            rusqlite::params_from_iter(binds),
            sql::ArtCandidateColumns::read,
        )
        .await
        .map_err(|error| format!("genre artwork read: {error}"))?;
        let mut out: Vec<GenreCandidates> = Vec::new();
        for row in rows {
            if out
                .last()
                .is_none_or(|last| last.folded != row.genre_folded)
            {
                out.push(GenreCandidates {
                    folded: row.genre_folded,
                    revision: row.revision,
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
            ) = (
                row.album_id,
                row.album_title,
                row.cover_version,
                row.source,
                row.path,
                out.last_mut(),
            )
            else {
                continue;
            };
            let root = library_folder(Path::new(&path), row.tracks.as_deref());
            last.candidates.push(ArtCandidate {
                album_id,
                album_title,
                album_artist_name: row.album_artist_name,
                album_artist_id: row.album_artist_id,
                match_count: row.match_count.unwrap_or(0),
                cover_version,
                content_hash: row.content_hash,
                source,
                path,
                root,
            });
        }
        Ok(out)
    }
}
