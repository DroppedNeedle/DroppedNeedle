//! Provider-cache admin: stats and clears.
//!
//! v3 keeps one cache — the in-memory provider byte cache — where v2 had a
//! shelf of them (memory, disk metadata, covers, library rows, AudioDB).
//! The stats here cover what exists: live entry counts plus the registered
//! invalidation roots. Byte totals and hit rates arrive with cache
//! instrumentation, which is outside stage 10; until then the counts and
//! the cleared-per-run numbers are the honest picture.

use std::sync::Arc;

use super::{
    error::AdminError,
    models::{CacheClearBody, CacheClearResponse, CacheStatsResponse},
};
use crate::providers::{
    InMemoryProviderCache, ProviderCache, cache::PROVIDER_CACHE_PREFIXES, invalidate_source,
    prefixes_for,
};

/// Live entry count plus the registered sources, in registry order.
pub async fn cache_stats(cache: &InMemoryProviderCache) -> CacheStatsResponse {
    CacheStatsResponse {
        entries: cache.len().await,
        sources: PROVIDER_CACHE_PREFIXES
            .iter()
            .map(|entry| entry.source.to_owned())
            .collect(),
    }
}

/// Clear the cache: everything by default, or one source's registered
/// roots. Unknown sources are rejected (a typo must not read as a
/// successful no-op); clearing an idle source succeeds with zero dropped.
pub async fn clear_cache(
    cache: &Arc<InMemoryProviderCache>,
    body: &CacheClearBody,
) -> Result<CacheClearResponse, AdminError> {
    let scope = body.scope.as_deref().unwrap_or("all");
    let store: &dyn ProviderCache = cache.as_ref();
    let cleared = match scope {
        "all" => store.clear_prefix("").await,
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
            invalidate_source(store, source).await
        }
        other => {
            return Err(AdminError::InvalidInput {
                message: format!("Unknown cache scope: {other} (want all or source)"),
            });
        }
    };
    let remaining = cache.len().await;
    Ok(CacheClearResponse {
        message: if cleared == 1 {
            "Cleared 1 cache entry".to_owned()
        } else {
            format!("Cleared {cleared} cache entries")
        },
        cleared_entries: cleared,
        remaining_entries: remaining,
    })
}
