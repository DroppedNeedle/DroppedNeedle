//! Local album art: the rows the library scan records, and the files
//! behind them.
//!
//! The scan stores a reference per album in `local_album_artwork`: the path
//! of a folder image, or the id of the track whose tags hold the picture,
//! plus the hash of the image bytes and a version that goes up when the
//! hash changes. This module looks those rows up, stats the files behind
//! them and reads the bytes back.
//! Only `folder` and `embedded` rows are served from here; other sources
//! (including the ones v2 wrote) have no local bytes to read.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension as _, params};

use droppedneedle_kernel::beneath::{RootLinks, open_file_beneath};
use droppedneedle_kernel::db::statement::Statement;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_library::scan::{exact_stat_revision, mtime_ns_from_metadata};
use droppedneedle_library::tags::read_cover_art_from;
use droppedneedle_providers::coverart::sniff_image_content_type;

use super::sql;

pub(crate) use super::sql::art_tracks_column;

/// Folder images and embedded pictures larger than this are not served
/// (the scan uses the same bound).
const MAX_LOCAL_BYTES: u64 = 25 * 1024 * 1024;

/// One album's recorded local art.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArt {
    /// Local album id.
    pub album_id: String,
    /// `folder` or `embedded`.
    pub source: String,
    /// Folder image path, or the file of the track holding the picture.
    pub path: PathBuf,
    /// The library folder the path was scanned under. Nothing is read
    /// without one.
    pub root: Option<PathBuf>,
    /// Hash of the bytes when the scan last read them.
    pub content_hash: Option<String>,
    /// Art version, bumped when the hash changes.
    pub version: i64,
}

/// What an album knows about its art and identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumArtContext {
    /// Release group the album is identified as, if any.
    pub release_group_mbid: Option<String>,
    /// Local art the scan recorded, if any.
    pub local: Option<LocalArt>,
}

/// Lookups over the catalog tables.
#[derive(Debug, Clone)]
pub struct LocalArtwork {
    db: Db,
}

/// The library folder `path` was scanned under: that of the first track in
/// `tracks` (from [`art_tracks_column`]) whose folder holds it. A track's
/// folder is its file less as many components as its relative path has,
/// as the scan works it out: the two can be spelled in different Unicode
/// forms, so their lengths say nothing.
pub(crate) fn library_folder(path: &Path, tracks: Option<&str>) -> Option<PathBuf> {
    let tracks: Vec<(Option<String>, Option<String>)> =
        match serde_json::from_str(tracks.unwrap_or("[]")) {
            Ok(tracks) => tracks,
            Err(error) => {
                tracing::warn!(%error, "local art tracks unreadable");
                return None;
            }
        };
    tracks.into_iter().find_map(|track| {
        let (Some(file), Some(relative)) = track else {
            return None;
        };
        let depth = Path::new(&relative).components().count();
        let folder = Path::new(&file).ancestors().nth(depth)?;
        (depth > 0 && path.starts_with(folder)).then(|| folder.to_path_buf())
    })
}

impl LocalArtwork {
    /// Lookups over the database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// The album's identity and local art.
    pub async fn album(&self, album_id: &str) -> Result<Option<AlbumArtContext>, DbError> {
        let id = album_id.to_owned();
        let group = self
            .db
            .read("album art identity", move |conn| {
                let group: Option<Option<String>> = conn
                    .query_row(sql::ALBUM_GROUP.text(), params![id], |row| row.get(0))
                    .optional()?;
                Ok::<_, DbError>(group)
            })
            .await?;
        let Some(release_group_mbid) = group else {
            return Ok(None);
        };
        let local = self.first("album art", sql::ART_OF_ALBUM, album_id).await?;
        Ok(Some(AlbumArtContext {
            release_group_mbid,
            local,
        }))
    }

    /// Local art of any album identified as this release group.
    pub async fn by_release_group(&self, mbid: &str) -> Result<Option<LocalArt>, DbError> {
        self.first("release group art", sql::ART_OF_GROUP, mbid)
            .await
    }

    /// Local art of any album identified as this exact release.
    pub async fn by_release(&self, mbid: &str) -> Result<Option<LocalArt>, DbError> {
        self.first("release art", sql::ART_OF_RELEASE, mbid).await
    }

