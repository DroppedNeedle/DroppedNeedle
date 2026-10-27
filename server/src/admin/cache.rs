//! Cache admin: stats and clears.
//!
//! v3 keeps two caches: provider answers (MusicBrainz, ListenBrainz,
//! Last.fm, TheAudioDB and the rest) in their own cache file, and the cover
//! image cache on disk (album covers and artist images). The stats give
//! rows and bytes per provider and the images on disk; a clear drops
//! provider data, images, or both. Clearing provider data also drops the
//! discover, home and queue pages built from it. Anything cleared is
//! fetched or built again on demand.

use droppedneedle_kernel::db::cache::CacheError;

use super::{
    AdminDb,
    error::AdminError,
    models::{
        CacheClearBody, CacheClearResponse, CacheClearScope, CacheStatsResponse,
        ProviderSourceStats,
    },
};
use droppedneedle_catalog::reads::discover::cache as discover_pages;
use droppedneedle_catalog::reads::platform::artwork::cache::ArtworkCache;
use droppedneedle_providers::ProviderCacheStore;

/// Rows and bytes per provider, whether the cache file is working, and the
/// images on disk.
pub async fn cache_stats(
    cache: &ProviderCacheStore,
    covers: Option<&ArtworkCache>,
) -> CacheStatsResponse {
    let (cover_images, cover_bytes) = match covers {
        Some(covers) => covers.usage().await,
        None => (0, 0),
    };
    let sources: Vec<ProviderSourceStats> = cache
        .stats()
        .await
        .into_iter()
        .map(|stats| ProviderSourceStats {
            source: stats.source,
            rows: stats.rows,
            bytes: stats.bytes,
        })
        .collect();
    CacheStatsResponse {
        entries: sources.iter().map(|source| source.rows).sum(),
        sources,
        provider_data_on: cache.is_on(),
        cover_images,
        cover_bytes,
    }
}

/// Clear provider data, the images, or both (the default). Provider data
/// takes the built discover pages in `db` with it.
pub async fn clear_cache(
    cache: &ProviderCacheStore,
    db: Option<&AdminDb>,
    covers: Option<&ArtworkCache>,
    body: &CacheClearBody,
) -> Result<CacheClearResponse, AdminError> {
    let clear_rows = || async {
        if let Some(db) = db {
            discover_pages::clear_all(&db.db)
                .await
                .map_err(|error| AdminError::internal(&error))?;
        }
        match cache.clear().await {
            Ok(cleared) => Ok(cleared),
            // An off cache holds nothing to clear.
            Err(CacheError::Off) => Ok(0),
            Err(error) => Err(AdminError::internal(&error)),
        }
    };
    let clear_covers = || async {
        match covers {
            Some(covers) => covers.clear().await,
            None => 0,
        }
    };
    let (cleared, cleared_cover_images) = match body.scope {
        CacheClearScope::All => (clear_rows().await?, clear_covers().await),
        CacheClearScope::ProviderData => (clear_rows().await?, 0),
        CacheClearScope::Covers => (0, clear_covers().await),
    };
    let remaining = cache.stats().await.iter().map(|stats| stats.rows).sum();
    let entries = if cleared == 1 {
        "1 provider answer".to_owned()
    } else {
        format!("{cleared} provider answers")
    };
    let message = match body.scope {
        CacheClearScope::Covers => format!("Cleared {cleared_cover_images} cached images"),
        CacheClearScope::ProviderData => format!("Cleared {entries}"),
        CacheClearScope::All => {
            format!("Cleared {entries} and {cleared_cover_images} cached images")
        }
    };
    Ok(CacheClearResponse {
        message,
        cleared_entries: cleared,
        remaining_entries: remaining,
        cleared_cover_images,
    })
}
