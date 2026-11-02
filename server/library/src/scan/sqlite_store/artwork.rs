//! Album art found on disk, recorded after each scan.
//!
//! [`SqliteScanStore::refresh_album_artwork`] walks the indexed albums and
//! records the cover each one has in `local_album_artwork`: an image in the
//! album folder first (`cover.jpg`, `folder.png`, ...), else the first
//! picture embedded in one of its tracks. That is the same order v2 used
//! for local art, and the one Navidrome uses.
//!
//! Only the reference is stored (the folder image path, or the id of the
//! track holding the picture) plus a hash of the image bytes. The hash is
//! the art's identity: the row's `version` goes up only when it changes, and
//! the cover URLs and compat ids carry that version so clients refetch new
//! art and keep caching unchanged art.
//!
//! Each album also gets a check row holding a signature of what was looked
//! at: its tracks' stat revisions, the modification times of its folders,
//! and the folder image's own size and time. Albums whose signature is
//! unchanged are skipped, so a scan that changed nothing costs one stat per
//! folder. Adding or replacing a folder image changes the folder's
//! modification time, which brings the album back in.
//!
//! An album with a track file that cannot be reached (its folder is away,
//! a share is unmounted, or a read fails) is left as it is, with no new
//! check row: only a file that was read says an album has no art. The scan
//! is what judges a track gone, and then the album comes back without it.
//!
//! Manual art is never touched. Provider rows (Cover Art Archive art kept
//! from v2) are replaced only when local art turns up.
//!
//! Versions never go backwards: the check row keeps the highest version
//! ever issued, so art that disappears and later comes back gets a new
//! version rather than one a client may still hold for older art.
//!
//! A sweep is a background chore and gives way to scans: it stops between
//! albums when `stop` says so (shutdown, or a scan was requested) and after
//! [`SWEEP_SLICE`] of reading (at least 50 albums), and reports itself
//! incomplete. Finished albums are
//! already saved, so the next sweep carries on from there.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::params;
use sha2::{Digest as _, Sha256};

use super::{Lane, RunStop, ScanStoreError, SqliteScanStore};
use crate::clock::LibraryClock as _;
use crate::tags::TagsError;
use droppedneedle_kernel::beneath::{RootLinks, open_file_beneath};
use droppedneedle_providers::coverart::sniff_image_content_type;

/// Folder image names, most deliberate first, matched case-insensitively
/// (v2 `_LOCAL_COVER_STEMS`).
const COVER_STEMS: [&str; 5] = ["cover", "folder", "front", "album", "artwork"];
/// Folder image extensions (v2 `_LOCAL_COVER_EXTENSIONS`).
const COVER_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
/// Folder images and embedded pictures larger than this are ignored (v2
/// `_LOCAL_COVER_MAX_BYTES`).
const COVER_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// Longest stretch one sweep runs before handing back to the scan loop.
pub const SWEEP_SLICE: std::time::Duration = std::time::Duration::from_secs(30);
/// Albums read per sweep at least before the slice may end it.
pub(super) const MIN_PER_SLICE: usize = 50;
/// Albums written per transaction, so the store lock is held briefly.
pub(super) const WRITE_BATCH: usize = 200;

/// What one sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ArtworkSweep {
    /// Albums whose files changed and were looked at again.
    pub checked: usize,
    /// Albums whose recorded art was added or changed.
    pub changed: usize,
    /// Albums whose local art disappeared.
    pub cleared: usize,
    /// Albums left as they are because a file could not be reached or read.
    pub unreachable: usize,
    /// False when the sweep stopped early; run it again to finish.
    pub complete: bool,
}

/// One indexed track of an album, in album order.
pub(super) struct TrackFile {
    pub(super) id: String,
    pub(super) path: PathBuf,
    pub(super) stat_revision: String,
    /// The size the scan read.
    pub(super) size_bytes: i64,
}

/// The art row an album has now.
struct CurrentArt {
    source: String,
    locator: Option<String>,
    content_hash: Option<String>,
}

/// Local art found for an album.
struct FoundArt {
    source: &'static str,
    locator: String,
    content_hash: String,
    folder_image: Option<PathBuf>,
}

/// What looking for an album's art came to.
struct ArtSearch {
    found: Option<FoundArt>,
    /// A folder or file could not be read, so the answer may be incomplete.
    unread: bool,
}

/// One album's result, waiting to be written.
struct Checked {
    album_id: String,
    /// `None` when a file could not be read: the album is looked at again.
    signature: Option<String>,
    found: Option<FoundArt>,
}

