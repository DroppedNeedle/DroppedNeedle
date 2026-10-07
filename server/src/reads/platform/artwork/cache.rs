//! Content-addressed cover cache on disk, bounded by size.
//!
//! Layout under `<cache_dir>/covers`:
//!
//! - `blobs/<aa>/<hash>`: image bytes, named by the SHA-256 of the bytes,
//!   so the same picture fetched for a release and its release group, or
//!   found in a folder and served through two routes, is stored once.
//! - `keys/<aa>/<key hash>`: a small text record mapping a lookup key (for
//!   example `caa:release-group:<mbid>:500`) to a blob, or recording that
//!   the archive had no art until a given time. TheAudioDB album
//!   thumbnails keep theirs under `keys-audiodb/` instead, so they can be
//!   forgotten on their own.
//!
//! When the blobs pass `COVER_CACHE_MAX_SIZE_MB` the least recently used
//! ones are deleted. A key whose blob was evicted reads as a miss, so the
//! art is fetched again. Eviction also deletes the key records that point
//! at the evicted blobs (a reverse map tracks them). A full pass over the
//! key records, dropping orphans and miss markers past their time, runs
//! when the cache loads and at most hourly after that. Every file operation runs on a blocking thread,
//! and writes go through a temporary file and a rename so a crash never
//! leaves a half-written image behind.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Least time between two full passes over the key records.
const FULL_CLEAN_EVERY: Duration = Duration::from_secs(3600);
/// Key records, by key hash.
const KEYS_DIR: &str = "keys";
/// Key records of TheAudioDB album thumbnails, kept apart so the AudioDB
/// clear can drop them without touching other art.
const AUDIODB_KEYS_DIR: &str = "keys-audiodb";
/// Keys that live in [`AUDIODB_KEYS_DIR`].
pub const AUDIODB_KEY_PREFIX: &str = "audiodb:";

use sha2::{Digest as _, Sha256};

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
    blobs: HashMap<String, BlobUse>,
    total: u64,
    clock: u64,
    /// Key record paths per blob hash, so eviction can drop them.
    keys_by_hash: HashMap<String, HashSet<PathBuf>>,
    /// When the last full pass over the key records ran.
    last_full_clean: Option<Instant>,
}

impl Index {
    fn touch(&mut self, hash: &str) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self.blobs.get_mut(hash) {
            entry.last_used = clock;
        }
    }
}

/// The bounded disk cache. Cheap to clone.
#[derive(Debug, Clone)]
pub struct ArtworkCache {
    root: PathBuf,
    max_bytes: u64,
    index: Arc<Mutex<Option<Index>>>,
}

