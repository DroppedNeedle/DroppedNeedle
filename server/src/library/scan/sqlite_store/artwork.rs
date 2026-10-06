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
//! Manual art is never touched. Provider rows (Cover Art Archive art kept
//! from v2) are replaced only when local art turns up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::params;
use sha2::{Digest as _, Sha256};

use super::{SqliteScanStore, retry_on_busy};
use crate::providers::coverart::sniff_image_content_type;

/// Folder image names, most deliberate first, matched case-insensitively
/// (v2 `_LOCAL_COVER_STEMS`).
const COVER_STEMS: [&str; 5] = ["cover", "folder", "front", "album", "artwork"];
/// Folder image extensions (v2 `_LOCAL_COVER_EXTENSIONS`).
const COVER_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
/// Folder images larger than this are ignored (v2 `_LOCAL_COVER_MAX_BYTES`).
const COVER_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// Albums written per transaction, so the store lock is held briefly.
const WRITE_BATCH: usize = 200;

/// What one sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ArtworkSweep {
    /// Albums whose files changed and were looked at again.
    pub checked: usize,
    /// Albums whose recorded art was added or changed.
    pub changed: usize,
    /// Albums whose local art disappeared.
    pub cleared: usize,
}

/// One indexed track of an album, in album order.
struct TrackFile {
    id: String,
    path: PathBuf,
    stat_revision: String,
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

/// One album's result, waiting to be written.
struct Checked {
    album_id: String,
    signature: String,
    found: Option<FoundArt>,
}

/// Everything the sweep reads from the database up front.
#[derive(Default)]
struct Snapshot {
    albums: Vec<(String, Vec<TrackFile>)>,
    current: HashMap<String, CurrentArt>,
    checks: HashMap<String, String>,
}

impl SqliteScanStore {
    /// Record local album art for every album whose files changed since the
    /// last sweep. File reads happen outside the store lock; writes go in
    /// small transactions. `stop` is polled between albums so shutdown does
    /// not wait for a large first sweep.
    pub fn refresh_album_artwork(&self, stop: &dyn Fn() -> bool) -> ArtworkSweep {
        let snapshot = match self.artwork_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "album art sweep could not read the catalog; skipped");
                return ArtworkSweep::default();
            }
        };
        let mut sweep = ArtworkSweep::default();
        let mut pending = Vec::new();
        for (album_id, tracks) in &snapshot.albums {
            if stop() {
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
            let signature = album_signature(tracks, &dirs, current_folder);
            if snapshot.checks.get(album_id) == Some(&signature) {
                continue;
            }
            sweep.checked += 1;
            let found = find_art(tracks, &dirs);
            // Sign with the image actually found, so the next sweep (which
            // signs with the stored folder image) sees the same value.
            let signature = album_signature(tracks, &dirs, found_folder(&found));
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
                "album art sweep finished"
            );
        }
        sweep
    }

    fn artwork_snapshot(&self) -> rusqlite::Result<Snapshot> {
        let inner = self.lock();
        let mut snapshot = Snapshot::default();
        let mut tracks = inner.conn.prepare(
            "SELECT t.local_album_id, t.id, t.file_path, t.stat_revision \
             FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
             WHERE t.availability = 'indexed' AND a.retired_into_album_id IS NULL \
             ORDER BY t.local_album_id, t.disc_number, t.track_number, t.id",
        )?;
        let mut rows = tracks.query([])?;
        while let Some(row) = rows.next()? {
            let album_id: String = row.get(0)?;
            let track = TrackFile {
                id: row.get(1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                stat_revision: row.get(3)?,
            };
            match snapshot.albums.last_mut() {
                Some((last, files)) if *last == album_id => files.push(track),
                _ => snapshot.albums.push((album_id, vec![track])),
            }
        }
        let mut current = inner.conn.prepare(
            "SELECT local_album_id, source, source_locator, content_hash FROM local_album_artwork",
        )?;
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
        let mut checks = inner
            .conn
            .prepare("SELECT local_album_id, signature FROM local_album_artwork_checks")?;
        let mut rows = checks.query([])?;
        while let Some(row) = rows.next()? {
            snapshot.checks.insert(row.get(0)?, row.get(1)?);
        }
        Ok(snapshot)
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
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0);
        let mut inner = self.lock();
        let written = retry_on_busy("album art", || {
            let tx = inner.conn.transaction()?;
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
                                "INSERT INTO local_album_artwork \
                                 (local_album_id, source, source_locator, content_hash, updated_at) \
                                 SELECT ?1, ?2, ?3, ?4, ?5 \
                                 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
                                 ON CONFLICT(local_album_id) DO UPDATE SET \
                                 source = excluded.source, \
                                 source_locator = excluded.source_locator, \
                                 cover_url = NULL, \
                                 version = CASE WHEN content_hash IS excluded.content_hash \
                                     THEN version ELSE version + 1 END, \
                                 content_hash = excluded.content_hash, \
                                 updated_at = excluded.updated_at, \
                                 row_revision = row_revision + 1",
                                params![
                                    checked.album_id,
                                    found.source,
                                    found.locator,
                                    found.content_hash,
                                    now
                                ],
                            )?;
                        }
                    }
                    None => {
                        cleared += tx.execute(
                            "DELETE FROM local_album_artwork \
                             WHERE local_album_id = ?1 AND source IN ('folder', 'embedded')",
                            params![checked.album_id],
                        )?;
                    }
                }
                tx.execute(
                    "INSERT INTO local_album_artwork_checks (local_album_id, signature, checked_at) \
                     SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
                     ON CONFLICT(local_album_id) DO UPDATE SET \
                     signature = excluded.signature, checked_at = excluded.checked_at",
                    params![checked.album_id, checked.signature, now],
                )?;
            }
            tx.commit()?;
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