/// Everything the art and lyrics sweeps read from the database up front,
/// in one read they share.
#[derive(Default)]
pub(super) struct Snapshot {
    pub(super) albums: Vec<(String, Vec<TrackFile>)>,
    current: HashMap<String, CurrentArt>,
    checks: HashMap<String, String>,
    pub(super) lyrics_checks: HashMap<String, String>,
}

impl SqliteScanStore {
    /// Record local album art for every album whose files changed since the
    /// last sweep. File reads happen outside the store lock; writes go in
    /// small transactions. `stop` is polled between albums so neither
    /// shutdown nor a requested scan waits for a large first sweep. A
    /// catalog that cannot be read is an error; a batch that cannot be
    /// written is logged and its albums are looked at again next sweep.
    pub fn refresh_album_artwork(
        &self,
        stop: &dyn Fn() -> bool,
    ) -> Result<ArtworkSweep, ScanStoreError> {
        Ok(self.sweep_artwork(&self.sweep_snapshot()?, stop))
    }

    /// The album art sweep over a snapshot already read.
    pub(super) fn sweep_artwork(
        &self,
        snapshot: &Snapshot,
        stop: &dyn Fn() -> bool,
    ) -> ArtworkSweep {
        // The slice clock starts at the first album that needs reading, so
        // the signature pass over unchanged albums never uses it up.
        let mut started: Option<std::time::Instant> = None;
        let mut sweep = ArtworkSweep {
            complete: true,
            ..ArtworkSweep::default()
        };
        let mut pending = Vec::new();
        for (album_id, tracks) in &snapshot.albums {
            let slice_spent = started.is_some_and(|at| at.elapsed() >= SWEEP_SLICE)
                && sweep.checked + sweep.unreachable >= MIN_PER_SLICE;
            if stop() || slice_spent {
                sweep.complete = false;
                break;
            }
            let current = snapshot.current.get(album_id);
            if current.is_some_and(|art| art.source == "manual") {
                continue;
            }
            let dirs = album_dirs(tracks);
            let current_folder = current
                .filter(|art| art.source == "folder")
                .and_then(|art| art.locator.as_deref())
                .map(Path::new);
            let signature = album_signature(tracks, &dirs, current_folder.as_slice());
            if snapshot.checks.get(album_id) == Some(&signature) {
                continue;
            }
            started.get_or_insert_with(std::time::Instant::now);
            if any_file_away(tracks) {
                sweep.unreachable += 1;
                continue;
            }
            sweep.checked += 1;
            let ArtSearch { found, unread } = find_art(tracks, &dirs);
            // A failed read only stands aside for art that adds to an album
            // with none: an unreadable cover must not cost the embedded one.
            // The album is still looked at again, as fixing a file's
            // permissions leaves every time the signature reads alone.
            if unread {
                sweep.unreachable += 1;
                if found.is_none() || current.is_some() {
                    continue;
                }
            }
            // Sign with the image actually found, so the next sweep (which
            // signs with the stored folder image) sees the same value.
            let signature =
                (!unread).then(|| album_signature(tracks, &dirs, found_folder(&found).as_slice()));
            pending.push(Checked {
                album_id: album_id.clone(),
                signature,
                found,
            });
            if pending.len() >= WRITE_BATCH {
                self.write_artwork(&mut pending, &snapshot.current, &mut sweep);
            }
        }
        self.write_artwork(&mut pending, &snapshot.current, &mut sweep);
        if sweep.checked > 0 {
            tracing::info!(
                checked = sweep.checked,
                changed = sweep.changed,
                cleared = sweep.cleared,
                complete = sweep.complete,
                "album art sweep finished"
            );
        }
        if sweep.unreachable > 0 {
            tracing::warn!(
                albums = sweep.unreachable,
                "album art sweep cleared nothing for albums with files it could not reach"
            );
        }
        sweep
    }

    pub(super) fn sweep_snapshot(&self) -> Result<Snapshot, ScanStoreError> {
        self.read_long(&RunStop::never(), "scan_sweep_snapshot", |conn| {
            Ok(sweep_snapshot(conn)?)
        })
    }

