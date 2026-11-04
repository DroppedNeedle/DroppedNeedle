//! Which local files playback and downloads may serve, and where they live
//! on disk.
//!
//! Local stream keys and download ids are catalog track ids. Only files the
//! library catalog knows are ever served: a track row that is currently
//! streamable (`availability = 'indexed'`). The row's
//! root id and relative path are joined onto that root's configured
//! directory, and every read opens the file beneath that directory without
//! following a link below it (the directory itself is the admin's and may
//! be one). Nothing a caller sends ever becomes part of a path; ids only
//! select catalog rows.

use std::path::{Component, Path, PathBuf};

use rusqlite::{OptionalExtension as _, Row};

use droppedneedle_kernel::beneath::{RootLinks, met_a_link, open_file_beneath};
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_library::scan::coordinator::{ResolverSource as _, SharedResolver};
use droppedneedle_library::scan::roots::RootRegistry;

use super::sql;

/// One catalog file playback or a download may serve.
#[derive(Debug, Clone, PartialEq)]
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
    /// Audio format the scan read, e.g. `flac`.
    pub file_format: String,
    /// Source bitrate in kbps, when the scan read one.
    pub bitrate_kbps: Option<i64>,
    /// Length in seconds, when the scan read one.
    pub duration_seconds: Option<f64>,
}

/// An album and its streamable files, disc and track order.
#[derive(Debug, Clone, PartialEq)]
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
    /// The path leaves every library root, or meets a link below one.
    Outside,
}

/// A file on disk and the folder it is read beneath. Every read opens it
/// with [`LocatedFile::open`], which follows no link below the folder, so
/// a folder swapped for a link after the lookup is refused at the read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedFile {
    /// The library folder as configured, which may itself be a link.
    pub root: PathBuf,
    /// The file, below `root`.
    pub path: PathBuf,
}

impl LocatedFile {
    /// Open the regular file for reading. Blocking.
    pub fn open(&self) -> std::io::Result<std::fs::File> {
        open_file_beneath(&self.root, &self.path, RootLinks::Follow)
    }
}

impl CatalogFile {
    /// A row read by `sql::TRACK_FILE` or `sql::ALBUM_FILES`.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            track_id: row.get(0)?,
            root_id: row.get(1)?,
            relative_path: row.get(2)?,
            file_path: row.get(3)?,
            title: row.get(4)?,
            disc_number: row.get(5)?,
            track_number: row.get(6)?,
            file_format: row.get(7)?,
            bitrate_kbps: row.get(8)?,
            duration_seconds: row.get(9)?,
        })
    }
}

/// Catalog reads over the database's read pool.
#[derive(Clone)]
pub struct FileCatalog {
    db: Db,
}

impl FileCatalog {
    /// Read from this database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// One streamable track's file; the old id of a track that continued
    /// into another answers with that track's file. None for unknown,
    /// missing or excluded tracks.
    pub async fn track(&self, track_id: &str) -> Result<Option<CatalogFile>, DbError> {
        let track_id = track_id.to_owned();
        self.db
            .read("stream-track-file", move |conn| {
                Ok(conn
                    .query_row(sql::TRACK_FILE.text(), [track_id], CatalogFile::from_row)
                    .optional()?)
            })
            .await
    }

