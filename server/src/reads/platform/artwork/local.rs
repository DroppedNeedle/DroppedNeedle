//! Local album art: the rows the library scan records, and the files
//! behind them.
//!
//! The scan stores a reference per album in `local_album_artwork`: the path
//! of a folder image, or the id of the track whose tags hold the picture,
//! plus the hash of the image bytes and a version that goes up when the
//! hash changes. This module looks those rows up and reads the bytes back.
//! Only `folder` and `embedded` rows are served from here; other sources
//! (kept from v2) have no local bytes to read.

use std::path::PathBuf;

use sqlx::{Row as _, SqlitePool};

use crate::providers::coverart::sniff_image_content_type;

/// Folder images larger than this are not served (the scan uses the same
/// bound).
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

const ART_COLUMNS: &str = "w.local_album_id AS album_id, w.source AS source, \
    CASE WHEN w.source = 'embedded' THEN t.file_path ELSE w.source_locator END AS path, \
    w.content_hash AS content_hash, w.version AS version";

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
                &format!("SELECT {ART_COLUMNS} {ART_JOINS} WHERE w.local_album_id = ?"),
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
                "SELECT {ART_COLUMNS} {ART_JOINS} \
                 JOIN local_album_external_identities ae ON ae.local_album_id = w.local_album_id \
                 WHERE ae.release_group_mbid = ? ORDER BY w.local_album_id"
            ),
            mbid,
        )
        .await
    }

    /// Local art of any album identified as this exact release.
    pub async fn by_release(&self, mbid: &str) -> Result<Option<LocalArt>, sqlx::Error> {
        self.first(
            &format!(
                "SELECT {ART_COLUMNS} {ART_JOINS} \
                 JOIN local_album_external_identities ae ON ae.local_album_id = w.local_album_id \
                 WHERE ae.release_mbid = ? ORDER BY w.local_album_id"
            ),
            mbid,
        )
        .await
    }

    /// First servable row of a lookup: a local source with a path.
    async fn first(&self, sql: &str, value: &str) -> Result<Option<LocalArt>, sqlx::Error> {
        let rows = sqlx::query(sql).bind(value).fetch_all(&self.pool).await?;
        for row in rows {
            let source: String = row.try_get("source")?;
            let path: Option<String> = row.try_get("path")?;
            let (true, Some(path)) = (source == "folder" || source == "embedded", path) else {
                continue;
            };
            return Ok(Some(LocalArt {
                album_id: row.try_get("album_id")?,
                source,
                path: PathBuf::from(path),
                content_hash: row.try_get("content_hash")?,
                version: row.try_get("version")?,
            }));
        }
        Ok(None)
    }
}

/// Read the image behind a local art row, on a blocking thread. `None`
/// when the file is gone, too large, or not a raster image.
pub async fn read_local_art(art: &LocalArt) -> Option<(Vec<u8>, &'static str)> {
    let path = art.path.clone();
    let embedded = art.source == "embedded";
    let read = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        if embedded {
            match crate::library::tags::read_cover_art(&path) {
                Ok(bytes) => bytes,
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
