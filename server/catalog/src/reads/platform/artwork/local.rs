//! Local album art: the rows the library scan records, and the files
//! behind them.
//!
//! The scan stores a reference per album in `local_album_artwork`: the path
//! of a folder image, or the id of the track whose tags hold the picture,
//! plus the hash of the image bytes and a version that goes up when the
//! hash changes. This module looks those rows up, stats the files behind
//! them and reads the bytes back.
//! Only `folder` and `embedded` rows are served from here; other sources
//! (kept from v2) have no local bytes to read.

use std::path::{Path, PathBuf};

use sqlx::{Row as _, SqlitePool};

use droppedneedle_library::scan::{exact_stat_revision, mtime_ns_from_metadata};
use droppedneedle_providers::coverart::sniff_image_content_type;

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
    pool: SqlitePool,
}

/// The tracks that can place art row `w` in its library folder, as a JSON
/// array of `[file_path, relative_path]` pairs: `embedded`, the track
/// holding the picture, for embedded art, else every indexed track of the
/// album. Read with [`library_folder`].
pub(crate) fn art_tracks_column(embedded: &str) -> String {
    format!(
        "CASE WHEN w.source = 'embedded' \
         THEN json_array(json_array({embedded}.file_path, {embedded}.relative_path)) \
         ELSE (SELECT json_group_array(json_array(art_track.file_path, art_track.relative_path)) \
         FROM local_tracks art_track WHERE art_track.local_album_id = w.local_album_id \
         AND art_track.availability = 'indexed') END"
    )
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

fn art_columns() -> String {
    format!(
        "w.local_album_id AS album_id, w.source AS source, \
         CASE WHEN w.source = 'embedded' THEN t.file_path ELSE w.source_locator END AS path, \
         {} AS tracks, w.content_hash AS content_hash, w.version AS version",
        art_tracks_column("t")
    )
}

const ART_JOINS: &str = "FROM local_album_artwork w \
    JOIN local_albums a ON a.id = w.local_album_id AND a.retired_into_album_id IS NULL \
    LEFT JOIN local_tracks t ON t.id = w.source_locator AND w.source = 'embedded' \
        AND t.availability = 'indexed'";

impl LocalArtwork {
    /// Lookups over the reader pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The album's identity and local art.
    pub async fn album(&self, album_id: &str) -> Result<Option<AlbumArtContext>, sqlx::Error> {
        let exists = sqlx::query(
            "SELECT ae.release_group_mbid AS rg FROM local_albums a \
             LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
             WHERE a.id = ? AND a.retired_into_album_id IS NULL",
        )
        .bind(album_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = exists else {
            return Ok(None);
        };
        let local = self
            .first(
                &format!(
                    "SELECT {} {ART_JOINS} WHERE w.local_album_id = ?",
                    art_columns()
                ),
                album_id,
            )
            .await?;
        Ok(Some(AlbumArtContext {
            release_group_mbid: row.try_get("rg")?,
            local,
        }))
    }

    /// Local art of any album identified as this release group.
    pub async fn by_release_group(&self, mbid: &str) -> Result<Option<LocalArt>, sqlx::Error> {
        self.first(
            &format!(
                "SELECT {} {ART_JOINS} \
                 JOIN local_album_external_identities ae ON ae.local_album_id = w.local_album_id \
                 WHERE ae.release_group_mbid = ? ORDER BY w.local_album_id",
                art_columns()
            ),
            mbid,
        )
        .await
    }

    /// Local art of any album identified as this exact release.
    pub async fn by_release(&self, mbid: &str) -> Result<Option<LocalArt>, sqlx::Error> {
        self.first(
            &format!(
                "SELECT {} {ART_JOINS} \
                 JOIN local_album_external_identities ae ON ae.local_album_id = w.local_album_id \
                 WHERE ae.release_mbid = ? ORDER BY w.local_album_id",
                art_columns()
            ),
            mbid,
        )
        .await
    }

    /// The art of a library artist's albums, which stands in for the
    /// artist's own picture: at most `limit` albums, one per distinct
    /// image. Albums under the artist's name come before ones that only
    /// credit them, studio albums before compilations, then the newest
    /// release, then the latest added.
    pub async fn by_artist(
        &self,
        artist_id: &str,
        limit: usize,
    ) -> Result<Vec<LocalArt>, sqlx::Error> {
        let mut arts = self
            .servable(
                &format!(
                    "SELECT {} {ART_JOINS} \
                     WHERE w.local_album_id IN (SELECT id FROM local_albums \
                     WHERE album_artist_id = ?1 \
                     UNION SELECT local_album_id FROM local_album_artists \
                     WHERE local_artist_id = ?1) \
                     AND a.has_indexed_track = 1 AND w.source IN ('folder', 'embedded') \
                     ORDER BY a.album_artist_id = ?1 DESC, a.is_compilation, \
                     a.year IS NULL, a.year DESC, a.created_at DESC, a.id LIMIT 40",
                    art_columns()
                ),
                artist_id,
            )
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
    async fn first(&self, sql: &str, value: &str) -> Result<Option<LocalArt>, sqlx::Error> {
        Ok(self.servable(sql, value).await?.into_iter().next())
    }

    /// The servable rows of a lookup, in its order: a local source with a
    /// path.
    async fn servable(&self, sql: &str, value: &str) -> Result<Vec<LocalArt>, sqlx::Error> {
        let rows = sqlx::query(sql).bind(value).fetch_all(&self.pool).await?;
        let mut arts = Vec::new();
        for row in rows {
            let source: String = row.try_get("source")?;
            let path: Option<String> = row.try_get("path")?;
            let tracks: Option<String> = row.try_get("tracks")?;
            let (true, Some(path)) = (source == "folder" || source == "embedded", path) else {
                continue;
            };
            let path = PathBuf::from(path);
            arts.push(LocalArt {
                album_id: row.try_get("album_id")?,
                source,
                root: library_folder(&path, tracks.as_deref()),
                path,
                content_hash: row.try_get("content_hash")?,
                version: row.try_get("version")?,
            });
        }
        Ok(arts)
    }
}

/// True when `path` lies below library folder `root` with no link on the
/// way, the file itself included. Blocking.
fn link_free(root: Option<&Path>, path: &Path) -> bool {
    let Some(root) = root else {
        return false;
    };
    match droppedneedle_library::publish::paths::ensure_no_symlink_below(root, path) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "local art not read");
            false
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
        if !link_free(root.as_deref(), &path) {
            return None;
        }
        let meta = std::fs::symlink_metadata(&path).ok()?;
        meta.is_file()
            .then(|| exact_stat_revision(meta.len(), mtime_ns_from_metadata(&meta)))
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
        if !link_free(root.as_deref(), &path) {
            return None;
        }
        if embedded {
            match droppedneedle_library::tags::read_cover_art(&path) {
                Ok(bytes) => bytes.filter(|bytes| bytes.len() as u64 <= MAX_LOCAL_BYTES),
                Err(error) => {
                    tracing::debug!(%error, path = %path.display(), "embedded art unreadable");
                    None
                }
            }
        } else {
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_LOCAL_BYTES {
                return None;
            }
            std::fs::read(&path).ok()
        }
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
