//! Cover art: local files first, then the Cover Art Archive, through a
//! bounded disk cache.
//!
//! Resolution follows Navidrome and v2:
//!
//! 1. With `prefer_local_cover_art` on (the default), the album's own art
//!    wins: the folder image or embedded picture the library scan recorded
//!    (see [`local`]).
//! 2. Otherwise, or when there is none, the Cover Art Archive front cover at
//!    the requested size. The archive renders 250, 500 and 1200 pixel
//!    thumbnails itself, so nothing is resized here.
//! 3. With the preference off, local art is the fallback when the archive
//!    has nothing.
//!
//! Local art larger than the asked size (250, 500 or 1200) is scaled down
//! once (see [`resize`]) and the rendition cached next to the original.
//!
//! Every image lands in the content-addressed [`cache`], so a cover is
//! fetched from the archive once and served from disk afterwards, and a
//! "no art" answer is remembered for a while instead of asked again.
//! Concurrent requests for the same cover share one fetch. A fetch that
//! outlasts [`WARM_WAIT`] keeps running in the background while the web
//! route answers 202 ("warming"), which the frontend polls.
//!
//! Artist images have no source wired yet and read as missing.

pub mod cache;
pub mod local;
pub mod remote;
pub mod resize;

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use crate::providers::Singleflight;
use crate::providers::coverart::{DownloadSize, EntityKind, sniff_image_content_type};
use crate::reads::platform::covers::{CoverArt, CoverBytes, CoverLookup};

use self::cache::{ArtworkCache, KeyEntry, unix_now};
use self::local::{LocalArt, LocalArtwork, read_local_art};
use self::remote::RemoteCovers;

/// Source label for Cover Art Archive bytes; the routes give these the
/// short cache window, as v2 did.
pub const CAA_SOURCE: &str = "cover-art-archive";
/// How long the web routes wait for an archive fetch before answering 202.
pub const WARM_WAIT: Duration = Duration::from_secs(4);
/// How long compat clients (which cannot poll) wait for an archive fetch.
pub const COMPAT_WAIT: Duration = Duration::from_secs(20);
/// The archive said it has no cover: ask again after four hours (v2
/// `COVER_NEGATIVE_TTL_SECONDS`).
pub const MISS_TTL_SECS: u64 = 4 * 3600;
/// The archive could not be reached: ask again after 15 minutes (v2
/// `COVER_TRANSIENT_NEGATIVE_TTL_SECONDS`).
pub const OUTAGE_TTL_SECS: u64 = 900;
/// Largest archive image accepted (v2 `MAX_DELIVERY_IMAGE_BYTES`).
pub const MAX_REMOTE_BYTES: usize = 20 * 1024 * 1024;

/// Reads `prefer_local_cover_art` per call, so a settings change applies
/// to the next request.
pub type PreferLocal = Arc<dyn Fn() -> bool + Send + Sync>;

/// The production [`CoverArt`].
#[derive(Clone)]
pub struct ArtworkService {
    cache: ArtworkCache,
    local: LocalArtwork,
    remote: Option<Arc<dyn RemoteCovers>>,
    prefer_local: PreferLocal,
    flights: Singleflight<Option<CoverBytes>>,
}

impl ArtworkService {
    /// Wire the service. `remote` is `None` only where no network source
    /// should be used (tests of the local path).
    pub fn new(
        cache: ArtworkCache,
        local: LocalArtwork,
        remote: Option<Arc<dyn RemoteCovers>>,
        prefer_local: PreferLocal,
    ) -> Self {
        Self {
            cache,
            local,
            remote,
            prefer_local,
            flights: Singleflight::new(),
        }
    }

    /// Release or release-group cover by MBID.
    async fn mbid_cover(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: Option<&str>,
        wait: Duration,
    ) -> CoverLookup {
        let mbid = mbid.trim().to_ascii_lowercase();
        if !crate::providers::coverart::is_valid_mbid(&mbid) {
            return CoverLookup::Missing;
        }
        let prefer_local = (self.prefer_local)();
        if prefer_local && let Some(cover) = self.local_for(entity, &mbid, size).await {
            return CoverLookup::Found(cover);
        }
        match self.remote_cover(entity, &mbid, size, wait).await {
            CoverLookup::Missing => {}
            other => return other,
        }
        if !prefer_local && let Some(cover) = self.local_for(entity, &mbid, size).await {
            return CoverLookup::Found(cover);
        }
        CoverLookup::Missing
    }

