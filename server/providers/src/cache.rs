//! Cache-aside helpers with registered key prefixes.
//!
//! Provider reads go through [`cache_aside_json`]: a hit returns the cached
//! value, a miss runs the fetch closure and stores its result. Keys live
//! under per-source prefixes, and every source registers its invalidation
//! roots in [`PROVIDER_CACHE_PREFIXES`] so a source change (endpoint swap,
//! credential rotation, settings edit) sweeps exactly that source's keys via
//! [`invalidate_source`].
//!
//! A client may write under a narrower prefix than its registered root (a
//! sweep of `lfm_` covers `lfm_management:...`), as long as the prefix
//! starts under one of its roots.

use std::{collections::HashMap, time::Duration};

use futures_util::future::BoxFuture;
use sha2::{Digest as _, Sha256};

/// Byte cache behind the provider clients. Object-safe so [`Providers`](super::Providers)
/// holds one as `Arc<dyn ProviderCache>`; the boxed futures keep it that way
/// without an async-trait dependency.
pub trait ProviderCache: Send + Sync + std::fmt::Debug {
    /// Fetch one entry, or `None` on miss or expiry.
    fn get_bytes<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>>;
    /// Store one entry for `ttl`.
    fn set_bytes<'a>(&'a self, key: &'a str, value: Vec<u8>, ttl: Duration) -> BoxFuture<'a, ()>;
    /// Drop one entry.
    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()>;
    /// Drop every entry under `prefix`, returning how many went.
    fn clear_prefix<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, usize>;
}

/// Read-through fetch for JSON-serializable values. Decode failures behave
/// as misses (the corrupt entry is replaced by the fresh fetch); fetch
/// failures return the error and cache nothing.
pub async fn cache_aside_json<T, E, F, Fut>(
    cache: &dyn ProviderCache,
    key: &str,
    ttl: Duration,
    fetch: F,
) -> Result<T, E>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    if let Some(hit) = cache.get_bytes(key).await
        && let Ok(decoded) = serde_json::from_slice::<T>(&hit)
    {
        return Ok(decoded);
    }
    let fresh = fetch().await?;
    if let Ok(bytes) = serde_json::to_vec(&fresh) {
        cache.set_bytes(key, bytes, ttl).await;
    }
    Ok(fresh)
}

/// Most entries the in-process cache holds. Past this, a write first drops
/// expired entries and then the entry closest to expiry, so search and page
/// caches keyed by user text can never grow without bound.
pub const MAX_MEMORY_ENTRIES: usize = 20_000;

/// In-process TTL cache for tests and single-node deployments, bounded at
/// [`MAX_MEMORY_ENTRIES`].
#[derive(Debug, Default)]
pub struct InMemoryProviderCache {
    entries: tokio::sync::Mutex<HashMap<String, CacheEntry>>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    value: Vec<u8>,
    expires: tokio::time::Instant,
}

impl InMemoryProviderCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Entries currently held, including unexpired-but-unread ones.
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Whether the cache holds no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.lock().await.is_empty()
    }
}

impl ProviderCache for InMemoryProviderCache {
    fn get_bytes<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let mut entries = self.entries.lock().await;
            let entry = entries.get(key)?.clone();
            if entry.expires <= tokio::time::Instant::now() {
                entries.remove(key);
                return None;
            }
            Some(entry.value)
        })
    }

    fn set_bytes<'a>(&'a self, key: &'a str, value: Vec<u8>, ttl: Duration) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut entries = self.entries.lock().await;
            if entries.len() >= MAX_MEMORY_ENTRIES && !entries.contains_key(key) {
                let now = tokio::time::Instant::now();
                entries.retain(|_, entry| entry.expires > now);
                if entries.len() >= MAX_MEMORY_ENTRIES
                    && let Some(oldest) = entries
                        .iter()
                        .min_by_key(|(_, entry)| entry.expires)
                        .map(|(key, _)| key.clone())
                {
                    entries.remove(&oldest);
                }
            }
            entries.insert(
                key.to_owned(),
                CacheEntry {
                    value,
                    expires: tokio::time::Instant::now() + ttl,
                },
            );
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.entries.lock().await.remove(key);
        })
    }

    fn clear_prefix<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, usize> {
        Box::pin(async move {
            let mut entries = self.entries.lock().await;
            let before = entries.len();
            entries.retain(|key, _| !key.starts_with(prefix));
            before - entries.len()
        })
    }
}

/// One source's invalidation roots: sweeping these prefixes drops everything
/// the source may have cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePrefixes {
    /// Lowercase provider key.
    pub source: &'static str,
    /// Prefix roots; every key the source writes starts with one of these.
    pub roots: &'static [&'static str],
}