    /// One album (a merged album answers for the album it merged into)
    /// with its streamable files. None when the album is unknown.
    pub async fn album(&self, album_id: &str) -> Result<Option<AlbumFiles>, DbError> {
        let album_id = album_id.to_owned();
        self.db
            .read("stream-album-files", move |conn| {
                let album = conn
                    .query_row(sql::ALBUM.text(), [album_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .optional()?;
                let Some((id, title, artist_name)) = album else {
                    return Ok(None);
                };
                let mut query = conn.prepare(sql::ALBUM_FILES.text())?;
                let files = query
                    .query_map([id], CatalogFile::from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(Some(AlbumFiles {
                    title,
                    artist_name,
                    files,
                }))
            })
            .await
    }

    /// The album a MusicBrainz release group opens when it has a streamable
    /// track (the one the pages show), else the oldest live album with one
    /// identified as the release. Ids compare case-insensitively.
    pub async fn album_by_mbid(&self, mbid: &str) -> Result<Option<String>, DbError> {
        let needle = mbid.trim().to_lowercase();
        self.db
            .read("stream-album-by-mbid", move |conn| {
                for statement in [sql::ALBUM_BY_RELEASE_GROUP, sql::ALBUM_BY_RELEASE] {
                    let found = conn
                        .query_row(statement.text(), [&needle], |row| row.get::<_, String>(0))
                        .optional()?;
                    if found.is_some() {
                        return Ok(found);
                    }
                }
                Ok(None)
            })
            .await
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

/// A catalog file and the library folder it is read beneath, proven to
/// open there as a regular file. Blocking: touches the filesystem, so
/// callers run it off the async workers.
pub fn resolve(registry: &RootRegistry, file: &CatalogFile) -> Result<LocatedFile, PathRefusal> {
    let located = match registry.resolve(&file.root_id) {
        Some(root) => LocatedFile {
            path: join_relative(&root.path, &file.relative_path).ok_or(PathRefusal::Outside)?,
            root: root.path.clone(),
        },
        // A root removed from settings keeps its rows until the next scan;
        // the recorded path still serves while it lies below a configured
        // library folder, and is read beneath that folder, so no link below
        // it is followed.
        None => {
            let path = PathBuf::from(&file.file_path);
            let root = registry
                .roots()
                .iter()
                .map(|root| root.path.clone())
                .find(|root| path.starts_with(root) && path != *root)
                .ok_or(PathRefusal::Outside)?;
            LocatedFile { root, path }
        }
    };
    match located.open() {
        Ok(_) => Ok(located),
        Err(error) if met_a_link(&error) => Err(PathRefusal::Outside),
        Err(_) => Err(PathRefusal::Missing),
    }
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
    roots: SharedResolver,
}

impl LibraryFiles {
    /// Catalog reads over `db`, paths resolved against `roots`, which is
    /// read again on every lookup so root changes apply without a restart.
    pub fn new(db: Db, roots: SharedResolver) -> Self {
        Self {
            catalog: FileCatalog::new(db),
            roots,
        }
    }

    /// The catalog reads.
    pub fn catalog(&self) -> &FileCatalog {
        &self.catalog
    }

    /// The root registry as configured right now.
    pub fn registry(&self) -> RootRegistry {
        self.roots.resolver().registry().clone()
    }

    /// The file of one streamable catalog track, confined to the library
    /// roots, with the format, bitrate and length the scan read from it.
    pub async fn locate_track(
        &self,
        track_id: &str,
    ) -> Result<(CatalogFile, LocatedFile), LocateError> {
        let file = self
            .catalog
            .track(track_id)
            .await
            .map_err(|error| LocateError::Internal(error.to_string()))?
            .ok_or(LocateError::Unknown)?;
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || {
            let registry = roots.resolver().registry().clone();
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
    use droppedneedle_library::scan::models::EffectivePolicy;
    use droppedneedle_library::scan::roots::LibraryRoot;

    fn file(root_id: &str, relative: &str, absolute: &str) -> CatalogFile {
        CatalogFile {
            track_id: "t1".to_owned(),
            root_id: root_id.to_owned(),
            relative_path: relative.to_owned(),
            file_path: absolute.to_owned(),
            title: "Song".to_owned(),
            disc_number: 1,
            track_number: 1,
            file_format: "flac".to_owned(),
            bitrate_kbps: None,
            duration_seconds: None,
        }
    }

    #[test]
    fn only_files_inside_a_root_resolve() {
        let music =
            droppedneedle_kernel::db::scratch::ScratchDir::new("download-root").expect("music dir");
        let elsewhere = droppedneedle_kernel::db::scratch::ScratchDir::new("download-other")
            .expect("other dir");
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

    /// Nothing below a library folder is followed, even a link that stays
    /// inside it, and a folder swapped for a link after the lookup is
    /// refused when the file is read. The folder itself may be a link.
    #[test]
    fn a_linked_folder_below_a_root_is_never_served() {
        let scratch =
            droppedneedle_kernel::db::scratch::ScratchDir::new("download-linked").expect("dir");
        let base = scratch.path();
        let music = base.join("music");
        std::fs::create_dir_all(music.join("Artist/Album")).expect("album dir");
        std::fs::write(music.join("Artist/Album/song.flac"), b"x").expect("song");
        std::os::unix::fs::symlink(&music, base.join("alias")).expect("root link");
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "r1",
                base.join("alias"),
                EffectivePolicy::Automatic,
            )],
            true,
            "rev",
        );

        let located = resolve(&registry, &file("r1", "Artist/Album/song.flac", ""))
            .expect("a real folder, under a linked root, serves");
        std::os::unix::fs::symlink(music.join("Artist"), music.join("Linked")).expect("link");
        assert_eq!(
            resolve(&registry, &file("r1", "Linked/Album/song.flac", "")),
            Err(PathRefusal::Outside),
            "a link that stays inside the root is not followed"
        );
        let removed_root = base.join("alias/Linked");
        let removed_file = removed_root.join("Album/song.flac");
        assert_eq!(
            resolve(
                &registry,
                &file(
                    "gone-root",
                    "Album/song.flac",
                    &removed_file.to_string_lossy()
                )
            ),
            Err(PathRefusal::Outside),
            "a removed root's row is read beneath the configured folder, not through a link"
        );
        let kept_file = base.join("alias/Artist/Album/song.flac");
        assert!(
            resolve(
                &registry,
                &file("gone-root", "Album/song.flac", &kept_file.to_string_lossy())
            )
            .is_ok(),
            "a removed root's row under a real folder still serves"
        );

        std::fs::rename(music.join("Artist"), base.join("held")).expect("folder moves");
        std::os::unix::fs::symlink(base.join("held"), music.join("Artist")).expect("swap");
        assert!(
            located.open().is_err(),
            "a folder swapped for a link after the lookup is refused at the read"
        );
    }
}