/// Folders an album's tracks live in, in track order. Disc folders
/// (`Album/CD1/...`) keep their art one level up, so when every track
/// folder shares one parent, that parent is probed last (v2 rule).
fn album_dirs(tracks: &[TrackFile]) -> Vec<PathBuf> {
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

/// Hash of what decides an album's art: track ids and stat revisions,
/// folder modification times, and the folder image's size and time.
fn album_signature(tracks: &[TrackFile], dirs: &[PathBuf], folder_image: Option<&Path>) -> String {
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
    if let Some(image) = folder_image {
        hasher.update(image.to_string_lossy().as_bytes());
        hasher.update(stat_stamp(image).as_bytes());
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
fn find_art(tracks: &[TrackFile], dirs: &[PathBuf]) -> Option<FoundArt> {
    if let Some((path, bytes)) = folder_image(dirs) {
        return Some(FoundArt {
            source: "folder",
            locator: path.to_string_lossy().into_owned(),
            content_hash: content_hash(&bytes),
            folder_image: Some(path),
        });
    }
    for track in tracks {
        match crate::library::tags::read_cover_art(&track.path) {
            Ok(Some(bytes)) if sniff_image_content_type(&bytes).is_some() => {
                return Some(FoundArt {
                    source: "embedded",
                    locator: track.id.clone(),
                    content_hash: content_hash(&bytes),
                    folder_image: None,
                });
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, path = %track.path.display(), "embedded art read failed");
            }
        }
    }
    None
}

/// The first readable raster image named like a cover in the folders.
fn folder_image(dirs: &[PathBuf]) -> Option<(PathBuf, Vec<u8>)> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut by_name: HashMap<String, PathBuf> = HashMap::new();
        for entry in entries.flatten() {
            // Symlinks are never followed, as in the walk.
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                by_name.insert(
                    entry.file_name().to_string_lossy().to_lowercase(),
                    entry.path(),
                );
            }
        }
        for stem in COVER_STEMS {
            for extension in COVER_EXTENSIONS {
                let Some(path) = by_name.get(&format!("{stem}.{extension}")) else {
                    continue;
                };
                let readable = std::fs::metadata(path)
                    .is_ok_and(|meta| meta.len() > 0 && meta.len() <= COVER_MAX_BYTES);
                if !readable {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(path)
                    && sniff_image_content_type(&bytes).is_some()
                {
                    return Some((path.clone(), bytes));
                }
            }
        }
    }
    None
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
