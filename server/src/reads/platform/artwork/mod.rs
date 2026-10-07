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
//! Artist images come from TheAudioDB's artist thumbnail, else the
//! Wikidata portrait (v2's order, minus the Lidarr and Jellyfin sources v3
//! does not mirror). The catalog names the candidate URLs through
//! [`ArtistImageSource`]; the image itself is downloaded once, from an
//! allowed host only (see [`remote::check_image_url`]), stored full size,
//! and scaled down per requested size like local art. A grid of artists
//! never waits on the lookups: past [`WARM_WAIT`] the route answers 202 and
//! the resolve finishes in the background, at most
//! [`ARTIST_RESOLVE_PERMITS`] at a time.
//!
//! TheAudioDB album thumbnails, fetched by the library precache, stand in
//! for a release group's cover when neither the album's own art nor the
//! archive has one.

pub mod cache;
pub mod local;
pub mod remote;
pub mod resize;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::future::BoxFuture;

use crate::providers::Singleflight;
use crate::providers::coverart::{
    ArtworkBytes, DownloadSize, EntityKind, sniff_image_content_type,
};
use crate::reads::platform::covers::{CoverArt, CoverBytes, CoverLookup};

use self::cache::{ArtworkCache, KeyEntry, unix_now};
use self::local::{LocalArt, LocalArtwork, read_local_art};
use self::remote::{ImageFetch, RemoteCovers, RemoteImages};

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
/// Local art resizes decoding at once; each can hold a large picture in
/// memory.
pub const RESIZE_PERMITS: usize = 3;
/// Largest archive image accepted (v2 `MAX_DELIVERY_IMAGE_BYTES`).
pub const MAX_REMOTE_BYTES: usize = 20 * 1024 * 1024;
/// Source label for artist images.
pub const ARTIST_SOURCE: &str = "artist-image";
/// Source label for TheAudioDB album thumbnails served as covers.
pub const AUDIODB_SOURCE: &str = "audiodb";
/// How long a precache run waits for one image before moving on (the
/// fetch keeps going in the background).
pub const PRECACHE_WAIT: Duration = Duration::from_secs(60);
/// Artist images resolved at once. Each resolve asks MusicBrainz and
/// TheAudioDB, which are paced, so more would only queue.
pub const ARTIST_RESOLVE_PERMITS: usize = 4;

/// Candidate artist image URLs, best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistImageCandidates {
    /// URLs to try in order: TheAudioDB thumbnail, then the Wikidata
    /// portrait.
    pub urls: Vec<String>,
    /// A source could not answer, so "no image" is not settled yet and is
    /// asked again sooner.
    pub incomplete: bool,
}

/// Names the image URLs an artist has. The catalog implements it; lookups
/// it makes are cached and paced there.
pub trait ArtistImageSource: Send + Sync {
    /// Candidate URLs for one artist (lowercase MBID).
    fn candidates<'a>(&'a self, artist_mbid: &'a str) -> BoxFuture<'a, ArtistImageCandidates>;
}

/// The slot the catalog fills once it exists (the artwork service is built
/// first).
pub type ArtistSourceSlot = Arc<OnceLock<Arc<dyn ArtistImageSource>>>;

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
    resizes: Singleflight<Option<Rendition>>,
    resize_permits: Arc<tokio::sync::Semaphore>,
    images: Option<Arc<dyn RemoteImages>>,
    artist_source: ArtistSourceSlot,
    artist_flights: Singleflight<Option<CoverBytes>>,
    artist_permits: Arc<tokio::sync::Semaphore>,
}