    /// Write one batch of results and clear it. A failed batch is logged
    /// and dropped; its albums carry no new check row, so the next sweep
    /// looks at them again.
    fn write_artwork(
        &self,
        pending: &mut Vec<Checked>,
        current: &HashMap<String, CurrentArt>,
        sweep: &mut ArtworkSweep,
    ) {
        if pending.is_empty() {
            return;
        }
        let now = self.clock.now_secs();
        let written = self.write_once(Lane::Background, "scan_album_art", |tx| {
            let mut changed = 0;
            let mut cleared = 0;
            for checked in pending.iter() {
                let existing = current.get(&checked.album_id);
                match &checked.found {
                    Some(found) => {
                        let same = existing.is_some_and(|art| {
                            art.source == found.source
                                && art.locator.as_deref() == Some(found.locator.as_str())
                                && art.content_hash.as_deref() == Some(found.content_hash.as_str())
                        });
                        if !same {
                            changed += tx.execute(
                                crate::scan::sql::WRITE_ARTWORK_ROW.text(),
                                params![
                                    checked.album_id,
                                    found.source,
                                    found.locator,
                                    found.content_hash,
                                    crate::clock::ms(now)
                                ],
                            )?;
                        }
                    }
                    None => {
                        cleared += tx.execute(
                            crate::scan::sql::DROP_FILE_ARTWORK.text(),
                            params![checked.album_id],
                        )?;
                    }
                }
                if let Some(signature) = &checked.signature {
                    tx.execute(
                        crate::scan::sql::WRITE_ARTWORK_CHECK.text(),
                        params![checked.album_id, signature, crate::clock::ms(now)],
                    )?;
                }
            }
            Ok((changed, cleared))
        });
        match written {
            Ok((changed, cleared)) => {
                sweep.changed += changed;
                sweep.cleared += cleared;
            }
            Err(error) => {
                tracing::warn!(%error, albums = pending.len(), "album art batch not saved; retried next sweep");
            }
        }
        pending.clear();
    }
}

/// Every live album with its indexed tracks, in album order.
pub(super) fn album_tracks(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<Vec<(String, Vec<TrackFile>)>> {
    let mut albums: Vec<(String, Vec<TrackFile>)> = Vec::new();
    let mut tracks = conn.prepare(crate::scan::sql::ALBUM_TRACKS.text())?;
    let mut rows = tracks.query([])?;
    while let Some(row) = rows.next()? {
        let album_id: String = row.get(0)?;
        let track = TrackFile {
            id: row.get(1)?,
            path: PathBuf::from(row.get::<_, String>(2)?),
            stat_revision: row.get(3)?,
            size_bytes: row.get(4)?,
        };
        match albums.last_mut() {
            Some((last, files)) if *last == album_id => files.push(track),
            _ => albums.push((album_id, vec![track])),
        }
    }
    Ok(albums)
}

/// True when any of an album's indexed track files cannot be reached. A
/// missing file counts: a share that is down reads as an empty folder or
/// no folder at all, and only the scan judges a track gone. So does an
/// empty file where the scan read a size: a share can leave stubs behind.
pub(super) fn any_file_away(tracks: &[TrackFile]) -> bool {
    tracks
        .iter()
        .any(|track| match std::fs::symlink_metadata(&track.path) {
            Ok(meta) => meta.len() == 0 && track.size_bytes > 0,
            Err(_) => true,
        })
}

/// Folders an album's tracks live in, in track order. Disc folders
/// (`Album/CD1/...`) keep their art one level up, so when every track
/// folder shares one parent, that parent is probed last (v2 rule).
pub(super) fn album_dirs(tracks: &[TrackFile]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for track in tracks {
        if let Some(parent) = track.path.parent()
            && !dirs.iter().any(|dir| dir == parent)
        {
            dirs.push(parent.to_path_buf());
        }
    }
    if dirs.len() > 1 {
        let mut parents = dirs.iter().filter_map(|dir| dir.parent());
        if let Some(first) = parents.next()
            && parents.all(|other| other == first)
            && first.file_name().is_some()
            && !dirs.iter().any(|dir| dir == first)
        {
            dirs.push(first.to_path_buf());
        }
    }
    dirs
}

/// Hash of what decides an album's art or lyrics: track ids and stat
/// revisions, folder modification times, and the size and time of each of
/// `files` (the folder image, or the `.lrc` files beside the tracks).
pub(super) fn album_signature(tracks: &[TrackFile], dirs: &[PathBuf], files: &[&Path]) -> String {
    let mut hasher = Sha256::new();
    for track in tracks {
        hasher.update(track.id.as_bytes());
        hasher.update([0]);
        hasher.update(track.stat_revision.as_bytes());
        hasher.update([0]);
    }
    for dir in dirs {
        hasher.update(dir.to_string_lossy().as_bytes());
        hasher.update(stat_stamp(dir).as_bytes());
    }
    for file in files {
        hasher.update(file.to_string_lossy().as_bytes());
        hasher.update(stat_stamp(file).as_bytes());
    }
    hex(&hasher.finalize())
}

/// `size:mtime_ns` of a path, or `missing`.
fn stat_stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |elapsed| elapsed.as_nanos());
            format!("{}:{mtime}", meta.len())
        }
        Err(_) => "missing".to_owned(),
    }
}

fn found_folder(found: &Option<FoundArt>) -> Option<&Path> {
    found.as_ref().and_then(|art| art.folder_image.as_deref())
}