/// Hex SHA-256 of some bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Seconds since the epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl ArtworkCache {
    /// A cache rooted at `root` (normally `<cache_dir>/covers`), holding at
    /// most `max_bytes` of images. Nothing touches the disk until first use.
    pub fn new(root: PathBuf, max_bytes: u64) -> Self {
        Self {
            root,
            max_bytes,
            index: Arc::new(Mutex::new(None)),
        }
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
        if !is_hash(hash) {
            return false;
        }
        let path = self.blob_path(hash);
        blocking(move || path.is_file()).await.unwrap_or(false)
    }

    /// Read a blob by content hash, marking it recently used.
    pub async fn blob(&self, hash: &str) -> Option<Vec<u8>> {
        if !is_hash(hash) {
            return None;
        }
        let path = self.blob_path(hash);
        let bytes = blocking(move || std::fs::read(path).ok()).await??;
        if let Some(index) = self.lock().as_mut() {
            index.touch(hash);
        }
        Some(bytes)
    }

    /// Store image bytes, optionally under a key, and return their hash.
    /// Evicts old blobs when the cache grows past its bound.
    pub async fn put(&self, key: Option<&str>, bytes: Vec<u8>, content_type: &str) -> String {
        let hash = sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let blob = self.blob_path(&hash);
        let key_path = key.map(|key| self.key_path(key));
        let key_file = key_path
            .clone()
            .map(|path| (path, format!("hit\n{hash}\n{content_type}\n")));
        let written = blocking(move || -> std::io::Result<bool> {
            let fresh = !blob.exists();
            if fresh {
                write_atomic(&blob, &bytes)?;
            }
            if let Some((path, text)) = key_file {
                write_atomic(&path, text.as_bytes())?;
            }
            Ok(fresh)
        })
        .await;
        match written {
            Some(Ok(fresh)) => {
                self.ensure_index().await;
                let over = {
                    let mut guard = self.lock();
                    let Some(index) = guard.as_mut() else {
                        return hash;
                    };
                    if fresh && !index.blobs.contains_key(&hash) {
                        index.total += size;
                    }
                    index.clock += 1;
                    let clock = index.clock;
                    index.blobs.insert(
                        hash.clone(),
                        BlobUse {
                            size,
                            last_used: clock,
                        },
                    );
                    if let Some(path) = key_path {
                        index
                            .keys_by_hash
                            .entry(hash.clone())
                            .or_default()
                            .insert(path);
                    }
                    index.total > self.max_bytes
                };
                if over {
                    self.evict(&hash).await;
                }
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "cover cache write failed; serving without caching");
            }
            None => {}
        }
        hash
    }

    /// Point `key` at a blob already stored under `hash`, without reading
    /// or hashing the bytes again.
    pub async fn put_key(&self, key: &str, hash: &str, content_type: &str) {
        if !is_hash(hash) {
            return;
        }
        let path = self.key_path(key);
        let text = format!("hit\n{hash}\n{content_type}\n");
        let target = path.clone();
        match blocking(move || write_atomic(&target, text.as_bytes())).await {
            Some(Ok(())) => {
                if let Some(index) = self.lock().as_mut() {
                    index.touch(hash);
                    index
                        .keys_by_hash
                        .entry(hash.to_owned())
                        .or_default()
                        .insert(path);
                }
            }
            Some(Err(error)) => tracing::warn!(%error, "cover cache key not written"),
            None => {}
        }
    }

    /// Record that the source had no art for `key` for `ttl_secs`.
    pub async fn put_miss(&self, key: &str, ttl_secs: u64) {
        let path = self.key_path(key);
        let text = format!("miss\n{}\n", unix_now() + ttl_secs);
        if let Some(Err(error)) = blocking(move || write_atomic(&path, text.as_bytes())).await {
            tracing::warn!(%error, "cover cache miss marker not written");
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

    /// Forget every TheAudioDB album thumbnail (the admin "clear AudioDB").
    /// Their key records go at once; the images themselves age out of the
    /// cache like any unused image. Returns how many records went.
    pub async fn clear_audiodb(&self) -> u64 {
        let dir = self.root.join(AUDIODB_KEYS_DIR);
        blocking(move || {
            let count = count_files(&dir);
            if let Err(error) = std::fs::remove_dir_all(&dir)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%error, path = %dir.display(), "AudioDB thumbnail clear incomplete");
            }
            count
        })
        .await
        .unwrap_or(0)
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
                keys_by_hash: HashMap::new(),
                last_full_clean: None,
            });
        }
        self.full_clean().await;
    }

    /// One pass over every key record: drop those whose blob is gone, miss
    /// markers past their time, and unreadable records, and rebuild the
    /// reverse map from the rest.
    async fn full_clean(&self) {
        let live: HashSet<String> = {
            let mut guard = self.lock();
            let Some(index) = guard.as_mut() else {
                return;
            };
            index.last_full_clean = Some(Instant::now());
            index.blobs.keys().cloned().collect()
        };
        let root = self.root.clone();
        let Some((removed, kept)) = blocking(move || {
            let mut removed = 0;
            let mut kept = Vec::new();
            for dir in [KEYS_DIR, AUDIODB_KEYS_DIR] {
                let (gone, held) = clean_key_files(&root.join(dir), &live, unix_now());
                removed += gone;
                kept.extend(held);
            }
            (removed, kept)
        })
        .await
        else {
            return;
        };
        if let Some(index) = self.lock().as_mut() {
            for (hash, path) in kept {
                index.keys_by_hash.entry(hash).or_default().insert(path);
            }
        }
        if removed > 0 {
            tracing::debug!(removed, "cover cache dropped stale key records");
        }
    }

    /// Delete least recently used blobs until the cache fits, never the one
    /// just written.
    async fn evict(&self, keep: &str) {
        let victims = {
            let mut guard = self.lock();
            let Some(index) = guard.as_mut() else {
                return;
            };
            let mut order: Vec<(u64, String, u64)> = index
                .blobs
                .iter()
                .filter(|(hash, _)| hash.as_str() != keep)
                .map(|(hash, entry)| (entry.last_used, hash.clone(), entry.size))
                .collect();
            order.sort();
            let mut victims = Vec::new();
            for (_, hash, size) in order {
                if index.total <= self.max_bytes {
                    break;
                }
                index.blobs.remove(&hash);
                index.total = index.total.saturating_sub(size);
                let keys = index.keys_by_hash.remove(&hash).unwrap_or_default();
                victims.push((self.blob_path(&hash), hash, keys));
            }
            victims
        };
        if victims.is_empty() {
            return;
        }
        let count = victims.len();
        blocking(move || {
            for (blob, hash, keys) in victims {
                if let Err(error) = std::fs::remove_file(&blob)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, path = %blob.display(), "cover cache eviction failed");
                }
                // Only records still pointing at this blob go; a key may
                // have moved on to newer art since.
                for key in keys {
                    let points_here = std::fs::read_to_string(&key)
                        .ok()
                        .and_then(|text| parse_key(&text))
                        .is_some_and(|entry| {
                            matches!(entry, KeyEntry::Hit { hash: pointed, .. } if pointed == hash)
                        });
                    if points_here {
                        let _ = std::fs::remove_file(&key);
                    }
                }
            }
        })
        .await;
        tracing::debug!(count, "cover cache evicted old images");
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        let shard = hash.get(..2).unwrap_or("00");
        self.root.join("blobs").join(shard).join(hash)
    }

    fn key_path(&self, key: &str) -> PathBuf {
        let name = sha256_hex(key.as_bytes());
        let shard = name.get(..2).unwrap_or("00").to_owned();
        let dir = if key.starts_with(AUDIODB_KEY_PREFIX) {
            AUDIODB_KEYS_DIR
        } else {
            KEYS_DIR
        };
        self.root.join(dir).join(shard).join(name)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Index>> {
        self.index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Run blocking file work off the async workers. `None` if the task
/// panicked, which is logged.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::error!(%error, "cover cache file task failed");
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
        _ => None,
    }
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

/// Delete key records that point at a blob not in `live`, expired miss
/// markers, and records that do not parse. Returns how many went, and the
/// (hash, path) of every record kept that points at a blob.
fn clean_key_files(
    dir: &Path,
    live: &HashSet<String>,
    now: u64,
) -> (usize, Vec<(String, PathBuf)>) {
    let mut removed = 0;
    let mut kept = Vec::new();
    let Ok(shards) = std::fs::read_dir(dir) else {
        return (0, kept);
    };
    for shard in shards.flatten() {
        let Ok(files) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if file.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let stale = match std::fs::read_to_string(&path)
                .ok()
                .as_deref()
                .map(parse_key)
            {
                Some(Some(KeyEntry::Hit { hash, .. })) => {
                    if live.contains(&hash) {
                        kept.push((hash, path.clone()));
                        false
                    } else {
                        true
                    }
                }
                Some(Some(KeyEntry::Miss { until })) => until <= now,
                Some(None) => true,
                None => false,
            };
            if stale && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    (removed, kept)
}

/// Files under `dir`, one shard level deep.
fn count_files(dir: &Path) -> u64 {
    let Ok(shards) = std::fs::read_dir(dir) else {
        return 0;
    };
    shards
        .flatten()
        .filter_map(|shard| std::fs::read_dir(shard.path()).ok())
        .map(|files| files.flatten().count() as u64)
        .sum()
}

/// Every blob on disk with its size; older files count as less recently
/// used, so a restart keeps a sensible eviction order.
fn scan_blobs(dir: &Path) -> HashMap<String, BlobUse> {
    let mut found: Vec<(SystemTime, String, u64)> = Vec::new();
    let Ok(shards) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    for shard in shards.flatten() {
        let Ok(files) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !is_hash(&name) {
                continue;
            }
            if let Ok(meta) = file.metadata() {
                let modified = meta.modified().unwrap_or(UNIX_EPOCH);
                found.push((modified, name, meta.len()));
            }
        }
    }
    found.sort();
    found
        .into_iter()
        .enumerate()
        .map(|(order, (_, hash, size))| {
            (
                hash,
                BlobUse {
                    size,
                    last_used: order as u64,
                },
            )
        })
        .collect()
}
