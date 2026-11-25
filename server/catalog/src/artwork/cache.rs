//! Content-addressed cover cache on disk, bounded by size.
//!
//! Layout under `<cache_dir>/covers`:
//!
//! - `blobs/<aa>/<hash>`: image bytes, named by the SHA-256 of the bytes,
//!   so the same picture fetched for a release and its release group, or
//!   found in a folder and served through two routes, is stored once.
//! - `keys/<aa>/<key hash>`: a small text record mapping a lookup key (for
//!   example `caa:release-group:<mbid>:500`) to a blob, or recording that
//!   the archive had no art, or could not be asked, until a given time. A record may also carry the
//!   image's credit (see [`ImageCredit`]). TheAudioDB album
//!   thumbnails keep theirs under `keys-audiodb/` instead, so they can be
//!   forgotten on their own.
//!
//! The bound is either fixed (`COVER_CACHE_MAX_SIZE_MB`) or worked out from
//! the library: 512 MiB plus 0.5 MiB per album, at least 1 GiB, and never
//! more than a quarter of the room on the cache's volume (free space plus
//! what the cache holds), with 64 MiB as the least. An automatic bound is
//! worked out when the index loads and at most hourly after that, while the
//! cache is over it.
//!
//! When the blobs pass the bound, the least recently used ones are deleted
//! until they fill 90% of it, so the next writes have room. A key whose blob
//! was evicted reads as a miss, so the art is fetched again. Eviction also
//! deletes the key records that point at the evicted blobs. A full pass over
//! the key records, dropping orphans and miss markers past their time, runs
//! when the cache loads and at most hourly after that. When that pass read
//! every key record cleanly, it also deletes the images no record names.
//! The index also keeps which key records hold an image and which a fresh
//! "no art" marker, so a library view can tell, without touching the disk,
//! that a cover route would answer 404. Every file operation runs on a
//! blocking thread, and writes go through a temporary file and a rename so
//! a crash never leaves a half-written image behind.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use droppedneedle_kernel::clock::Clock;

use crate::local::store::LocalCatalog;

/// Least time between two full passes over the key records.
const FULL_CLEAN_EVERY: Duration = Duration::from_secs(3600);
/// Images unlinked per hold of the index lock, so a large removal never
/// keeps other callers waiting long.
const UNLINK_BATCH: usize = 64;
/// Least time between two workings-out of an automatic bound, in ms.
const BOUND_EVERY_MS: i64 = 3_600_000;
/// Key records, by key hash.
const KEYS_DIR: &str = "keys";
/// Key records of TheAudioDB album thumbnails, kept apart so the AudioDB
/// clear can drop them without touching other art.
const AUDIODB_KEYS_DIR: &str = "keys-audiodb";
/// Keys that live in [`AUDIODB_KEYS_DIR`].
pub const AUDIODB_KEY_PREFIX: &str = "audiodb:";

const MIB: u64 = 1024 * 1024;
/// What an automatic bound starts from before any album is counted.
const AUTO_BASE: u64 = 512 * MIB;
/// What an automatic bound adds per album the library lists.
const AUTO_PER_ALBUM: u64 = MIB / 2;
/// The least an automatic bound asks for while the volume has room.
const AUTO_FLOOR: u64 = 1024 * MIB;
/// The least an automatic bound ever is, however full the volume.
const AUTO_MINIMUM: u64 = 64 * MIB;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// A SHA-256, as bytes.
type Digest = [u8; 32];

/// The images a full pass started from, with their last use, sorted by hash.
type Snapshot = Vec<(Digest, u64)>;

/// Which marker [`ArtworkCache::put_marker`] writes.
#[derive(Debug, Clone, Copy)]
enum Marker {
    Miss,
    Unreachable,
}

/// What a key file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEntry {
    /// Art stored under this content hash.
    Hit {
        /// SHA-256 of the image bytes.
        hash: String,
        /// MIME type of the bytes.
        content_type: String,
    },
    /// The source had no art; do not ask again before `until` (unix secs).
    Miss {
        /// Unix second the marker expires.
        until: u64,
    },
    /// The source could not be asked (down, refusing, or resting); nothing
    /// is known about the art. Ask again after `until` (unix secs).
    Unreachable {
        /// Unix second the marker expires.
        until: u64,
    },
}

/// Where a credited picture came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CreditSource {
    /// A Wikimedia Commons file.
    WikimediaCommons,
}

/// The credit a picture's source asks for, kept in the picture's key record
/// so the two are stored and dropped together. Text is plain, never HTML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageCredit {
    /// Where the picture came from.
    pub source: CreditSource,
    /// Who made it.
    pub author: Option<String>,
    /// The licence's short name.
    pub licence: Option<String>,
    /// The licence's page, http or https.
    pub licence_url: Option<String>,
    /// The picture's page at its source, http or https.
    pub file_url: String,
}

/// Prefix of the key record line that holds an [`ImageCredit`] as JSON.
const CREDIT_LINE: &str = "credit ";

/// The cache's size bound as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheLimit {
    /// The bound in bytes.
    pub bytes: u64,
    /// Worked out from the library, rather than set by
    /// `COVER_CACHE_MAX_SIZE_MB`.
    pub automatic: bool,
}

/// The folder a key record lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KeyDir {
    Art,
    AudioDb,
}

impl KeyDir {
    const ALL: [Self; 2] = [Self::Art, Self::AudioDb];

    fn name(self) -> &'static str {
        match self {
            Self::Art => KEYS_DIR,
            Self::AudioDb => AUDIODB_KEYS_DIR,
        }
    }
}

/// A key record: its folder and the hash of its key, which names its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct KeyId {
    dir: KeyDir,
    name: Digest,
}

/// One blob's bookkeeping for eviction.
#[derive(Debug, Clone, Copy)]
struct BlobUse {
    size: u64,
    last_used: u64,
}

/// In-memory view of the blob directory, built on first use.
#[derive(Debug, Default)]
struct Index {
    blobs: HashMap<Digest, BlobUse>,
    total: u64,
    clock: u64,
    /// When the last full pass over the key records ran.
    last_full_clean: Option<Instant>,
    /// "No art" markers by key record, with the second they expire.
    misses: HashMap<KeyId, u64>,
    /// Key records that point at an image, with the image they name.
    held: HashMap<KeyId, Digest>,
    /// Images a write is storing or naming right now, by how many writes.
    writing: HashMap<Digest, u32>,
}

