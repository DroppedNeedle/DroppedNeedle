//! Which local files playback and downloads may serve, and where they live
//! on disk.
//!
//! Local stream keys and download ids are catalog track ids. Only files the
//! library catalog knows are ever served: a track row that is currently
//! streamable (`availability = 'indexed'`). The row's
//! root id and relative path are joined onto that root's configured
//! directory, and the result must still sit inside a configured library
//! root after symlinks resolve. Nothing a caller sends ever becomes part of
//! a path; ids only select catalog rows.

use std::path::{Component, Path, PathBuf};

use sqlx::{Row as _, SqlitePool};

use crate::library::scan::roots::RootRegistry;
use crate::library::wiring::RootSource;

/// One catalog file playback or a download may serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFile {
    /// Local track id.
    pub track_id: String,
    /// Library root the file was scanned under.
    pub root_id: String,
    /// Path relative to that root.
    pub relative_path: String,
    /// Absolute path recorded at scan time; used only when the root id is
    /// no longer configured.
    pub file_path: String,
    /// Track title, for the archive entry name.
    pub title: String,
    /// Disc number.
    pub disc_number: i64,
    /// Track number within the disc.
    pub track_number: i64,
}

/// An album and its streamable files, disc and track order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumFiles {
    /// Album title.
    pub title: String,
    /// Album artist display name.
    pub artist_name: String,
    /// Streamable files.
    pub files: Vec<CatalogFile>,
}

/// Why a catalog file cannot be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRefusal {
    /// The file is gone from disk, or its root is not configured.
    Missing,
    /// The path resolves outside every library root.
    Outside,
}

const FILE_COLUMNS: &str =
    "id, root_id, relative_path, file_path, title, disc_number, track_number";

fn file_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<CatalogFile, sqlx::Error> {
    Ok(CatalogFile {
        track_id: row.try_get("id")?,
        root_id: row.try_get("root_id")?,
        relative_path: row.try_get("relative_path")?,
        file_path: row.try_get("file_path")?,
        title: row.try_get("title")?,
        disc_number: row.try_get("disc_number")?,
        track_number: row.try_get("track_number")?,
    })
}

/// Catalog reads over the reader pool.
#[derive(Debug, Clone)]
pub struct FileCatalog {
    pool: SqlitePool,
}

impl FileCatalog {
    /// Read from this pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// One streamable track's file. None for unknown, missing or excluded
    /// tracks.
    pub async fn track(&self, track_id: &str) -> Result<Option<CatalogFile>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {FILE_COLUMNS} FROM local_tracks WHERE id = ? AND availability = 'indexed'"
        ))
        .bind(track_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(file_from_row).transpose()
    }

    /// One album (a merged album answers for the album it merged into)
    /// with its streamable files. None when the album is unknown.
    pub async fn album(&self, album_id: &str) -> Result<Option<AlbumFiles>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT a.id AS id, a.title AS title, \
             COALESCE(an.display_name, a.album_artist_name, '') AS artist_name \
             FROM local_albums a LEFT JOIN local_artists an ON an.id = a.album_artist_id \
             WHERE a.id = (SELECT COALESCE(retired_into_album_id, id) FROM local_albums \
             WHERE id = ?)",
        )
        .bind(album_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: String = row.try_get("id")?;
        let rows = sqlx::query(&format!(
            "SELECT {FILE_COLUMNS} FROM local_tracks \
             WHERE local_album_id = ? AND availability = 'indexed' \
             ORDER BY disc_number, track_number, id"
        ))
        .bind(&id)
        .fetch_all(&self.pool)
        .await?;
        Ok(Some(AlbumFiles {
            title: row.try_get("title")?,
            artist_name: row.try_get("artist_name")?,
            files: rows.iter().map(file_from_row).collect::<Result<_, _>>()?,
        }))
    }

    /// The oldest live album with a streamable track holding a MusicBrainz
    /// release group, else release. Ids compare case-insensitively.
    pub async fn album_by_mbid(&self, mbid: &str) -> Result<Option<String>, sqlx::Error> {
        let needle = mbid.trim().to_lowercase();
        for column in ["release_group_mbid", "release_mbid"] {
            let found: Option<String> = sqlx::query_scalar(&format!(
                "SELECT b.id FROM local_album_external_identities e \
                 JOIN local_albums b ON b.id = e.local_album_id \
                 WHERE lower(e.{column}) = ? AND b.retired_into_album_id IS NULL \
                 AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
                 AND t.availability = 'indexed') \
                 ORDER BY b.created_at, b.id LIMIT 1"
            ))
            .bind(&needle)
            .fetch_optional(&self.pool)
            .await?;
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }
}