/// Registered invalidation roots for every provider. The MusicBrainz list
/// includes the composite responses built from MusicBrainz data, which
/// must clear with it.
pub const PROVIDER_CACHE_PREFIXES: &[SourcePrefixes] = &[
    SourcePrefixes {
        source: "musicbrainz",
        roots: &[
            "mb:artist:search:",
            "mb:artist:detail:",
            "mb:album:search:",
            "mb:rg:detail:",
            "mb:release:detail:",
            "mb:release_to_rg:",
            "mb:recording:",
            "mb:recording:search:",
            "mb:recording_to_rg:",
            "mb:artist_rels:",
            "mb:artist_rgs:",
            "mb_artists_by_tag:",
            "mb_rg_by_tag:",
            "mb:url:resolution:",
            "mb:release:verify:",
            "mb:release:duplicate-search:",
            "mb:release:edition-search:",
            "mb:management:release:",
            "mb:redirect:",
            "mb:isrc:",
            "artist_info:",
            "home_response:",
            "discover_response:",
            "album_info:",
            "album_tracks_info:",
            "discover_queue_enrich:",
            "artist_discovery:top_songs:",
            "artist_discovery:top_albums:",
            "getit:",
        ],
    },
    SourcePrefixes {
        source: "listenbrainz",
        roots: &["lb_"],
    },
    SourcePrefixes {
        source: "lastfm",
        roots: &["lfm_"],
    },
    SourcePrefixes {
        source: "audiodb",
        roots: &["audiodb_"],
    },
    SourcePrefixes {
        source: "acoustid",
        roots: &["acoustid:"],
    },
    SourcePrefixes {
        source: "coverartarchive",
        roots: &["caa:management:"],
    },
];

/// One source's invalidation roots, or an empty list for unknown sources.
#[must_use]
pub fn prefixes_for(source: &str) -> &'static [&'static str] {
    PROVIDER_CACHE_PREFIXES
        .iter()
        .find(|entry| entry.source == source)
        .map_or(&[], |entry| entry.roots)
}

/// Sweep every registered root for `source`, returning entries dropped.
/// Unknown sources sweep nothing.
pub async fn invalidate_source(cache: &dyn ProviderCache, source: &str) -> usize {
    let mut dropped = 0;
    for root in prefixes_for(source) {
        dropped += cache.clear_prefix(root).await;
    }
    dropped
}

/// Build `{prefix}{sha256(parts):x}:{suffix}` for free-text lookups, where
/// raw text would make keys unbounded. Parts are whitespace-collapsed and
/// lowercased before hashing.
#[must_use]
pub fn digest_key(prefix: &str, parts: &[&str], suffix: &str) -> String {
    let mut hasher = Sha256::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hasher.update(b"\x00");
        }
        let collapsed = part.split_whitespace().collect::<Vec<_>>().join(" ");
        hasher.update(collapsed.to_lowercase().as_bytes());
    }
    format!("{prefix}{:x}:{suffix}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache root a settings section names has cached reads to drop, so a
    /// save never invalidates nothing by a misspelt name.
    #[test]
    fn every_settings_cache_root_has_prefixes() {
        use droppedneedle_kernel::runtime_config::effects::CacheRoot;

        for root in CacheRoot::ALL {
            assert!(!prefixes_for(root.as_str()).is_empty(), "{root:?}");
        }
    }

    #[tokio::test]
    async fn json_aside_replaces_corrupt_entries() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Genres {
            names: Vec<String>,
        }
        let cache = InMemoryProviderCache::new();
        cache
            .set_bytes("lfm_x", b"{oops".to_vec(), Duration::from_secs(60))
            .await;
        let value = cache_aside_json::<Genres, String, _, _>(
            &cache,
            "lfm_x",
            Duration::from_secs(60),
            || async {
                Ok(Genres {
                    names: vec!["rock".to_owned()],
                })
            },
        )
        .await
        .expect("fetch succeeds");
        assert_eq!(value.names, ["rock"]);
        // The corrupt entry is now a decodable one.
        let again = cache_aside_json::<Genres, String, _, _>(
            &cache,
            "lfm_x",
            Duration::from_secs(60),
            || async { Err("must-not-run".to_owned()) },
        )
        .await
        .expect("hit decodes");
        assert_eq!(again.names, ["rock"]);
    }

    #[tokio::test]
    async fn fetch_failures_cache_nothing() {
        let cache = InMemoryProviderCache::new();
        let outcome = cache_aside_json::<String, &str, _, _>(
            &cache,
            "audiodb_x",
            Duration::from_secs(60),
            || async { Err("down") },
        )
        .await;
        assert_eq!(outcome, Err("down"));
        assert!(cache.is_empty().await);
    }

    #[tokio::test]
    async fn invalidate_source_sweeps_only_that_source() {
        let cache = InMemoryProviderCache::new();
        cache
            .set_bytes("lfm_artist:x", vec![1], Duration::from_secs(60))
            .await;
        cache
            .set_bytes(
                "lfm_management:genres:abc",
                vec![2],
                Duration::from_secs(60),
            )
            .await;
        cache
            .set_bytes("mb:rg:detail:x", vec![3], Duration::from_secs(60))
            .await;
        assert_eq!(invalidate_source(&cache, "lastfm").await, 2);
        assert_eq!(cache.get_bytes("mb:rg:detail:x").await, Some(vec![3]));
        assert_eq!(invalidate_source(&cache, "unknown").await, 0);
    }

    #[test]
    fn digest_keys_canonicalize() {
        let left = digest_key("lrclib:exact:", &[" Hello  World ", "Artist"], "180");
        let right = digest_key("lrclib:exact:", &["hello world", "artist"], "180");
        assert_eq!(left, right);
        assert!(left.starts_with("lrclib:exact:"));
        assert!(left.ends_with(":180"));
    }

    #[tokio::test]
    async fn expired_entries_read_as_misses() {
        let cache = InMemoryProviderCache::new();
        cache
            .set_bytes("k", vec![1], Duration::from_millis(1))
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(cache.get_bytes("k").await, None);
        assert_eq!(cache.len().await, 0);
    }
}