    async fn local_for(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: Option<&str>,
    ) -> Option<CoverBytes> {
        let found = match entity {
            EntityKind::ReleaseGroup => self.local.by_release_group(mbid).await,
            EntityKind::Release => self.local.by_release(mbid).await,
        };
        match found {
            Ok(Some(art)) => self.serve_local(&art, size).await,
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "local art lookup failed; trying other sources");
                None
            }
        }
    }

    /// A scan-recorded art row at `size` (`None` is full size): the
    /// original from the cache or the file, scaled down once when it is
    /// larger than the asked size.
    async fn serve_local(&self, art: &LocalArt, size: Option<&str>) -> Option<CoverBytes> {
        let original = self.local_original(art).await?;
        let Some(max) = size.and_then(|size| size.parse::<u32>().ok()) else {
            return Some(original);
        };
        let key = format!("rendition:{}:{max}", original.hash);
        if let Some(KeyEntry::Hit { hash, content_type }) = self.cache.key(&key).await
            && let Some(bytes) = self.cache.blob(&hash).await
        {
            return Some(CoverBytes {
                bytes,
                content_type,
                hash,
                ..original
            });
        }
        let source = original.bytes.clone();
        let shrunk = tokio::task::spawn_blocking(move || resize::shrink_to_fit(&source, max))
            .await
            .unwrap_or_else(|error| {
                tracing::error!(%error, "local art resize task failed");
                None
            });
        match shrunk {
            Some((bytes, content_type)) => {
                let hash = self
                    .cache
                    .put(Some(&key), bytes.clone(), content_type)
                    .await;
                Some(CoverBytes {
                    bytes,
                    content_type: content_type.to_owned(),
                    hash,
                    ..original
                })
            }
            None => {
                // Already small enough: remember that, so it is not decoded
                // again.
                self.cache
                    .put(Some(&key), original.bytes.clone(), &original.content_type)
                    .await;
                Some(original)
            }
        }
    }

    /// The original bytes of a scan-recorded art row: from the cache when
    /// the stored hash is there, else from the file (then cached).
    async fn local_original(&self, art: &LocalArt) -> Option<CoverBytes> {
        if let Some(hash) = &art.content_hash
            && let Some(bytes) = self.cache.blob(hash).await
            && let Some(content_type) = sniff_image_content_type(&bytes)
        {
            return Some(CoverBytes {
                bytes,
                content_type: content_type.to_owned(),
                source: art.source.clone(),
                hash: hash.clone(),
                version: Some(art.version),
            });
        }
        let (bytes, content_type) = read_local_art(art).await?;
        let hash = self.cache.put(None, bytes.clone(), content_type).await;
        Some(CoverBytes {
            bytes,
            content_type: content_type.to_owned(),
            source: art.source.clone(),
            hash,
            version: Some(art.version),
        })
    }

    /// Archive cover through the cache, coalesced, waiting up to `wait`.
    async fn remote_cover(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: Option<&str>,
        wait: Duration,
    ) -> CoverLookup {
        let Some(remote) = self.remote.clone() else {
            return CoverLookup::Missing;
        };
        let size = download_size(size);
        let key = format!("caa:{}:{mbid}:{}", entity.path(), size.label());
        match self.cache.key(&key).await {
            Some(KeyEntry::Hit { hash, content_type }) => {
                if let Some(bytes) = self.cache.blob(&hash).await {
                    return CoverLookup::Found(CoverBytes {
                        bytes,
                        content_type,
                        source: CAA_SOURCE.to_owned(),
                        hash,
                        version: None,
                    });
                }
            }
            Some(KeyEntry::Miss { until }) if until > unix_now() => return CoverLookup::Missing,
            _ => {}
        }
        let flights = self.flights.clone();
        let cache = self.cache.clone();
        let mbid = mbid.to_owned();
        // Spawned so a fetch the caller stops waiting for still lands in
        // the cache for the next request.
        let flight = tokio::spawn(async move {
            let flight_key = key.clone();
            flights
                .run(&flight_key, move || async move {
                    Ok(fetch_and_store(remote.as_ref(), &cache, entity, &mbid, size, &key).await)
                })
                .await
        });
        match tokio::time::timeout(wait, flight).await {
            Ok(Ok(Ok(found))) => match found.as_ref() {
                Some(cover) => CoverLookup::Found(cover.clone()),
                None => CoverLookup::Missing,
            },
            Ok(Ok(Err(error))) => {
                tracing::warn!(%error, "cover fetch failed");
                CoverLookup::Missing
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "cover fetch task failed");
                CoverLookup::Missing
            }
            Err(_) => CoverLookup::Warming,
        }
    }
}