/// Folder image first, then the first embedded picture.
fn find_art(tracks: &[TrackFile], dirs: &[PathBuf]) -> ArtSearch {
    let mut unread = false;
    if let Some((path, bytes)) = folder_image(dirs, &mut unread) {
        return ArtSearch {
            found: Some(FoundArt {
                source: "folder",
                locator: path.to_string_lossy().into_owned(),
                content_hash: content_hash(&bytes),
                folder_image: Some(path),
            }),
            unread,
        };
    }
    for track in tracks {
        match read_track_art(&track.path) {
            Ok(Some(bytes))
                if bytes.len() as u64 <= COVER_MAX_BYTES
                    && sniff_image_content_type(&bytes).is_some() =>
            {
                return ArtSearch {
                    found: Some(FoundArt {
                        source: "embedded",
                        locator: track.id.clone(),
                        content_hash: content_hash(&bytes),
                        folder_image: None,
                    }),
                    unread,
                };
            }
            Ok(_) => {}
            Err(error) => {
                unread |= matches!(error, TagsError::Io { .. });
                tracing::debug!(%error, path = %track.path.display(), "embedded art read failed");
            }
        }
    }
    ArtSearch {
        found: None,
        unread,
    }
}

/// The first picture in a track file, opened without following a link. A
/// FIFO or device fails rather than hanging the sweep.
fn read_track_art(path: &Path) -> Result<Option<Vec<u8>>, TagsError> {
    let file = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))
        .and_then(|folder| open_file_beneath(folder, path, RootLinks::Follow))
        .map_err(|source| TagsError::Io {
            path: path.display().to_string(),
            source,
        })?;
    crate::tags::read_cover_art_from(&file, path)
}

/// The first readable raster image named like a cover in the folders.
/// `unread` is set when a folder or image could not be read.
fn folder_image(dirs: &[PathBuf], unread: &mut bool) -> Option<(PathBuf, Vec<u8>)> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            *unread = true;
            continue;
        };
        let mut by_name: HashMap<String, PathBuf> = HashMap::new();
        for entry in entries {
            // Symlinks are never followed, as in the walk.
            match entry.and_then(|entry| Ok((entry.file_type()?, entry))) {
                Ok((kind, entry)) if kind.is_file() => {
                    by_name.insert(
                        entry.file_name().to_string_lossy().to_lowercase(),
                        entry.path(),
                    );
                }
                Ok(_) => {}
                Err(_) => *unread = true,
            }
        }
        for stem in COVER_STEMS {
            for extension in COVER_EXTENSIONS {
                let Some(path) = by_name.get(&format!("{stem}.{extension}")) else {
                    continue;
                };
                match read_listed_image(dir, path) {
                    Ok(Some(bytes)) if sniff_image_content_type(&bytes).is_some() => {
                        return Some((path.clone(), bytes));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(%error, path = %path.display(), "folder art read failed");
                        *unread = true;
                    }
                }
            }
        }
    }
    None
}

/// The bytes of image `path`, listed in `dir` as a regular file. The open
/// does not follow a link, so a file swapped for one after the listing is
/// not read. `None` when it is empty or too large, an error when it does
/// not open or read.
fn read_listed_image(dir: &Path, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file = open_file_beneath(dir, path, RootLinks::Follow)?;
    let size = file.metadata()?.len();
    if size == 0 || size > COVER_MAX_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(COVER_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= COVER_MAX_BYTES).then_some(bytes))
}

/// Hex SHA-256 of image bytes: the art's identity and cache key.
fn content_hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// What the sweeps read up front, in one read.
fn sweep_snapshot(conn: &rusqlite::Connection) -> rusqlite::Result<Snapshot> {
    let mut snapshot = Snapshot {
        albums: album_tracks(conn)?,
        ..Snapshot::default()
    };
    let mut current = conn.prepare(crate::scan::sql::ALL_ARTWORK.text())?;
    let mut rows = current.query([])?;
    while let Some(row) = rows.next()? {
        snapshot.current.insert(
            row.get(0)?,
            CurrentArt {
                source: row.get(1)?,
                locator: row.get(2)?,
                content_hash: row.get(3)?,
            },
        );
    }
    let mut checks = conn.prepare(crate::scan::sql::ALL_ARTWORK_CHECKS.text())?;
    let mut rows = checks.query([])?;
    while let Some(row) = rows.next()? {
        snapshot.checks.insert(row.get(0)?, row.get(1)?);
    }
    let mut checks = conn.prepare(crate::scan::sql::ALL_LYRICS_CHECKS.text())?;
    let mut rows = checks.query([])?;
    while let Some(row) = rows.next()? {
        snapshot.lyrics_checks.insert(row.get(0)?, row.get(1)?);
    }
    Ok(snapshot)
}
