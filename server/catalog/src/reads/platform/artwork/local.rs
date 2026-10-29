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

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io::{self, BufReader, Read as _, Seek as _};
use std::os::fd::{AsRawFd, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use lofty::config::{ParseOptions, ParsingMode};
use lofty::error::FileParseError;
use lofty::file::{FileType, TaggedFile, TaggedFileExt as _};
use lofty::probe::Probe;
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

/// Open the regular file at `path` for reading, beneath library folder
/// `root`.
///
/// The root is the admin's setting and may be reached through a link, so it
/// is opened as named. Everything below it is opened one component at a
/// time with `O_NOFOLLOW`, the file included: a link met on the way, even
/// one swapped in after the scan or a moment ago, fails the open rather
/// than being followed. `.` and `..` below the root are refused. Blocking.
fn open_below(root: &Path, path: &Path) -> io::Result<File> {
    let refused = || io::Error::new(io::ErrorKind::InvalidInput, "not below the library folder");
    let below = path.strip_prefix(root).map_err(|_| refused())?;
    let mut parts = below
        .components()
        .map(|part| match part {
            Component::Normal(name) => c_name(name),
            _ => Err(refused()),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let name = parts.pop().ok_or_else(refused)?;
    let mut dir = open_at(
        None,
        &c_name(root.as_os_str())?,
        libc::O_PATH | libc::O_DIRECTORY,
    )?;
    for part in parts {
        dir = open_at(
            Some(&dir),
            &part,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )?;
    }
    // Non-blocking so a FIFO cannot hang the open; cleared once the file is
    // known to be regular.
    let file = File::from(open_at(
        Some(&dir),
        &name,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_NOFOLLOW,
    )?);
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    // SAFETY: `file` owns an open descriptor for the length of both calls.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above; only the status flags change.
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name holds a NUL byte"))
}

/// `openat` one name in `dir`, or a path from the working folder when
/// there is no `dir`.
fn open_at(dir: Option<&OwnedFd>, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    let dir = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `dir` is an open descriptor or `AT_FDCWD`, and `name` a
    // NUL-terminated string, both alive for the call.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by a successful `openat` and nothing
    // else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// [`open_below`] for a local art row. `None`, logged, when there is no
/// library folder or the open fails.
fn open_art(root: Option<&Path>, path: &Path) -> Option<File> {
    match open_below(root?, path) {
        Ok(file) => Some(file),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "local art not read");
            None
        }
    }
}

/// The first picture in the tags of the open audio file `file`, read as
/// the library reads tags: as the container `path`'s extension names, in
/// lofty's default mode and then its relaxed one.
fn embedded_picture(file: &File, path: &Path) -> Option<Vec<u8>> {
    if let Err(error) = droppedneedle_library::tags::format_for_path(path) {
        tracing::debug!(%error, path = %path.display(), "embedded art unreadable");
        return None;
    }
    let file_type = FileType::from_path(path)?;
    let read = |options: ParseOptions| -> Result<TaggedFile, FileParseError> {
        let mut file = file.try_clone()?;
        file.rewind()?;
        Probe::with_file_type(BufReader::new(file), file_type)
            .options(options)
            .read()
    };
    let tagged = match read(ParseOptions::new()) {
        Err(error) if !is_io(&error) => {
            read(ParseOptions::new().parsing_mode(ParsingMode::Relaxed))
        }
        read => read,
    };
    match tagged {
        Ok(tagged) => tagged
            .tags()
            .iter()
            .find_map(|tag| tag.pictures().first())
            .map(|picture| picture.data().to_vec()),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "embedded art unreadable");
            None
        }
    }
}

fn is_io(error: &FileParseError) -> bool {
    std::error::Error::source(error).is_some_and(|source| source.is::<io::Error>())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The opener walks below the library folder without following any
    /// link, so a folder swapped for a link after the scan, or after any
    /// earlier check, is refused at the read itself. The library folder
    /// may itself be a link.
    #[test]
    fn the_opener_never_follows_a_link_below_the_library_folder() {
        let scratch =
            droppedneedle_kernel::db::scratch::ScratchDir::new("local-art-open").expect("scratch");
        let base = scratch.path().to_path_buf();
        let (music, outside) = (base.join("music"), base.join("outside"));
        std::fs::create_dir_all(music.join("Artist/Album")).expect("dir");
        std::fs::create_dir_all(outside.join("Album")).expect("dir");
        std::fs::write(music.join("Artist/Album/cover.png"), b"inside").expect("file");
        std::fs::write(outside.join("Album/cover.png"), b"private").expect("file");
        std::os::unix::fs::symlink(&music, base.join("alias")).expect("link");
        let read = |root: &Path, path: &Path| {
            let mut bytes = Vec::new();
            open_below(root, path)?.read_to_end(&mut bytes)?;
            io::Result::Ok(bytes)
        };

        let cover = music.join("Artist/Album/cover.png");
        assert_eq!(read(&music, &cover).expect("plain"), b"inside");
        let aliased = base.join("alias/Artist/Album/cover.png");
        assert_eq!(
            read(&base.join("alias"), &aliased).expect("linked library folder"),
            b"inside"
        );

        std::os::unix::fs::symlink(
            outside.join("Album/cover.png"),
            music.join("Artist/link.png"),
        )
        .expect("link");
        assert!(
            read(&music, &music.join("Artist/link.png")).is_err(),
            "linked file"
        );

        std::fs::rename(music.join("Artist"), base.join("held")).expect("move");
        std::os::unix::fs::symlink(&outside, music.join("Artist")).expect("link");
        assert!(read(&music, &cover).is_err(), "a parent swapped for a link");
        assert!(read(&music, &music.join("Artist/../Artist/Album/cover.png")).is_err());
        assert!(read(&music, &outside.join("Album/cover.png")).is_err());
    }
}