impl Index {
    fn touch(&mut self, hash: &Digest) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self.blobs.get_mut(hash) {
            entry.last_used = clock;
        }
    }

    /// A write of `hash` starts: the image counts as used, and no full pass
    /// removes it until the write ends.
    fn begin_write(&mut self, hash: &Digest) {
        self.touch(hash);
        *self.writing.entry(*hash).or_default() += 1;
    }

    fn end_write(&mut self, hash: &Digest) {
        if let Some(count) = self.writing.get_mut(hash) {
            *count -= 1;
            if *count == 0 {
                self.writing.remove(hash);
            }
        }
    }
}

/// How the bound is set.
#[derive(Clone)]
enum Limit {
    Fixed(u64),
    Automatic(Arc<AutoBound>),
}

/// An automatic bound: where it reads the library's size and the room on
/// the cache's volume, and what it last worked out.
struct AutoBound {
    albums: LocalCatalog,
    volume: PathBuf,
    bytes: AtomicU64,
    /// When it was last worked out, in ms by the cache's clock.
    worked_out_at: AtomicI64,
    /// Set while one worker works it out.
    working: AtomicBool,
    /// Set once a failed working-out has been logged, until one succeeds.
    failure_logged: AtomicBool,
}

/// The bound worked out from the library's album count, the free space on
/// the cache's volume and what the cache holds now.
fn automatic_bound(albums: u64, free: u64, cached: u64) -> u64 {
    let wanted = AUTO_BASE
        .saturating_add(albums.saturating_mul(AUTO_PER_ALBUM))
        .max(AUTO_FLOOR);
    let room = free.saturating_add(cached) / 4;
    wanted.min(room).max(AUTO_MINIMUM)
}

/// The bounded disk cache. Cheap to clone.
#[derive(Clone)]
pub struct ArtworkCache {
    root: PathBuf,
    limit: Limit,
    index: Arc<Mutex<Option<Index>>>,
    clock: Arc<dyn Clock>,
    /// Set once a background index load has been started.
    load_started: Arc<AtomicBool>,
    /// Set while a full pass runs, so two never overlap.
    pass_running: Arc<AtomicBool>,
    /// Set once a key walk's read error has been logged, until a walk reads
    /// cleanly.
    walk_error_logged: Arc<AtomicBool>,
}

impl std::fmt::Debug for ArtworkCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtworkCache")
            .field("root", &self.root)
            .field("limit", &self.limit())
            .finish_non_exhaustive()
    }
}

/// SHA-256 of some bytes.
fn sha256(bytes: &[u8]) -> Digest {
    Sha256::digest(bytes).into()
}

/// Hex SHA-256 of some bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    to_hex(&sha256(bytes))
}

fn to_hex(digest: &Digest) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The digest a 64-character lowercase hex text spells, the form every
/// file name and record here is written in.
fn parse_hex(text: &str) -> Option<Digest> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut digest = [0u8; 32];
    for (byte, pair) in digest.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(digest)
}

impl ArtworkCache {
    /// A cache rooted at `root` (normally `<cache_dir>/covers`), holding at
    /// most `max_bytes` of images, its markers dated by `clock`. Nothing
    /// touches the disk until first use.
    pub fn new(root: PathBuf, max_bytes: u64, clock: Arc<dyn Clock>) -> Self {
        Self::with_limit(root, Limit::Fixed(max_bytes), clock)
    }

    /// A cache rooted at `root` whose bound follows the library: the albums
    /// `albums` lists and the room on the volume holding `volume` (the cache
    /// folder, which exists before `root` does). Nothing touches the disk
    /// until first use.
    pub fn sized_to_library(
        root: PathBuf,
        albums: LocalCatalog,
        volume: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let auto = AutoBound {
            albums,
            volume,
            bytes: AtomicU64::new(AUTO_FLOOR),
            worked_out_at: AtomicI64::new(i64::MIN),
            working: AtomicBool::new(false),
            failure_logged: AtomicBool::new(false),
        };
        Self::with_limit(root, Limit::Automatic(Arc::new(auto)), clock)
    }

