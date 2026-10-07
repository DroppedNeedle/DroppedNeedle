//! Cache admin: stats and clears.
//!
//! v3 keeps two caches where v2 had a shelf of them: the in-memory
//! provider byte cache (v2's memory and disk metadata caches, AudioDB
//! answers included) and the cover image cache on disk (album covers and
//! artist images). v2's "library" shelf was a mirror of Lidarr's library;
//! v3's library is the scanned database itself, not a cache, so it has no
//! clear. The stats cover live entry counts, the registered invalidation
//! roots, and the image count and size on disk.

use std::sync::Arc;

use super::{
    error::AdminError,
    models::{CacheClearBody, CacheClearResponse, CacheStatsResponse},
};
use crate::providers::{
    InMemoryProviderCache, ProviderCache, cache::PROVIDER_CACHE_PREFIXES, invalidate_source,
    prefixes_for,
};
use crate::reads::platform::artwork::cache::ArtworkCache;

/// Live entry count, the registered sources in registry order, and the
/// images on disk.
pub async fn cache_stats(
    cache: &InMemoryProviderCache,
    covers: Option<&ArtworkCache>,
) -> CacheStatsResponse {
    let (cover_images, cover_bytes) = match covers {
        Some(covers) => covers.usage().await,
        None => (0, 0),
    };
    CacheStatsResponse {
        entries: cache.len().await,
        sources: PROVIDER_CACHE_PREFIXES
            .iter()
            .map(|entry| entry.source.to_owned())
            .collect(),
        cover_images,
        cover_bytes,
    }
}

/// Clear caches: everything by default, one source's registered roots,
/// the images only, or TheAudioDB's answers only. Unknown scopes and
/// sources are rejected (a typo must not read as a successful no-op);
/// clearing an idle source succeeds with zero dropped.
pub async fn clear_cache(
    cache: &Arc<InMemoryProviderCache>,
    covers: Option<&ArtworkCache>,
    body: &CacheClearBody,
) -> Result<CacheClearResponse, AdminError> {
    let scope = body.scope.as_deref().unwrap_or("all");
    let store: &dyn ProviderCache = cache.as_ref();
    let clear_covers = || async {
        match covers {
            Some(covers) => covers.clear().await,
            None => 0,
        }
    };
    let (cleared, cleared_cover_images) = match scope {
        "all" => (store.clear_prefix("").await, clear_covers().await),
        "covers" => (0, clear_covers().await),
        "audiodb" => {
            let thumbnails = match covers {
                Some(covers) => covers.clear_audiodb().await,
                None => 0,
            };
            (invalidate_source(store, "audiodb").await, thumbnails)
        }
        "source" => {
            let source = body.source.as_deref().unwrap_or("").trim();
            if source.is_empty() {
                return Err(AdminError::InvalidInput {
                    message: "A source clear needs a source name".to_owned(),
                });
            }
            if prefixes_for(source).is_empty() {
                return Err(AdminError::InvalidInput {
                    message: format!("Unknown cache source: {source}"),
                });
            }
            (invalidate_source(store, source).await, 0)
        }
        other => {
            return Err(AdminError::InvalidInput {
                message: format!(
                    "Unknown cache scope: {other} (want all, source, covers or audiodb)"
                ),
            });
        }
    };
    let remaining = cache.len().await;
    let entries = if cleared == 1 {
        "1 cache entry".to_owned()
    } else {
        format!("{cleared} cache entries")
    };
    let message = match scope {
        "covers" => format!("Cleared {cleared_cover_images} cached images"),
        "audiodb" => {
            format!("Cleared {entries} and {cleared_cover_images} AudioDB album thumbnails")
        }
        "all" => format!("Cleared {entries} and {cleared_cover_images} cached images"),
        _ => format!("Cleared {entries}"),
    };
    Ok(CacheClearResponse {
        message,
        cleared_entries: cleared,
        remaining_entries: remaining,
        cleared_cover_images,
    })
}