/// A scaled-down copy of local art.
#[derive(Debug, Clone)]
struct Rendition {
    bytes: Vec<u8>,
    content_type: String,
    hash: String,
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
            resizes: Singleflight::new(),
            resize_permits: Arc::new(tokio::sync::Semaphore::new(RESIZE_PERMITS)),
            images: None,
            artist_source: Arc::new(OnceLock::new()),
            artist_flights: Singleflight::new(),
            artist_permits: Arc::new(tokio::sync::Semaphore::new(ARTIST_RESOLVE_PERMITS)),
        }
    }

    /// Download artist images and AudioDB thumbnails through `images`.
    #[must_use]
    pub fn with_images(mut self, images: Arc<dyn RemoteImages>) -> Self {
        self.images = Some(images);
        self
    }

    /// The slot the artist image source goes into once the catalog exists.
    pub fn artist_source_slot(&self) -> ArtistSourceSlot {
        Arc::clone(&self.artist_source)
    }

    /// The disk cache, for the admin stats and clears.
    pub fn cache(&self) -> &ArtworkCache {
        &self.cache
    }

    /// Whether the artist's image question is settled in the cache: an
    /// image, or a "none" that has not expired.
    pub async fn artist_image_cached(&self, artist_mbid: &str) -> bool {
        self.settled(&artist_key(artist_mbid)).await
    }

    /// Resolve and cache the artist's image now (the precache), waiting up
    /// to [`PRECACHE_WAIT`]. True when an image is cached.
    pub async fn warm_artist_image(&self, artist_mbid: &str) -> bool {
        matches!(
            self.artist_original(artist_mbid, PRECACHE_WAIT).await,
            CoverLookup::Found(_)
        )
    }

    /// Whether a release group's 500 px cover is settled: the album's own
    /// art (when preferred), or an archive answer that has not expired.
    pub async fn release_group_cover_cached(&self, mbid: &str) -> bool {
        let mbid = mbid.trim().to_ascii_lowercase();
        if (self.prefer_local)() && matches!(self.local.by_release_group(&mbid).await, Ok(Some(_)))
        {
            return true;
        }
        self.settled(&format!(
            "caa:{}:{mbid}:500",
            EntityKind::ReleaseGroup.path()
        ))
        .await
    }

    /// Fetch and cache a release group's 500 px cover now (the precache).
    pub async fn warm_release_group_cover(&self, mbid: &str) -> bool {
        matches!(
            self.mbid_cover(EntityKind::ReleaseGroup, mbid, Some("500"), PRECACHE_WAIT)
                .await,
            CoverLookup::Found(_)
        )
    }

    /// Whether TheAudioDB's thumbnail for a release group is settled.
    pub async fn audiodb_cover_cached(&self, mbid: &str) -> bool {
        self.settled(&audiodb_cover_key(mbid)).await
    }

    /// Download and keep TheAudioDB's thumbnail for a release group, which
    /// stands in when the album has no other cover (v2's AudioDB prewarm).
    /// `None` records that AudioDB has no thumbnail.
    pub async fn store_audiodb_cover(&self, mbid: &str, url: Option<&str>) {
        let key = audiodb_cover_key(mbid);
        let Some(url) = url else {
            self.cache.put_miss(&key, MISS_TTL_SECS).await;
            return;
        };
        let Some(images) = &self.images else {
            return;
        };
        match images.fetch(url).await {
            ImageFetch::Found(art) => {
                self.cache
                    .put(Some(&key), art.bytes, &art.content_type)
                    .await;
            }
            ImageFetch::Unusable => self.cache.put_miss(&key, MISS_TTL_SECS).await,
            ImageFetch::Failed => self.cache.put_miss(&key, OUTAGE_TTL_SECS).await,
        }
    }

    /// True when `key` holds an image still on disk, or a fresh miss.
    async fn settled(&self, key: &str) -> bool {
        match self.cache.key(key).await {
            Some(KeyEntry::Hit { hash, .. }) => self.cache.has_blob(&hash).await,
            Some(KeyEntry::Miss { until }) => until > unix_now(),
            None => false,
        }
    }

    /// A cached image by key, labelled with `source`.
    async fn cached_image(&self, key: &str, source: &str) -> Option<CoverBytes> {
        let Some(KeyEntry::Hit { hash, content_type }) = self.cache.key(key).await else {
            return None;
        };
        let bytes = self.cache.blob(&hash).await?;
        Some(CoverBytes {
            bytes,
            content_type,
            source: source.to_owned(),
            hash,
            version: None,
        })
    }

    /// The artist's image at full size: from the cache, or resolved in the
    /// background while the caller waits up to `wait`.
    async fn artist_original(&self, mbid: &str, wait: Duration) -> CoverLookup {
        let key = artist_key(mbid);
        match self.cache.key(&key).await {
            Some(KeyEntry::Hit { .. }) => {
                if let Some(image) = self.cached_image(&key, ARTIST_SOURCE).await {
                    return CoverLookup::Found(image);
                }
            }
            Some(KeyEntry::Miss { until }) if until > unix_now() => return CoverLookup::Missing,
            _ => {}
        }
        let (Some(source), Some(images)) = (self.artist_source.get().cloned(), self.images.clone())
        else {
            return CoverLookup::Missing;
        };
        let flights = self.artist_flights.clone();
        let cache = self.cache.clone();
        let permits = Arc::clone(&self.artist_permits);
        let mbid = mbid.to_owned();
        // Spawned so a resolve the caller stops waiting for still lands in
        // the cache for the next request.
        let flight = tokio::spawn(async move {
            let flight_key = key.clone();
            flights
                .run(&flight_key, move || async move {
                    let _permit = permits.acquire_owned().await.ok();
                    Ok(resolve_artist(source.as_ref(), images.as_ref(), &cache, &mbid, &key).await)
                })
                .await
        });
        match tokio::time::timeout(wait, flight).await {
            Ok(Ok(Ok(found))) => match found.as_ref() {
                Some(image) => CoverLookup::Found(image.clone()),
                None => CoverLookup::Missing,
            },
            Ok(Ok(Err(error))) => {
                tracing::warn!(%error, "artist image resolve failed");
                CoverLookup::Missing
            }
            Ok(Err(error)) => {
                tracing::error!(%error, "artist image task failed");
                CoverLookup::Missing
            }
            Err(_) => CoverLookup::Warming,
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
        if entity == EntityKind::ReleaseGroup
            && let Some(cover) = self
                .cached_image(&audiodb_cover_key(&mbid), AUDIODB_SOURCE)
                .await
        {
            return CoverLookup::Found(self.sized(cover, size).await);
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
        Some(self.sized(original, size).await)
    }

    /// `original` scaled down to `size` (`None` is full size), the rendition
    /// cached by the original's hash.
    async fn sized(&self, original: CoverBytes, size: Option<&str>) -> CoverBytes {
        let Some(max) = size.and_then(|size| size.parse::<u32>().ok()) else {
            return original;
        };
        let key = format!("rendition:{}:{max}", original.hash);
        if let Some(KeyEntry::Hit { hash, content_type }) = self.cache.key(&key).await
            && let Some(bytes) = self.cache.blob(&hash).await
        {
            return CoverBytes {
                bytes,
                content_type,
                hash,
                ..original
            };
        }
        // One resize per rendition at a time, and at most a few overall.
        let cache = self.cache.clone();
        let permits = self.resize_permits.clone();
        let source = original.bytes.clone();
        let (original_hash, original_type) = (original.hash.clone(), original.content_type.clone());
        let flight_key = key.clone();
        let rendition = self
            .resizes
            .run(&flight_key, move || async move {
                Ok(resize_and_store(
                    cache,
                    permits,
                    key,
                    source,
                    max,
                    original_hash,
                    original_type,
                )
                .await)
            })
            .await;
        match rendition.as_deref() {
            Ok(Some(rendition)) => CoverBytes {
                bytes: rendition.bytes.clone(),
                content_type: rendition.content_type.clone(),
                hash: rendition.hash.clone(),
                ..original
            },
            Ok(None) | Err(_) => original,
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

/// Scale local art down to `max` under a resize permit and cache the
/// rendition under `key`. Art already small enough is recorded as its own
/// rendition (by its existing hash) so it is not decoded again. A failed
/// resize task caches nothing.
async fn resize_and_store(
    cache: ArtworkCache,
    permits: Arc<tokio::sync::Semaphore>,
    key: String,
    source: Vec<u8>,
    max: u32,
    original_hash: String,
    original_type: String,
) -> Option<Rendition> {
    let _permit = match permits.acquire_owned().await {
        Ok(permit) => permit,
        Err(error) => {
            tracing::error!(%error, "resize permits closed; serving the original");
            return None;
        }
    };
    match tokio::task::spawn_blocking(move || resize::shrink_to_fit(&source, max)).await {
        Ok(Some((bytes, content_type))) => {
            let hash = cache.put(Some(&key), bytes.clone(), content_type).await;
            Some(Rendition {
                bytes,
                content_type: content_type.to_owned(),
                hash,
            })
        }
        Ok(None) => {
            cache.put_key(&key, &original_hash, &original_type).await;
            None
        }
        Err(error) => {
            tracing::error!(%error, "local art resize task failed; serving the original");
            None
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

/// Cache key of an artist's full-size image.
fn artist_key(mbid: &str) -> String {
    format!("artist:{}", mbid.trim().to_ascii_lowercase())
}

/// Cache key of TheAudioDB's thumbnail for a release group.
fn audiodb_cover_key(mbid: &str) -> String {
    format!(
        "{}release-group:{}",
        self::cache::AUDIODB_KEY_PREFIX,
        mbid.trim().to_ascii_lowercase()
    )
}

/// Largest artist image kept: downloads are scaled down to this before
/// they are stored, so artist pictures do not crowd covers out of the
/// size-bounded cache.
pub const ARTIST_STORED_MAX: u32 = 1200;

/// Snap a requested artist image width to the renditions kept (250, 500);
/// anything larger, or no width, is the stored image.
fn artist_size(size_px: Option<u32>) -> Option<&'static str> {
    match size_px? {
        0..=250 => Some("250"),
        251..=500 => Some("500"),
        _ => None,
    }
}

/// An artist image scaled down to [`ARTIST_STORED_MAX`], off the async
/// workers. Images already small enough, or that cannot be decoded, stay
/// as downloaded.
async fn shrink_artist_image(art: ArtworkBytes) -> ArtworkBytes {
    let source = art.bytes.clone();
    match tokio::task::spawn_blocking(move || resize::shrink_to_fit(&source, ARTIST_STORED_MAX))
        .await
    {
        Ok(Some((bytes, content_type))) => ArtworkBytes {
            bytes,
            content_type: content_type.to_owned(),
        },
        Ok(None) => art,
        Err(error) => {
            tracing::error!(%error, "artist image resize task failed; keeping the download");
            art
        }
    }
}

/// One artist image resolve: the first candidate that downloads is
/// cached; with none, a miss marker (short when a source was down).
async fn resolve_artist(
    source: &dyn ArtistImageSource,
    images: &dyn RemoteImages,
    cache: &ArtworkCache,
    mbid: &str,
    key: &str,
) -> Option<CoverBytes> {
    let candidates = source.candidates(mbid).await;
    let mut failed = candidates.incomplete;
    for url in &candidates.urls {
        match images.fetch(url).await {
            ImageFetch::Found(art) => {
                let art = shrink_artist_image(art).await;
                let hash = cache
                    .put(Some(key), art.bytes.clone(), &art.content_type)
                    .await;
                return Some(CoverBytes {
                    bytes: art.bytes,
                    content_type: art.content_type,
                    source: ARTIST_SOURCE.to_owned(),
                    hash,
                    version: None,
                });
            }
            ImageFetch::Unusable => {}
            ImageFetch::Failed => failed = true,
        }
    }
    let ttl = if failed {
        OUTAGE_TTL_SECS
    } else {
        MISS_TTL_SECS
    };
    cache.put_miss(key, ttl).await;
    None
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
        artist_id: &'a str,
        size_px: Option<u32>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(async move {
            let mbid = artist_id.trim().to_ascii_lowercase();
            if !crate::providers::coverart::is_valid_mbid(&mbid) {
                return CoverLookup::Missing;
            }
            match self.artist_original(&mbid, WARM_WAIT).await {
                CoverLookup::Found(original) => {
                    CoverLookup::Found(self.sized(original, artist_size(size_px)).await)
                }
                other => other,
            }
        })
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