    fn with_limit(root: PathBuf, limit: Limit, clock: Arc<dyn Clock>) -> Self {
        Self {
            root,
            limit,
            index: Arc::new(Mutex::new(None)),
            clock,
            load_started: Arc::new(AtomicBool::new(false)),
            pass_running: Arc::new(AtomicBool::new(false)),
            walk_error_logged: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The bound as it stands and whether it is automatic.
    pub fn limit(&self) -> CacheLimit {
        match &self.limit {
            Limit::Fixed(bytes) => CacheLimit {
                bytes: *bytes,
                automatic: false,
            },
            Limit::Automatic(auto) => CacheLimit {
                bytes: auto.bytes.load(Ordering::Acquire),
                automatic: true,
            },
        }
    }

    /// Whether the index is in memory. When it is not, start loading it in
    /// the background, once, and answer false: a caller that must not wait
    /// on the disk treats the cache as knowing nothing yet.
    pub fn loaded_or_start(&self) -> bool {
        if self.lock().is_some() {
            return true;
        }
        if !self.load_started.swap(true, Ordering::AcqRel) {
            let cache = self.clone();
            tokio::spawn(async move { cache.ensure_index().await });
        }
        false
    }

    /// Whether `key` holds a "no art" marker that has not expired, and no
    /// image. False while the index is not loaded.
    pub fn knows_missing(&self, key: &str) -> bool {
        let id = key_id(key);
        let now = self.unix_now();
        self.lock().as_ref().is_some_and(|index| {
            !index.held.contains_key(&id) && index.misses.get(&id).is_some_and(|until| *until > now)
        })
    }

    /// Whether `key` points at an image, as far as the loaded index knows.
    pub fn holds(&self, key: &str) -> bool {
        let id = key_id(key);
        self.lock()
            .as_ref()
            .is_some_and(|index| index.held.contains_key(&id))
    }

    /// Seconds since the epoch, by the cache's clock.
    pub fn unix_now(&self) -> u64 {
        u64::try_from(self.clock.now_unix()).unwrap_or(0)
    }

    /// Read a key record.
    pub async fn key(&self, key: &str) -> Option<KeyEntry> {
        self.ensure_index().await;
        let path = self.key_path(key);
        let text = blocking(move || std::fs::read_to_string(path).ok()).await??;
        parse_key(&text)
    }

    /// Whether a blob is on disk, by a metadata check only: no read, and
    /// its place in the eviction order stays as it was.
    pub async fn has_blob(&self, hash: &str) -> bool {
        let Some(digest) = parse_hex(hash) else {
            return false;
        };
        let path = blob_path(&self.root, &digest);
        blocking(move || path.is_file()).await.unwrap_or(false)
    }

    /// Read a blob by content hash, marking it recently used.
    pub async fn blob(&self, hash: &str) -> Option<Vec<u8>> {
        let digest = parse_hex(hash)?;
        let path = blob_path(&self.root, &digest);
        let bytes = blocking(move || std::fs::read(path).ok()).await??;
        if let Some(index) = self.lock().as_mut() {
            index.touch(&digest);
        }
        Some(bytes)
    }

    /// The credit stored with the image under `key`, while that image is on
    /// disk. A record without one, written before credits were kept
    /// included, reads as `None`.
    pub async fn credit(&self, key: &str) -> Option<ImageCredit> {
        let path = self.key_path(key);
        let text = blocking(move || std::fs::read_to_string(path).ok()).await??;
        let Some(KeyEntry::Hit { hash, .. }) = parse_key(&text) else {
            return None;
        };
        if !self.has_blob(&hash).await {
            return None;
        }
        parse_credit(&text)
    }

    /// Store image bytes, optionally under a key, and return their hash.
    /// Evicts old blobs when the cache grows past its bound.
    pub async fn put(&self, key: Option<&str>, bytes: Vec<u8>, content_type: &str) -> String {
        self.put_credited(key, bytes, content_type, None).await
    }

    /// [`ArtworkCache::put`], with `credit` kept in the key record beside
    /// the image.
    pub async fn put_credited(
        &self,
        key: Option<&str>,
        bytes: Vec<u8>,
        content_type: &str,
        credit: Option<&ImageCredit>,
    ) -> String {
        let digest = sha256(&bytes);
        let hash = to_hex(&digest);
        let size = bytes.len() as u64;
        let blob = blob_path(&self.root, &digest);
        let id = key.map(key_id);
        let record = id.map(|id| {
            (
                key_file(&self.root, &id),
                hit_record(&hash, content_type, credit),
            )
        });
        self.ensure_index().await;
        // Noted before the disk is touched, so a full pass that has this
        // image down as an orphan keeps it.
        if let Some(index) = self.lock().as_mut() {
            index.begin_write(&digest);
        }
        let written = blocking(move || -> std::io::Result<()> {
            if !blob.exists() {
                write_atomic(&blob, &bytes)?;
            }
            if let Some((path, text)) = record {
                write_atomic(&path, text.as_bytes())?;
            }
            Ok(())
        })
        .await;
        if let Some(Err(error)) = &written {
            tracing::warn!(%error, "cover cache write failed; serving without caching");
        }
        let over = {
            let mut guard = self.lock();
            let Some(index) = guard.as_mut() else {
                return hash;
            };
            index.end_write(&digest);
            if !matches!(written, Some(Ok(()))) {
                return hash;
            }
            index.clock += 1;
            let used = BlobUse {
                size,
                last_used: index.clock,
            };
            if index.blobs.insert(digest, used).is_none() {
                index.total += size;
            }
            if let Some(id) = id {
                index.misses.remove(&id);
                index.held.insert(id, digest);
            }
            index.total > self.limit().bytes
        };
        if over {
            self.make_room(digest).await;
        }
        hash
    }

    /// Point `key` at a blob already stored under `hash`, without reading
    /// or hashing the bytes again.
    pub async fn put_key(&self, key: &str, hash: &str, content_type: &str) {
        let Some(digest) = parse_hex(hash) else {
            return;
        };
        let id = key_id(key);
        self.ensure_index().await;
        // Noted first, like a put, so a running full pass keeps the image.
        let indexed = self.lock().as_mut().map(|index| {
            index.begin_write(&digest);
            index.blobs.contains_key(&digest)
        });
        // An image a pass removed before the write was noted is not named:
        // the record would point at nothing.
        if indexed == Some(false) && !self.has_blob(hash).await {
            if let Some(index) = self.lock().as_mut() {
                index.end_write(&digest);
            }
            tracing::debug!("cover cache key not written: its image is gone");
            return;
        }
        let text = hit_record(hash, content_type, None);
        let target = key_file(&self.root, &id);
        let written = blocking(move || write_atomic(&target, text.as_bytes())).await;
        if let Some(Err(error)) = &written {
            tracing::warn!(%error, "cover cache key not written");
        }
        if let Some(index) = self.lock().as_mut() {
            index.end_write(&digest);
            if matches!(written, Some(Ok(()))) {
                index.touch(&digest);
                index.misses.remove(&id);
                index.held.insert(id, digest);
            }
        }
    }

    /// Record that the source had no art for `key` for `ttl_secs`. A key
    /// that already holds an image still on disk keeps it: a failed fetch
    /// running beside one that succeeded never blanks the stored image.
    pub async fn put_miss(&self, key: &str, ttl_secs: u64) {
        self.put_marker(key, Marker::Miss, ttl_secs).await;
    }

    /// Record that the source could not be asked about `key`, for
    /// `ttl_secs`. Like [`ArtworkCache::put_miss`], a stored image is kept.
    pub async fn put_unreachable(&self, key: &str, ttl_secs: u64) {
        self.put_marker(key, Marker::Unreachable, ttl_secs).await;
    }

    async fn put_marker(&self, key: &str, marker: Marker, ttl_secs: u64) {
        if let Some(KeyEntry::Hit { hash, .. }) = self.key(key).await
            && self.has_blob(&hash).await
        {
            return;
        }
        let id = key_id(key);
        let until = self.unix_now() + ttl_secs;
        let kind = match marker {
            Marker::Miss => "miss",
            Marker::Unreachable => "unreachable",
        };
        let text = format!("{kind}\n{until}\n");
        let target = key_file(&self.root, &id);
        let written = match blocking(move || write_atomic(&target, text.as_bytes())).await {
            Some(Ok(())) => true,
            Some(Err(error)) => {
                tracing::warn!(%error, "cover cache miss marker not written");
                false
            }
            None => false,
        };
        if let Some(index) = self.lock().as_mut() {
            index.held.remove(&id);
            match marker {
                Marker::Miss if written => {
                    index.misses.insert(id, until);
                }
                _ => {
                    index.misses.remove(&id);
                }
            }
        }
        let due = self.lock().as_ref().is_some_and(|index| {
            index
                .last_full_clean
                .is_none_or(|last| last.elapsed() >= FULL_CLEAN_EVERY)
        });
        if due {
            self.full_clean().await;
        }
    }

    /// Images held and their total size in bytes.
    pub async fn usage(&self) -> (u64, u64) {
        self.ensure_index().await;
        self.lock()
            .as_ref()
            .map_or((0, 0), |index| (index.blobs.len() as u64, index.total))
    }

    /// Delete every image and key record (the admin "clear covers"). Art
    /// is fetched or read again on the next request. Returns how many
    /// images went.
    pub async fn clear(&self) -> u64 {
        self.ensure_index().await;
        let cleared = {
            let mut guard = self.lock();
            let count = guard.as_ref().map_or(0, |index| index.blobs.len() as u64);
            *guard = Some(Index {
                last_full_clean: Some(Instant::now()),
                ..Index::default()
            });
            count
        };
        let root = self.root.clone();
        let removed = blocking(move || {
            for dir in ["blobs", KEYS_DIR, AUDIODB_KEYS_DIR] {
                let path = root.join(dir);
                if let Err(error) = std::fs::remove_dir_all(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, path = %path.display(), "cover cache clear incomplete");
                }
            }
        })
        .await;
        if removed.is_none() {
            tracing::warn!("cover cache clear task failed");
        }
        cleared
    }

    /// Load the blob index from disk once, by scanning the blob folders.
    async fn ensure_index(&self) {
        if self.lock().is_some() {
            return;
        }
        let blobs_dir = self.root.join("blobs");
        let scanned = blocking(move || scan_blobs(&blobs_dir))
            .await
            .unwrap_or_default();
        {
            let mut guard = self.lock();
            if guard.is_some() {
                return;
            }
            let total = scanned.values().map(|entry| entry.size).sum();
            let clock = scanned.len() as u64;
            *guard = Some(Index {
                blobs: scanned,
                total,
                clock,
                ..Index::default()
            });
        }
        self.work_out_bound().await;
        self.full_clean().await;
    }

    /// One pass over every key record: drop those whose blob is gone, miss
    /// markers past their time, and unreadable records, and rebuild the
    /// index's view of the rest. When every record read cleanly, the images
    /// no record names go too.
    async fn full_clean(&self) {
        if self.pass_running.swap(true, Ordering::AcqRel) {
            return;
        }
        let _running = ClearOnDrop(&self.pass_running);
        if let Some(snapshot) = self.pass_snapshot()
            && let Some(walk) = self.walk_keys(snapshot).await
            && walk.clean
        {
            self.remove_orphans(walk.snapshot, walk.named).await;
        }
    }

    /// Every image in the index with its last use, sorted by hash. `None`
    /// while the index is not loaded.
    fn pass_snapshot(&self) -> Option<Snapshot> {
        let guard = self.lock();
        let index = guard.as_ref()?;
        let mut snapshot: Snapshot = index
            .blobs
            .iter()
            .map(|(digest, entry)| (*digest, entry.last_used))
            .collect();
        snapshot.sort_unstable();
        Some(snapshot)
    }

    /// Walk both key folders against `snapshot` and merge what they hold
    /// into the index. The pass counts as done only from here: one dropped
    /// mid-walk leaves the next write to run it again, so a first pass's
    /// held images and misses are never left out of the index for an hour.
    /// `None` if the walk's task failed.
    async fn walk_keys(&self, snapshot: Snapshot) -> Option<Walk> {
        let root = self.root.clone();
        let now = self.unix_now();
        let walked = blocking(move || {
            let mut pass = KeyPass::default();
            for dir in KeyDir::ALL {
                clean_key_files(&root, dir, &snapshot, now, &mut pass);
            }
            (snapshot, pass)
        })
        .await;
        let Some((snapshot, mut pass)) = walked else {
            if let Some(index) = self.lock().as_mut() {
                index.last_full_clean = Some(Instant::now());
            }
            return None;
        };
        if let Some(index) = self.lock().as_mut() {
            index.last_full_clean = Some(Instant::now());
            // Merged, never replaced: a write made while the pass ran has
            // already updated the index, and an image it stored wins.
            index.misses.retain(|_, until| *until > now);
            for (id, until) in pass.misses {
                if !index.held.contains_key(&id) {
                    index.misses.entry(id).or_insert(until);
                }
            }
            for (id, digest) in &pass.held {
                index.held.entry(*id).or_insert(*digest);
            }
        }
        let named = std::mem::take(&mut pass.held)
            .into_iter()
            .map(|(_, digest)| digest)
            .collect();
        if pass.removed > 0 {
            tracing::debug!(
                removed = pass.removed,
                "cover cache dropped stale key records"
            );
        }
        let clean = pass.errors == 0;
        if clean {
            self.walk_error_logged.store(false, Ordering::Release);
        } else if !self.walk_error_logged.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                errors = pass.errors,
                error = pass.first_error.as_deref().unwrap_or(""),
                "cover cache could not read every key record; no unnamed image is removed"
            );
        }
        Some(Walk {
            snapshot,
            named,
            clean,
        })
    }

    /// Delete the images of `snapshot` that no key record names, on disk
    /// as the walk read it (`walked`) or in the index, and that nobody used
    /// since the snapshot. They are worked through a batch at a time, the
    /// index lock released between batches: under the lock each is chosen
    /// again, taken out of the index and unlinked, so a write that names it
    /// either comes first and keeps it, or comes after and stores it again.
    /// One window stays open: a second server sharing the cache folder that
    /// writes a record between this pass's walk and its removal can lose
    /// the image it names; that record then reads as a miss and the art is
    /// fetched again.
    async fn remove_orphans(&self, snapshot: Snapshot, walked: HashSet<Digest>) {
        let index = Arc::clone(&self.index);
        let root = self.root.clone();
        let removed = blocking(move || {
            let (named, candidates) = {
                let guard = lock_index(&index);
                let Some(index) = guard.as_ref() else {
                    return 0usize;
                };
                let named = named_images(walked, index);
                let chosen: HashSet<Digest> = choose_orphans(&snapshot, &named, index)
                    .into_iter()
                    .collect();
                let candidates: Snapshot = snapshot
                    .into_iter()
                    .filter(|(digest, _)| chosen.contains(digest))
                    .collect();
                (named, candidates)
            };
            let mut removed = 0;
            for batch in candidates.chunks(UNLINK_BATCH) {
                let mut guard = lock_index(&index);
                let Some(index) = guard.as_mut() else {
                    break;
                };
                for digest in choose_orphans(batch, &named, index) {
                    if remove_blob(index, &root, &digest) {
                        removed += 1;
                    }
                }
            }
            removed
        })
        .await
        .unwrap_or(0);
        if removed > 0 {
            tracing::debug!(removed, "cover cache removed images no key names");
        }
    }

    /// Work out an automatic bound again when the last time was an hour
    /// ago or more, then evict down to 90% of the bound.
    async fn make_room(&self, keep: Digest) {
        if let Limit::Automatic(auto) = &self.limit {
            let last = auto.worked_out_at.load(Ordering::Acquire);
            if self.clock.now_ms().saturating_sub(last) >= BOUND_EVERY_MS {
                self.work_out_bound().await;
            }
        }
        self.evict(keep, self.limit().bytes).await;
    }

    /// Work out an automatic bound from the album count and the volume's
    /// room. One worker at a time; a read that fails keeps the bound as it
    /// was.
    async fn work_out_bound(&self) {
        let Limit::Automatic(auto) = &self.limit else {
            return;
        };
        if auto.working.swap(true, Ordering::AcqRel) {
            return;
        }
        let _working = ClearOnDrop(&auto.working);
        auto.worked_out_at
            .store(self.clock.now_ms(), Ordering::Release);
        let albums = auto.albums.stats().await.map(|stats| stats.total_albums);
        let volume = auto.volume.clone();
        let free = blocking(move || {
            droppedneedle_library::adapters::free_bytes_for(&volume)
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap_or_else(|| Err("free space task failed".to_owned()));
        let cached = self.lock().as_ref().map_or(0, |index| index.total);
        match (albums, free) {
            (Ok(albums), Ok(free)) => {
                let bytes = automatic_bound(albums, free, cached);
                auto.bytes.store(bytes, Ordering::Release);
                auto.failure_logged.store(false, Ordering::Release);
                tracing::debug!(bytes, albums, free, "cover cache bound worked out");
            }
            (albums, free) => {
                if !auto.failure_logged.swap(true, Ordering::AcqRel) {
                    let albums = albums.err().map(|error| error.to_string());
                    let free = free.err();
                    tracing::warn!(
                        albums = albums.as_deref().unwrap_or(""),
                        free = free.as_deref().unwrap_or(""),
                        bytes = auto.bytes.load(Ordering::Acquire),
                        "cover cache bound not worked out; keeping the last one"
                    );
                }
            }
        }
    }

    /// When the cache is over `bound`, delete least recently used blobs
    /// until it fills 90% of it, never the one just written or one a write
    /// is storing or naming. The order is taken once; the blobs then go a
    /// batch at a time, the index lock released between batches, each
    /// checked again (still unused since the order was taken), taken out
    /// of the index and unlinked under the lock. The key records that
    /// pointed at them go after.
    async fn evict(&self, keep: Digest, bound: u64) {
        let index = Arc::clone(&self.index);
        let root = self.root.clone();
        let Some(victims) = blocking(move || {
            let mut order: Vec<(u64, Digest)> = {
                let guard = lock_index(&index);
                let Some(index) = guard.as_ref() else {
                    return Vec::new();
                };
                if index.total <= bound {
                    return Vec::new();
                }
                index
                    .blobs
                    .iter()
                    .filter(|(digest, _)| **digest != keep)
                    .map(|(digest, entry)| (entry.last_used, *digest))
                    .collect()
            };
            order.sort_unstable();
            let target = bound - bound / 10;
            let mut gone = HashSet::new();
            'batches: for batch in order.chunks(UNLINK_BATCH) {
                let mut guard = lock_index(&index);
                let Some(index) = guard.as_mut() else {
                    break;
                };
                for (used, digest) in batch {
                    if index.total <= target {
                        break 'batches;
                    }
                    let unused = index
                        .blobs
                        .get(digest)
                        .is_some_and(|entry| entry.last_used == *used);
                    if unused
                        && !index.writing.contains_key(digest)
                        && remove_blob(index, &root, digest)
                    {
                        gone.insert(*digest);
                    }
                }
            }
            let mut guard = lock_index(&index);
            let Some(index) = guard.as_mut() else {
                return Vec::new();
            };
            let mut keys = Vec::new();
            index.held.retain(|id, digest| {
                let evicted = gone.contains(digest);
                if evicted {
                    keys.push((*id, *digest));
                }
                !evicted
            });
            tracing::debug!(count = gone.len(), "cover cache evicted old images");
            keys
        })
        .await
        else {
            return;
        };
        if victims.is_empty() {
            return;
        }
        let root = self.root.clone();
        let done = blocking(move || {
            // Only records still pointing at their evicted blob go; a key
            // may have moved on to newer art since.
            for (id, digest) in victims {
                let path = key_file(&root, &id);
                let points_here = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| parse_key(&text))
                    .is_some_and(|entry| {
                        matches!(entry, KeyEntry::Hit { hash, .. } if parse_hex(&hash) == Some(digest))
                    });
                if points_here
                    && let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::debug!(%error, path = %path.display(), "cover cache key record kept");
                }
            }
        })
        .await;
        if done.is_none() {
            tracing::warn!("cover cache key cleanup task failed");
        }
    }

    fn key_path(&self, key: &str) -> PathBuf {
        key_file(&self.root, &key_id(key))
    }

    fn lock(&self) -> MutexGuard<'_, Option<Index>> {
        lock_index(&self.index)
    }
}