/// `relative` joined onto `root`, refusing anything but plain components.
fn join_relative(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let mut parts = 0;
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => {
                path.push(part);
                parts += 1;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    (parts > 0).then_some(path)
}

/// Canonical path of a catalog file, proven to sit inside a configured
/// library root. Blocking: touches the filesystem, so callers run it off
/// the async workers.
pub fn resolve(registry: &RootRegistry, file: &CatalogFile) -> Result<PathBuf, PathRefusal> {
    let roots: Vec<PathBuf> = registry
        .roots()
        .iter()
        .filter_map(|root| root.path.canonicalize().ok())
        .collect();
    let candidate = match registry.resolve(&file.root_id) {
        Some(root) => join_relative(&root.path, &file.relative_path).ok_or(PathRefusal::Outside)?,
        // A root removed from settings keeps its rows until the next scan;
        // the recorded absolute path still serves if another root covers it.
        None => PathBuf::from(&file.file_path),
    };
    let resolved = candidate.canonicalize().map_err(|_| PathRefusal::Missing)?;
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        return Err(PathRefusal::Outside);
    }
    if !resolved.is_file() {
        return Err(PathRefusal::Missing);
    }
    Ok(resolved)
}

/// Why a track id cannot be served from disk.
#[derive(Debug)]
pub enum LocateError {
    /// No streamable catalog track has this id.
    Unknown,
    /// The catalog knows the track, but its file cannot be served.
    Refused(PathRefusal),
    /// The catalog read or the blocking task failed. Log-only text.
    Internal(String),
}

/// The library catalog plus the live root registry: everything needed to
/// turn a track id into a file on disk. Playback and downloads both go
/// through here, so the two can never disagree about which files are
/// reachable.
#[derive(Clone)]
pub struct LibraryFiles {
    catalog: FileCatalog,
    roots: RootSource,
}

impl LibraryFiles {
    /// Catalog reads over `pool`, paths resolved against `roots`, which is
    /// read again on every lookup so root changes apply without a restart.
    pub fn new(pool: SqlitePool, roots: RootSource) -> Self {
        Self {
            catalog: FileCatalog::new(pool),
            roots,
        }
    }

    /// The catalog reads.
    pub fn catalog(&self) -> &FileCatalog {
        &self.catalog
    }

    /// The root registry as configured right now.
    pub fn registry(&self) -> RootRegistry {
        (self.roots)()
    }

    /// The file of one streamable catalog track, confined to the library
    /// roots.
    pub async fn locate_track(
        &self,
        track_id: &str,
    ) -> Result<(CatalogFile, PathBuf), LocateError> {
        let file = self
            .catalog
            .track(track_id)
            .await
            .map_err(|error| LocateError::Internal(error.to_string()))?
            .ok_or(LocateError::Unknown)?;
        let roots = std::sync::Arc::clone(&self.roots);
        tokio::task::spawn_blocking(move || {
            let registry = roots();
            resolve(&registry, &file).map(|path| (file, path))
        })
        .await
        .map_err(|error| LocateError::Internal(error.to_string()))?
        .map_err(LocateError::Refused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::scan::models::EffectivePolicy;
    use crate::library::scan::roots::LibraryRoot;

    fn file(root_id: &str, relative: &str, absolute: &str) -> CatalogFile {
        CatalogFile {
            track_id: "t1".to_owned(),
            root_id: root_id.to_owned(),
            relative_path: relative.to_owned(),
            file_path: absolute.to_owned(),
            title: "Song".to_owned(),
            disc_number: 1,
            track_number: 1,
        }
    }

    #[test]
    fn only_files_inside_a_root_resolve() {
        let music = crate::tooling::scratch::ScratchDir::new("download-root").expect("music dir");
        let elsewhere =
            crate::tooling::scratch::ScratchDir::new("download-other").expect("other dir");
        std::fs::create_dir_all(music.path().join("Artist")).expect("album dir");
        std::fs::write(music.path().join("Artist/song.flac"), b"x").expect("song");
        std::fs::write(elsewhere.path().join("secret.flac"), b"x").expect("secret");
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "r1",
                music.path().to_path_buf(),
                EffectivePolicy::Automatic,
            )],
            true,
            "rev",
        );
        let outside = elsewhere.path().join("secret.flac");
        let outside = outside.to_string_lossy();

        assert!(resolve(&registry, &file("r1", "Artist/song.flac", "")).is_ok());
        assert_eq!(
            resolve(&registry, &file("r1", "../secret.flac", "")),
            Err(PathRefusal::Outside)
        );
        assert_eq!(
            resolve(&registry, &file("r1", "Artist/gone.flac", "")),
            Err(PathRefusal::Missing)
        );
        assert_eq!(
            resolve(&registry, &file("gone-root", "x", &outside)),
            Err(PathRefusal::Outside)
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                elsewhere.path().join("secret.flac"),
                music.path().join("Artist/link.flac"),
            )
            .expect("symlink");
            assert_eq!(
                resolve(&registry, &file("r1", "Artist/link.flac", "")),
                Err(PathRefusal::Outside),
                "a symlink out of the root is refused"
            );
        }
    }
}
