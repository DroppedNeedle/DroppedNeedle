//! Cache-aside helpers with registered key prefixes.
//!
//! Provider clients read through [`cache_aside`]: a hit returns the cached
//! bytes, a miss runs the fetch closure and stores its result. Keys live
//! under per-source prefixes, and every source registers its invalidation
//! roots in [`PROVIDER_CACHE_PREFIXES`] so a source change (endpoint swap,
//! credential rotation, settings edit) sweeps exactly that source's keys via
//! [`invalidate_source`].
//!
//! The prefix lists port v2's `cache_keys.py` sweep sets. Clients may use
//! narrower prefixes than the registered roots (a sweep of `lfm_` covers
//! `lfm_management:...`); the client contract only requires that every
//! prefix a client writes starts under one of its roots.

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

/// Read-through fetch with an infallible closure (scripted fakes, pure
/// derivations): hit returns cached bytes, miss runs `fetch` and stores its
/// result for `ttl`. Fallible fetches belong in [`cache_aside_json`] or match
/// on the cache directly.
pub async fn cache_aside_bytes<F, Fut>(
    cache: &dyn ProviderCache,
    key: &str,
    ttl: Duration,
    fetch: F,
) -> Vec<u8>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Vec<u8>>,
{
    if let Some(hit) = cache.get_bytes(key).await {
        return hit;
    }
    let value = fetch().await;
    cache.set_bytes(key, value.clone(), ttl).await;
    value
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

    /// Drop expired entries, returning how many went.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn cleanup_expired(&self) -> usize {
        let mut entries = self.entries.lock().await;
        let now = tokio::time::Instant::now();
        let before = entries.len();
        entries.retain(|_, entry| entry.expires > now);
        before - entries.len()
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
/// ports v2's `musicbrainz_prefixes()` sweep set verbatim, including the
/// provider-bearing composite responses that must cold-clear with it.
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

/// Whether `prefix` sits under one of `source`'s registered roots, so a
/// sweep of the roots covers it.
#[must_use]
pub fn prefix_is_registered(source: &str, prefix: &str) -> bool {
    prefixes_for(source)
        .iter()
        .any(|root| prefix.starts_with(root))
}

/// Build `{prefix}{id}` with the id canonicalized the way v2's `_mbid_key`
/// does: trimmed and lowercased (MBIDs are ASCII, so this matches casefold).
#[must_use]
pub fn namespaced_key(prefix: &str, id: &str) -> String {
    format!("{prefix}{}", id.trim().to_lowercase())
}

/// Build `{prefix}{sha256(parts):x}:{suffix}` for free-text lookups, where
/// raw text would make keys unbounded. Parts are whitespace-collapsed and
/// lowercased before hashing, like v2's digest builders.
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

    #[tokio::test]
    async fn aside_serves_miss_then_hit() {
        let cache = InMemoryProviderCache::new();
        let key = "mb:rg:detail:deadbeef:default";
        let miss = cache_aside_bytes(&cache, key, Duration::from_secs(60), || async {
            b"fresh".to_vec()
        })
        .await;
        assert_eq!(miss, b"fresh");
        let hit = cache_aside_bytes(&cache, key, Duration::from_secs(60), || async {
            b"must-not-run".to_vec()
        })
        .await;
        assert_eq!(hit, b"fresh");
        assert_eq!(cache.len().await, 1);
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
    fn narrower_prefixes_stay_covered_by_roots() {
        assert!(prefix_is_registered("lastfm", "lfm_management:album:"));
        assert!(prefix_is_registered(
            "musicbrainz",
            "mb:artist_rgs:deadbeef:page:10:0"
        ));
        assert!(!prefix_is_registered("lastfm", "mb:rg:detail:"));
        assert!(!prefix_is_registered("unknown", "lfm_"));
    }

    #[test]
    fn key_builders_canonicalize() {
        assert_eq!(
            namespaced_key("mb:rg:detail:", "  DEAD-BEEF "),
            "mb:rg:detail:dead-beef"
        );
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