/// Take `digest` out of `index` and unlink its file, under the caller's
/// hold of the index lock. A file that would not go is put back in the
/// index. True when the image is gone.
fn remove_blob(index: &mut Index, root: &Path, digest: &Digest) -> bool {
    let Some(entry) = index.blobs.remove(digest) else {
        return false;
    };
    index.total = index.total.saturating_sub(entry.size);
    let path = blob_path(root, digest);
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "cover cache image not removed");
            index.blobs.insert(*digest, entry);
            index.total += entry.size;
            false
        }
    }
}

/// Clears its flag when dropped, so a caller that stops waiting midway (a
/// cancelled request) never leaves the flag set.
struct ClearOnDrop<'a>(&'a AtomicBool);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn lock_index(index: &Mutex<Option<Index>>) -> MutexGuard<'_, Option<Index>> {
    index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Every image a key record names: the ones the pass's walk read on disk
/// (`walked`) and the ones the index holds. Both count, so an index entry
/// a later write left behind never hides what the disk says.
fn named_images(mut walked: HashSet<Digest>, index: &Index) -> HashSet<Digest> {
    walked.extend(index.held.values().copied());
    walked
}

/// The images of `snapshot` that `index` still holds with the same last
/// use, that are not in `named` and that no write is storing or naming.
fn choose_orphans(
    snapshot: &[(Digest, u64)],
    named: &HashSet<Digest>,
    index: &Index,
) -> Vec<Digest> {
    snapshot
        .iter()
        .filter(|(digest, used)| {
            !named.contains(digest)
                && !index.writing.contains_key(digest)
                && index
                    .blobs
                    .get(digest)
                    .is_some_and(|entry| entry.last_used == *used)
        })
        .map(|(digest, _)| *digest)
        .collect()
}