/// One archive fetch: store the bytes or a miss marker.
async fn fetch_and_store(
    remote: &dyn RemoteCovers,
    cache: &ArtworkCache,
    entity: EntityKind,
    mbid: &str,
    size: DownloadSize,
    key: &str,
) -> Option<CoverBytes> {
    use crate::providers::coverart::CaaError;
    match remote.front(entity, mbid, size).await {
        Ok(Some(art)) => {
            let hash = cache
                .put(Some(key), art.bytes.clone(), &art.content_type)
                .await;
            Some(CoverBytes {
                bytes: art.bytes,
                content_type: art.content_type,
                source: CAA_SOURCE.to_owned(),
                hash,
                version: None,
            })
        }
        Ok(None) => {
            cache.put_miss(key, MISS_TTL_SECS).await;
            None
        }
        Err(error) => {
            let ttl = match error {
                CaaError::Unavailable(_) | CaaError::RateLimited { .. } => OUTAGE_TTL_SECS,
                _ => MISS_TTL_SECS,
            };
            tracing::warn!(%error, entity = entity.path(), mbid, "cover art archive fetch failed");
            cache.put_miss(key, ttl).await;
            None
        }
    }
}

/// Map a validated size (`250`, `500`, `1200`, or `None` for full size)
/// onto the archive's renditions.
fn download_size(size: Option<&str>) -> DownloadSize {
    match size {
        Some("250") => DownloadSize::Size250,
        Some("500") => DownloadSize::Size500,
        Some("1200") => DownloadSize::Size1200,
        _ => DownloadSize::Full,
    }
}

impl CoverArt for ArtworkService {
    fn release_group_cover<'a>(
        &'a self,
        release_group_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(self.mbid_cover(EntityKind::ReleaseGroup, release_group_id, size, WARM_WAIT))
    }

    fn release_cover<'a>(
        &'a self,
        release_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(self.mbid_cover(EntityKind::Release, release_id, size, WARM_WAIT))
    }

    fn artist_image<'a>(
        &'a self,
        _artist_id: &'a str,
        _size_px: Option<u32>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(async { CoverLookup::Missing })
    }

    fn album_cover<'a>(
        &'a self,
        album_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        Box::pin(async move {
            let context = match self.local.album(album_id).await {
                Ok(Some(context)) => context,
                Ok(None) => return None,
                Err(error) => {
                    tracing::warn!(%error, "album art lookup failed");
                    return None;
                }
            };
            let prefer_local = (self.prefer_local)();
            if prefer_local
                && let Some(art) = &context.local
                && let Some(cover) = self.serve_local(art, size).await
            {
                return Some(cover);
            }
            if let Some(group) = &context.release_group_mbid {
                let group = group.trim().to_ascii_lowercase();
                if crate::providers::coverart::is_valid_mbid(&group)
                    && let CoverLookup::Found(cover) = self
                        .remote_cover(EntityKind::ReleaseGroup, &group, size, COMPAT_WAIT)
                        .await
                {
                    return Some(cover);
                }
            }
            match &context.local {
                Some(art) if !prefer_local => self.serve_local(art, size).await,
                _ => None,
            }
        })
    }

    fn local_album_art<'a>(
        &'a self,
        album_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        Box::pin(async move {
            match self.local.album(album_id).await {
                Ok(Some(context)) => match &context.local {
                    Some(art) => self.serve_local(art, size).await,
                    None => None,
                },
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(%error, "album art lookup failed");
                    None
                }
            }
        })
    }
}