    /// The art of a library artist's albums, which stands in for the
    /// artist's own picture: at most `limit` albums, one per distinct
    /// image, in [`sql::ART_OF_ARTIST`]'s order.
    pub async fn by_artist(&self, artist_id: &str, limit: usize) -> Result<Vec<LocalArt>, DbError> {
        let mut arts = self
            .servable("artist art", sql::ART_OF_ARTIST, artist_id)
            .await?;
        let mut seen = std::collections::HashSet::new();
        arts.retain(|art| {
            art.content_hash
                .as_ref()
                .is_none_or(|hash| seen.insert(hash.clone()))
        });
        arts.truncate(limit);
        Ok(arts)
    }

    /// First servable row of a lookup.
    async fn first(
        &self,
        name: &'static str,
        statement: Statement,
        value: &str,
    ) -> Result<Option<LocalArt>, DbError> {
        Ok(self
            .servable(name, statement, value)
            .await?
            .into_iter()
            .next())
    }

    /// The servable rows of a lookup, in its order: a local source with a
    /// path.
    async fn servable(
        &self,
        name: &'static str,
        statement: Statement,
        value: &str,
    ) -> Result<Vec<LocalArt>, DbError> {
        let value = value.to_owned();
        let rows = self
            .db
            .read(name, move |conn| {
                let rows = conn
                    .prepare(statement.text())?
                    .query_map(params![value], sql::ArtColumns::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, DbError>(rows)
            })
            .await?;
        let mut arts = Vec::new();
        for row in rows {
            let (true, Some(path)) = (row.source == "folder" || row.source == "embedded", row.path)
            else {
                continue;
            };
            let path = PathBuf::from(path);
            arts.push(LocalArt {
                album_id: row.album_id,
                source: row.source,
                root: library_folder(&path, row.tracks.as_deref()),
                path,
                content_hash: row.content_hash,
                version: row.version,
            });
        }
        Ok(arts)
    }
}

/// Open the file behind a local art row beneath its library folder, which
/// may itself be a link; nothing below it is followed. `None`, logged,
/// when there is no library folder or the open fails. Blocking.
fn open_art(root: Option<&Path>, path: &Path) -> Option<File> {
    match open_file_beneath(root?, path, RootLinks::Follow) {
        Ok(file) => Some(file),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "local art not read");
            None
        }
    }
}

/// The first picture in the tags of the open audio file `file`.
fn embedded_picture(file: &File, path: &Path) -> Option<Vec<u8>> {
    match read_cover_art_from(file, path) {
        Ok(picture) => picture,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "embedded art unreadable");
            None
        }
    }
}

/// The stat of the file behind a local art row (the folder image, or the
/// track holding the picture) in the scan's `size:mtime_ns` form, read
/// without following a link below the library folder. `None` when the
/// path is not a regular file.
pub async fn local_art_stamp(art: &LocalArt) -> Option<String> {
    let (root, path) = (art.root.clone(), art.path.clone());
    let stamp = tokio::task::spawn_blocking(move || {
        let meta = open_art(root.as_deref(), &path)?.metadata().ok()?;
        Some(exact_stat_revision(
            meta.len(),
            mtime_ns_from_metadata(&meta),
        ))
    })
    .await;
    match stamp {
        Ok(stamp) => stamp,
        Err(error) => {
            tracing::error!(%error, "local art stat task failed");
            None
        }
    }
}

/// Read the image behind a local art row, on a blocking thread. `None`
/// when the file is gone, too large, not a raster image, or reached through
/// a link below its library folder.
pub async fn read_local_art(art: &LocalArt) -> Option<(Vec<u8>, &'static str)> {
    let (root, path) = (art.root.clone(), art.path.clone());
    let embedded = art.source == "embedded";
    let read = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let file = open_art(root.as_deref(), &path)?;
        if embedded {
            return embedded_picture(&file, &path)
                .filter(|bytes| bytes.len() as u64 <= MAX_LOCAL_BYTES);
        }
        let size = file.metadata().ok()?.len();
        if size == 0 || size > MAX_LOCAL_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_LOCAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= MAX_LOCAL_BYTES).then_some(bytes)
    })
    .await;
    let bytes = match read {
        Ok(bytes) => bytes?,
        Err(error) => {
            tracing::error!(%error, "local art read task failed");
            return None;
        }
    };
    let content_type = sniff_image_content_type(&bytes)?;
    Some((bytes, content_type))
}