/// The record a lookup key is stored under.
fn key_id(key: &str) -> KeyId {
    let dir = if key.starts_with(AUDIODB_KEY_PREFIX) {
        KeyDir::AudioDb
    } else {
        KeyDir::Art
    };
    KeyId {
        dir,
        name: sha256(key.as_bytes()),
    }
}

fn key_file(root: &Path, id: &KeyId) -> PathBuf {
    let name = to_hex(&id.name);
    let shard = name.get(..2).unwrap_or("00").to_owned();
    root.join(id.dir.name()).join(shard).join(name)
}

fn blob_path(root: &Path, digest: &Digest) -> PathBuf {
    let hash = to_hex(digest);
    let shard = hash.get(..2).unwrap_or("00").to_owned();
    root.join("blobs").join(shard).join(hash)
}

/// Run blocking file or image work off the async workers. `None` if the
/// task panicked, which is logged.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::error!(%error, "cover blocking task failed");
            None
        }
    }
}

fn parse_key(text: &str) -> Option<KeyEntry> {
    let mut lines = text.lines();
    match lines.next()? {
        "hit" => {
            let hash = lines.next()?.to_owned();
            let content_type = lines.next()?.to_owned();
            is_hash(&hash).then_some(KeyEntry::Hit { hash, content_type })
        }
        "miss" => Some(KeyEntry::Miss {
            until: lines.next()?.parse().ok()?,
        }),
        "unreachable" => Some(KeyEntry::Unreachable {
            until: lines.next()?.parse().ok()?,
        }),
        _ => None,
    }
}

