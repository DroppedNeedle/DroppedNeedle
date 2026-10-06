//! Content-addressed cover cache on disk, bounded by size.
//!
//! Layout under `<cache_dir>/covers`:
//!
//! - `blobs/<aa>/<hash>`: image bytes, named by the SHA-256 of the bytes,
//!   so the same picture fetched for a release and its release group, or
//!   found in a folder and served through two routes, is stored once.
//! - `keys/<aa>/<key hash>`: a small text record mapping a lookup key (for
//!   example `caa:release-group:<mbid>:500`) to a blob, or recording that
//!   the archive had no art until a given time.
//!
//! When the blobs pass `COVER_CACHE_MAX_SIZE_MB` the least recently used
//! ones are deleted. A key whose blob was evicted reads as a miss, so the
//! art is fetched again. Key records pointing at evicted blobs, and miss
//! markers past their time, are removed when the cache first loads and
//! after each eviction. Every file operation runs on a blocking thread,
//! and writes go through a temporary file and a rename so a crash never
//! leaves a half-written image behind.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
        let key_file =
            key.map(|key| (self.key_path(key), format!("hit\n{hash}\n{content_type}\n")));
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

    /// Record that the source had no art for `key` for `ttl_secs`.
    pub async fn put_miss(&self, key: &str, ttl_secs: u64) {
        let path = self.key_path(key);
        let text = format!("miss\n{}\n", unix_now() + ttl_secs);
        if let Some(Err(error)) = blocking(move || write_atomic(&path, text.as_bytes())).await {
            tracing::warn!(%error, "cover cache miss marker not written");
        }
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
        let live = {
            let mut guard = self.lock();
            if guard.is_some() {
                return;
            }
            let total = scanned.values().map(|entry| entry.size).sum();
            let clock = scanned.len() as u64;
            let live: HashSet<String> = scanned.keys().cloned().collect();
            *guard = Some(Index {
                blobs: scanned,
                total,
                clock,
            });
            live
        };
        self.clean_keys(live).await;
    }

    /// Remove key records whose blob is gone and miss markers past their
    /// time.
    async fn clean_keys(&self, live: HashSet<String>) {
        let keys_dir = self.root.join("keys");
        let removed = blocking(move || clean_key_files(&keys_dir, &live, unix_now()))
            .await
            .unwrap_or(0);
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
                victims.push(self.blob_path(&hash));
            }
            victims
        };
        if victims.is_empty() {
            return;
        }
        let count = victims.len();
        let live: HashSet<String> = self
            .lock()
            .as_ref()
            .map(|index| index.blobs.keys().cloned().collect())
            .unwrap_or_default();
        blocking(move || {
            for path in victims {
                if let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, path = %path.display(), "cover cache eviction failed");
                }
            }
        })
        .await;
        tracing::debug!(count, "cover cache evicted old images");
        self.clean_keys(live).await;
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        let shard = hash.get(..2).unwrap_or("00");
        self.root.join("blobs").join(shard).join(hash)
    }

    fn key_path(&self, key: &str) -> PathBuf {
        let name = sha256_hex(key.as_bytes());
        let shard = name.get(..2).unwrap_or("00").to_owned();
        self.root.join("keys").join(shard).join(name)
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
/// markers, and records that do not parse. Returns how many went.
fn clean_key_files(dir: &Path, live: &HashSet<String>, now: u64) -> usize {
    let mut removed = 0;
    let Ok(shards) = std::fs::read_dir(dir) else {
        return 0;
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
                Some(Some(KeyEntry::Hit { hash, .. })) => !live.contains(&hash),
                Some(Some(KeyEntry::Miss { until })) => until <= now,
                Some(None) => true,
                None => false,
            };
            if stale && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
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