/// A key record pointing at a blob, with the image's credit on a line of its
/// own when it has one.
fn hit_record(hash: &str, content_type: &str, credit: Option<&ImageCredit>) -> String {
    let mut record = format!("hit\n{hash}\n{content_type}\n");
    match credit.map(serde_json::to_string) {
        Some(Ok(json)) => {
            record.push_str(CREDIT_LINE);
            record.push_str(&json);
            record.push('\n');
        }
        Some(Err(error)) => tracing::warn!(%error, "image credit not stored"),
        None => {}
    }
    record
}

/// The credit line of a key record. A record without one, or with one that
/// does not read, has no credit.
fn parse_credit(text: &str) -> Option<ImageCredit> {
    let json = text
        .lines()
        .skip(3)
        .find_map(|line| line.strip_prefix(CREDIT_LINE))?;
    serde_json::from_str(json).ok()
}

fn is_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let Some(dir) = path.parent() else {
        return Err(std::io::Error::other("cache path has no parent"));
    };
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Unique per write, so two writers of the same blob never share a
    // temporary file.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// What a full pass's walk of the key records leaves for its removal step.
struct Walk {
    /// The images the pass started from.
    snapshot: Snapshot,
    /// The images the records it read name.
    named: HashSet<Digest>,
    /// Whether every folder and record read cleanly.
    clean: bool,
}

/// What one pass over the key records found.
#[derive(Default)]
struct KeyPass {
    /// Records deleted.
    removed: usize,
    /// Every record kept that points at a blob, with the blob.
    held: Vec<(KeyId, Digest)>,
    /// Every "no art" marker kept, with its expiry.
    misses: Vec<(KeyId, u64)>,
    /// Folders and records that could not be read. The top folder not
    /// existing is not one.
    errors: usize,
    /// The first of them, for the log.
    first_error: Option<String>,
}

impl KeyPass {
    fn failed(&mut self, path: &Path, error: &std::io::Error) {
        self.errors += 1;
        if self.first_error.is_none() {
            self.first_error = Some(format!("{}: {error}", path.display()));
        }
    }
}

/// Delete the key records in `dir` that point at a blob neither in
/// `snapshot` (sorted by hash) nor on disk, expired markers, and records
/// that do not parse, and list what is kept in `pass`.
fn clean_key_files(
    root: &Path,
    dir: KeyDir,
    snapshot: &[(Digest, u64)],
    now: u64,
    pass: &mut KeyPass,
) {
    let top = root.join(dir.name());
    let shards = match std::fs::read_dir(&top) {
        Ok(shards) => shards,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            pass.failed(&top, &error);
            return;
        }
    };
    for shard in shards {
        let shard = match shard {
            Ok(shard) => match shard.file_type() {
                Ok(kind) if kind.is_dir() => shard.path(),
                // Not one of ours, such as a file a share's client leaves.
                Ok(_) => continue,
                Err(error) => {
                    pass.failed(&shard.path(), &error);
                    continue;
                }
            },
            Err(error) => {
                pass.failed(&top, &error);
                continue;
            }
        };
        let files = match std::fs::read_dir(&shard) {
            Ok(files) => files,
            Err(error) => {
                pass.failed(&shard, &error);
                continue;
            }
        };
        for file in files {
            let file = match file {
                Ok(file) => file,
                Err(error) => {
                    pass.failed(&shard, &error);
                    continue;
                }
            };
            let name = file.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Some(key_hash) = parse_hex(&name) else {
                continue;
            };
            let id = KeyId {
                dir,
                name: key_hash,
            };
            let path = file.path();
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    pass.failed(&path, &error);
                    continue;
                }
            };
            let entry = std::str::from_utf8(&bytes).ok().and_then(parse_key);
            let stale = match entry {
                Some(KeyEntry::Hit { hash, .. }) => match parse_hex(&hash) {
                    Some(digest)
                        if snapshot
                            .binary_search_by(|(held, _)| held.cmp(&digest))
                            .is_ok()
                            || blob_path(root, &digest).is_file() =>
                    {
                        pass.held.push((id, digest));
                        false
                    }
                    _ => true,
                },
                Some(KeyEntry::Miss { until }) => {
                    if until > now {
                        pass.misses.push((id, until));
                    }
                    until <= now
                }
                Some(KeyEntry::Unreachable { until }) => until <= now,
                None => true,
            };
            if stale && std::fs::remove_file(&path).is_ok() {
                pass.removed += 1;
            }
        }
    }
}

/// Every blob on disk with its size; older files count as less recently
/// used, so a restart keeps a sensible eviction order.
fn scan_blobs(dir: &Path) -> HashMap<Digest, BlobUse> {
    let mut found: Vec<(SystemTime, Digest, u64)> = Vec::new();
    let Ok(shards) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    for shard in shards.flatten() {
        let Ok(files) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Some(digest) = parse_hex(&file.file_name().to_string_lossy()) else {
                continue;
            };
            if let Ok(meta) = file.metadata() {
                let modified = meta.modified().unwrap_or(UNIX_EPOCH);
                found.push((modified, digest, meta.len()));
            }
        }
    }
    found.sort();
    found
        .into_iter()
        .enumerate()
        .map(|(order, (_, digest, size))| {
            (
                digest,
                BlobUse {
                    size,
                    last_used: order as u64,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker never outlives stored art. A "no art" marker is known in
    /// memory until it expires by the cache's clock, and an image stored
    /// under its key ends it at once. A failure marker never overwrites a
    /// stored image: a background fetch that fails beside a page fetch that
    /// stored the cover leaves the cover in place.
    #[tokio::test]
    async fn markers_never_outlive_stored_art() {
        let root = std::env::temp_dir().join(format!(
            "dn-artwork-markers-{}-{}",
            std::process::id(),
            droppedneedle_kernel::clock::SystemClock.now_ms()
        ));
        let clock = Arc::new(droppedneedle_kernel::test_support::ManualClock::at(
            1_000_000,
        ));
        let cache = ArtworkCache::new(root.clone(), 1024 * 1024, clock.clone());
        let key = "caa:release-group:aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b:500";
        assert!(!cache.loaded_or_start(), "nothing is known before the load");
        cache.ensure_index().await;

        cache.put_miss(key, 60).await;
        assert!(cache.knows_missing(key));
        clock.advance(60_000);
        assert!(!cache.knows_missing(key), "expired with the marker");

        cache.put_miss(key, 60).await;
        cache.put(Some(key), b"cover".to_vec(), "image/jpeg").await;
        assert!(!cache.knows_missing(key), "stored art ends the miss");
        assert!(cache.holds(key));

        let stored = "caa:release-group:aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b:250";
        let hash = cache
            .put(Some(stored), b"cover".to_vec(), "image/jpeg")
            .await;
        cache.put_miss(stored, 900).await;
        assert_eq!(
            cache.key(stored).await,
            Some(KeyEntry::Hit {
                hash,
                content_type: "image/jpeg".to_owned()
            }),
            "a failure never blanks a stored image"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dn-artwork-{name}-{}-{}",
            std::process::id(),
            droppedneedle_kernel::clock::SystemClock.now_ms()
        ))
    }

    fn manual_cache(root: &Path, max_bytes: u64) -> ArtworkCache {
        let clock = Arc::new(droppedneedle_kernel::test_support::ManualClock::at(
            1_000_000,
        ));
        ArtworkCache::new(root.to_owned(), max_bytes, clock)
    }

    fn on_disk(cache: &ArtworkCache, hash: &str) -> bool {
        parse_hex(hash).is_some_and(|digest| blob_path(&cache.root, &digest).is_file())
    }

    /// The choice of unnamed images is a pure step over the pass's snapshot
    /// and the index as it stands: an image goes only when the snapshot had
    /// it, no key record names it, nobody used it since and no write is
    /// storing or naming it.
    #[test]
    fn an_orphan_goes_only_while_unnamed_and_unused_since_the_snapshot() {
        let (named, orphan, used_since, written_since, writing) =
            ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32], [5u8; 32]);
        let mut index = Index::default();
        for (digest, last_used) in [
            (named, 1),
            (orphan, 2),
            (used_since, 9),
            (written_since, 10),
            (writing, 4),
        ] {
            index.blobs.insert(
                digest,
                BlobUse {
                    size: 10,
                    last_used,
                },
            );
        }
        index.held.insert(
            KeyId {
                dir: KeyDir::Art,
                name: [9u8; 32],
            },
            named,
        );
        index.writing.insert(writing, 1);
        let snapshot = vec![(named, 1), (orphan, 2), (used_since, 3), (writing, 4)];
        let walked = HashSet::new();
        assert_eq!(
            choose_orphans(&snapshot, &named_images(walked, &index), &index),
            vec![orphan]
        );
    }

    /// An image a record on disk names is kept even when the index has that
    /// record pointing elsewhere: what the walk read counts.
    #[test]
    fn an_image_only_the_walk_saw_named_is_kept() {
        let (on_disk, in_memory) = ([1u8; 32], [2u8; 32]);
        let mut index = Index::default();
        for digest in [on_disk, in_memory] {
            index.blobs.insert(
                digest,
                BlobUse {
                    size: 10,
                    last_used: 1,
                },
            );
        }
        index.held.insert(
            KeyId {
                dir: KeyDir::Art,
                name: [9u8; 32],
            },
            in_memory,
        );
        let snapshot = vec![(on_disk, 1), (in_memory, 1)];
        let walked = HashSet::from([on_disk]);
        assert!(choose_orphans(&snapshot, &named_images(walked, &index), &index).is_empty());
    }

    /// A pass whose walk of the key records met a read error removes no
    /// image, even one no record names.
    #[tokio::test]
    async fn a_walk_with_a_read_error_removes_nothing() {
        let root = temp_root("walk-error");
        let cache = manual_cache(&root, 1024 * 1024);
        let orphan = cache.put(None, b"orphan".to_vec(), "image/jpeg").await;
        let unreadable = root.join(KEYS_DIR).join("ab").join("ab".repeat(32));
        std::fs::create_dir_all(&unreadable).expect("a folder where a record should be");

        cache.full_clean().await;
        assert!(on_disk(&cache, &orphan), "kept after a walk that failed");
        assert_eq!(cache.usage().await.0, 1);

        std::fs::remove_dir_all(&unreadable).expect("folder removed");
        cache.full_clean().await;
        assert!(
            !on_disk(&cache, &orphan),
            "removed once the walk reads cleanly"
        );
        assert_eq!(cache.usage().await, (0, 0));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A write that names or stores an orphan while the pass runs, after its
    /// walk and before its removal step, keeps the image.
    #[tokio::test]
    async fn a_write_during_the_pass_keeps_its_image() {
        let root = temp_root("pass-race");
        let cache = manual_cache(&root, 1024 * 1024);
        let duplicate = cache.put(None, b"duplicate".to_vec(), "image/jpeg").await;
        let named = cache.put(None, b"named".to_vec(), "image/jpeg").await;
        let unnamed = cache.put(None, b"unnamed".to_vec(), "image/jpeg").await;

        let snapshot = cache.pass_snapshot().expect("index loaded");
        let walk = cache.walk_keys(snapshot).await.expect("walked");
        assert!(walk.clean);
        cache.put(None, b"duplicate".to_vec(), "image/jpeg").await;
        cache
            .put_key("caa:release:named:500", &named, "image/jpeg")
            .await;
        cache.remove_orphans(walk.snapshot, walk.named).await;

        assert!(on_disk(&cache, &duplicate), "a duplicate put keeps it");
        assert!(on_disk(&cache, &named), "a key pointed at it keeps it");
        assert!(!on_disk(&cache, &unnamed), "an untouched orphan goes");
        assert_eq!(
            cache.usage().await,
            (2, (b"duplicate".len() + b"named".len()) as u64)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A pass whose caller stops waiting during the walk leaves no pass
    /// marked running, so the next one runs.
    #[tokio::test]
    async fn a_pass_dropped_mid_walk_lets_the_next_one_run() {
        let root = temp_root("dropped-pass");
        let cache = manual_cache(&root, 1024 * 1024);
        for n in 0..200u32 {
            let key = format!("caa:release:{n}:500");
            cache
                .put(Some(&key), n.to_le_bytes().to_vec(), "image/jpeg")
                .await;
        }
        let dropped = tokio::time::timeout(Duration::ZERO, cache.full_clean()).await;
        assert!(dropped.is_err(), "dropped while the walk ran");
        assert!(!cache.pass_running.load(Ordering::Acquire));

        let orphan = cache.put(None, b"orphan".to_vec(), "image/jpeg").await;
        cache.full_clean().await;
        assert!(!on_disk(&cache, &orphan), "the next pass ran");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Filling past the bound evicts down to 90% of it in one pass, least
    /// recently read first and never the image just written, and a
    /// duplicate put counts its size once.
    #[tokio::test]
    async fn eviction_frees_a_tenth_least_recently_read_first() {
        let root = temp_root("evict");
        let cache = manual_cache(&root, 1000);
        let image = |n: u8| vec![n; 100];
        let mut hashes = Vec::new();
        for n in 0..10 {
            hashes.push(cache.put(None, image(n), "image/jpeg").await);
        }
        cache.put(None, image(0), "image/jpeg").await;
        assert_eq!(cache.usage().await, (10, 1000), "a duplicate counts once");
        cache.blob(&hashes[1]).await.expect("read");

        let newest = cache.put(None, image(10), "image/jpeg").await;
        assert_eq!(cache.usage().await, (9, 900), "down to 90% in one pass");
        assert!(on_disk(&cache, &newest));
        for (n, hash) in hashes.iter().enumerate() {
            assert_eq!(on_disk(&cache, hash), !matches!(n, 2 | 3), "image {n}");
        }

        cache.put(None, image(11), "image/jpeg").await;
        assert_eq!(
            cache.usage().await,
            (10, 1000),
            "room left for the next write"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The automatic bound: 512 MiB and half a MiB per album, at least
    /// 1 GiB, at most a quarter of the volume's room (free plus cached),
    /// never under 64 MiB. A set bound is used as it is.
    #[test]
    fn the_bound_follows_the_library_and_the_volume() {
        const GIB: u64 = 1024 * MIB;
        let plenty = 1024 * GIB;
        assert_eq!(automatic_bound(0, plenty, 0), GIB, "the floor");
        assert_eq!(automatic_bound(2_000, plenty, 0), 1_512 * MIB);
        assert_eq!(automatic_bound(25_000, plenty, 0), 13_012 * MIB);
        assert_eq!(
            automatic_bound(25_000, 20 * GIB, 0),
            5 * GIB,
            "a quarter of the room"
        );
        assert_eq!(
            automatic_bound(25_000, 2 * GIB, 2 * GIB),
            GIB,
            "what it holds counts as room"
        );
        assert_eq!(automatic_bound(2_000, 100 * MIB, 0), 64 * MIB, "the least");

        let root = temp_root("fixed");
        let fixed = manual_cache(&root, 500 * MIB);
        assert_eq!(
            fixed.limit(),
            CacheLimit {
                bytes: 500 * MIB,
                automatic: false
            }
        );
    }
}
