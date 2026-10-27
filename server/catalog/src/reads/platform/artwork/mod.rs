//! Cover art: local files first, then the Cover Art Archive, through a
//! bounded disk cache.
//!
//! Resolution follows Navidrome and v2:
//!
//! 1. With `prefer_local_cover_art` on (the default), the album's own art
//!    wins: the folder image or embedded picture the library scan recorded
//!    (see [`local`]).
//! 2. Otherwise, or when there is none, the Cover Art Archive front cover at
//!    the requested size. The archive renders 500 and 1200 pixel thumbnails
//!    itself. A 250 is scaled down from the 500, so one archive trip serves
//!    both sizes.
//! 3. With the preference off, local art is the fallback when the archive
//!    has nothing.
//!
//! Local art larger than the asked size (250, 500 or 1200) is scaled down
//! once (see [`resize`]) and only the rendition is cached, under a key that
//! carries the file's path and stat. A grid tile then costs a stat and a
//! small read; the original is opened again only when its stat changes. A
//! full-size request reads the file. Originals stay out of the cache, so
//! they never push downloaded covers out; art already no larger than the
//! asked size is kept as that size's rendition.
//!
//! Every image lands in the content-addressed [`cache`], so a cover is
//! fetched from the archive once and served from disk afterwards, and a
//! "no art" answer is remembered for a while instead of asked again. A
//! fetch that learned nothing (the archive down or refusing, or our own
//! limiter turning it down) is never taken for "no art": the routes answer
//! 202 and the cover is asked for again after [`OUTAGE_TTL_SECS`].
//! Concurrent requests for the same cover share one fetch. A page never
//! joins a background fetch, which queues behind every page at the
//! archive; it starts its own. A fetch that
//! outlasts [`WARM_WAIT`] keeps running in the background while the web
//! route answers 202 ("warming"), which the frontend polls.
//!
//! Artist images come from TheAudioDB's artist thumbnail, else the Deezer
//! picture and then the Commons portrait of the one Wikidata item carrying
//! the artist's MBID, matched by exact id. A page reads TheAudioDB from the
//! cache only; a miss queues a background fill (see [`warm`]) and the page
//! goes on to Wikidata. A Commons portrait keeps its author and licence in
//! its cache record, which the credit route reads. The catalog names the
//! candidate URLs through
//! [`ArtistImageSource`]; the image itself is downloaded once, from an
//! allowed host only (see
//! [`check_image_url`](droppedneedle_providers::images::check_image_url)),
//! stored full size, and scaled down per requested size like local art. A
//! grid of artists never waits on the lookups: past [`WARM_WAIT`] the route
//! answers 202 and the resolve finishes in the background, at most
//! [`ARTIST_RESOLVE_PERMITS`] at a time.
//!
//! TheAudioDB album thumbnails, fetched by the library precache, stand in
//! for a release group's cover when neither the album's own art nor the
//! archive has one.
//!
//! A library artist with no MusicBrainz link is pictured by a mosaic of
//! their own albums' local art, drawn here and cached like a rendition
//! under the covers' identities and file stats.

pub mod cache;
pub mod local;
pub mod remote;
pub mod resize;
pub mod warm;

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use crate::reads::platform::covers::{CoverArt, CoverBytes, CoverLookup};
use droppedneedle_providers::coverart::{ArtworkBytes, DownloadSize, EntityKind};
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::wikidata::CommonsImage;
use droppedneedle_providers::{PayloadKind, ProviderError, RequestPriority, Singleflight};

use self::cache::{ArtworkCache, CreditSource, ImageCredit, KeyEntry, sha256_hex};
use self::local::{LocalArt, LocalArtwork, local_art_stamp, read_local_art};
use self::remote::{ImageFetch, RemoteCovers, RemoteImages};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;

/// Source label for Cover Art Archive bytes; the routes give these the
/// short cache window, as v2 did.
pub const CAA_SOURCE: &str = "cover-art-archive";
/// How long a warm job gives its page's images to start before checking
/// whether the page is still fetching.
pub const PAGE_SETTLE: Duration = Duration::from_secs(2);
/// How long the web routes wait for an archive fetch before answering 202.
/// A typical fetch takes about 1.8 s (probed 2026-10-19), which this wait
/// covers. Past it, the 202 frees the browser connection, which matters on
/// plain HTTP/1.1, where a browser opens six connections per host.
pub const WARM_WAIT: Duration = Duration::from_millis(2_500);
/// How long compat clients (which cannot poll) wait for an archive fetch.
pub const COMPAT_WAIT: Duration = Duration::from_secs(20);
/// The archive said it has no cover: ask again after four hours (v2
/// `COVER_NEGATIVE_TTL_SECONDS`).
pub const MISS_TTL_SECS: u64 = 4 * 3600;
/// A source could not be asked (down, refusing, or resting after
/// failures): nothing is known about the art, so the routes answer
/// "warming" and ask again after one rest of the provider's row.
pub const OUTAGE_TTL_SECS: u64 = 30;
/// The archive sent an image too large to use: ask again after 15 minutes.
pub const UNUSABLE_TTL_SECS: u64 = 900;
/// Longest one archive fetch runs, waits for its turn included, once no
/// page waits on it. Past it the fetch ends and counts as unreachable.
pub const FLIGHT_LIMIT: Duration = Duration::from_secs(30);
/// [`FLIGHT_LIMIT`] for an artist image resolve, which asks several
/// providers in turn.
pub const ARTIST_FLIGHT_LIMIT: Duration = Duration::from_secs(60);
/// Local art resizes decoding at once; each can hold a large picture in
/// memory.
pub const RESIZE_PERMITS: usize = 3;
/// Largest archive image accepted (v2 `MAX_DELIVERY_IMAGE_BYTES`).
pub const MAX_REMOTE_BYTES: usize = 20 * 1024 * 1024;
/// Source label for artist images.
pub const ARTIST_SOURCE: &str = "artist-image";
/// Source label for TheAudioDB album thumbnails served as covers.
pub const AUDIODB_SOURCE: &str = "audiodb";
/// Source label for a library artist's mosaic of album covers.
pub const MOSAIC_SOURCE: &str = "mosaic";
/// Albums whose art a library artist's mosaic may draw on: the first four
/// that decode among these are used, so a broken cover is passed over.
const MOSAIC_CANDIDATES: usize = 8;
/// How long a precache run waits for one image before moving on (the
/// fetch keeps going in the background).
pub const PRECACHE_WAIT: Duration = Duration::from_secs(60);
/// Artist images resolved at once. Each resolve asks Wikidata, which takes
/// one request at a time, and then Deezer or Commons (MusicBrainz too when
/// Wikidata has no match), so more would only queue.
pub const ARTIST_RESOLVE_PERMITS: usize = 4;

/// One artist picture to try, with the credit its source asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistImageCandidate {
    /// Where to download it.
    pub url: String,
    /// Stored with the picture. Only Commons portraits carry one.
    pub credit: Option<ImageCredit>,
}

impl ArtistImageCandidate {
    /// A picture whose source asks for no credit.
    #[must_use]
    pub fn uncredited(url: String) -> Self {
        Self { url, credit: None }
    }

    /// A Commons portrait with the credit its file page gives.
    #[must_use]
    pub fn commons(image: CommonsImage) -> Self {
        Self {
            url: image.url,
            credit: image.credit.map(|credit| ImageCredit {
                source: CreditSource::WikimediaCommons,
                author: credit.author,
                licence: credit.licence,
                licence_url: credit.licence_url,
                file_url: credit.file_url,
            }),
        }
    }
}

/// Candidate artist images, best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistImageCandidates {
    /// Pictures to try in order: TheAudioDB thumbnail, then the Deezer
    /// picture, then the Commons portrait.
    pub images: Vec<ArtistImageCandidate>,
    /// A source could not answer, so "no image" is not settled yet and is
    /// asked again sooner.
    pub incomplete: bool,
}

/// Names the image URLs an artist has. The catalog implements it; lookups
/// it makes are cached and paced there.
pub trait ArtistImageSource: Send + Sync {
    /// Candidate URLs for one artist (lowercase MBID), asking the
    /// providers at `priority`.
    fn candidates<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, ArtistImageCandidates>;

    /// Fetch and cache, at background priority, the sources a page only
    /// reads from the cache (TheAudioDB). True when they name a picture.
    fn fill<'a>(&'a self, artist_mbid: &'a str) -> BoxFuture<'a, bool>;
}

/// Whether `prefer_local_cover_art` is on now, so a settings change applies
/// to the next request. Local art wins when the section cannot be read.
fn prefers_local(settings: &Live<AdvancedSettings>) -> bool {
    match settings.get() {
        Ok(settings) => settings.prefer_local_cover_art,
        Err(error) => {
            tracing::warn!(%error, "cannot read advanced settings; preferring local art");
            true
        }
    }
}

/// The production [`CoverArt`].
#[derive(Clone)]
pub struct ArtworkService {
    cache: ArtworkCache,
    local: LocalArtwork,
    remote: Option<Arc<dyn RemoteCovers>>,
    settings: Live<AdvancedSettings>,
    flights: Singleflight<Fetched>,
    resizes: Singleflight<Option<Rendition>>,
    mosaics: Singleflight<MosaicBuild>,
    resize_permits: Arc<tokio::sync::Semaphore>,
    images: Option<Arc<dyn RemoteImages>>,
    artist_source: Option<Arc<dyn ArtistImageSource>>,
    artist_flights: Singleflight<Fetched>,
    artist_permits: Arc<tokio::sync::Semaphore>,
}

/// What one fetch or resolve learned.
#[derive(Debug, Clone)]
enum Fetched {
    /// The image, now stored.
    Found(CoverBytes),
    /// The source has none.
    Missing,
    /// The source could not be asked; nothing is known.
    Unreachable,
}

impl Fetched {
    fn lookup(&self) -> CoverLookup {
        match self {
            Self::Found(cover) => CoverLookup::Found(cover.clone()),
            Self::Missing => CoverLookup::Missing,
            Self::Unreachable => CoverLookup::Warming,
        }
    }
}

/// A scaled-down copy of local art.
#[derive(Debug, Clone)]
struct Rendition {
    bytes: Vec<u8>,
    content_type: String,
    hash: String,
}

/// What drawing an artist's mosaic came to.
#[derive(Debug, Clone)]
enum MosaicBuild {
    /// The mosaic, now stored.
    Built(Rendition),
    /// Only the candidate at this index decoded.
    Single(usize),
    /// None of the candidates decoded.
    Nothing,
}

impl ArtworkService {
    /// Wire the service. `remote` is `None` only where no network source
    /// should be used (tests of the local path). `settings` says, per
    /// request, whether local art comes first.
    pub fn new(
        cache: ArtworkCache,
        local: LocalArtwork,
        remote: Option<Arc<dyn RemoteCovers>>,
        settings: Live<AdvancedSettings>,
    ) -> Self {
        Self {
            cache,
            local,
            remote,
            settings,
            flights: Singleflight::new(),
            resizes: Singleflight::new(),
            mosaics: Singleflight::new(),
            resize_permits: Arc::new(tokio::sync::Semaphore::new(RESIZE_PERMITS)),
            images: None,
            artist_source: None,
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

    /// Name artist images through `source` (the catalog).
    #[must_use]
    pub fn with_artist_source(mut self, source: Arc<dyn ArtistImageSource>) -> Self {
        self.artist_source = Some(source);
        self
    }

    /// Whether an album's own art comes before the Cover Art Archive now,
    /// as the next cover read will decide it.
    pub fn prefers_local_art(&self) -> bool {
        prefers_local(&self.settings)
    }

    /// The disk cache, for the admin stats and clears.
    pub fn cache(&self) -> &ArtworkCache {
        &self.cache
    }

    /// The credit stored with the artist's image, while the image is held.
    pub async fn artist_credit(&self, artist_mbid: &str) -> Option<ImageCredit> {
        let mbid = artist_mbid.trim().to_ascii_lowercase();
        if !is_valid_mbid(&mbid) {
            return None;
        }
        self.cache.credit(&artist_key(&mbid)).await
    }

    /// Whether the artist's image question is settled in the cache: an
    /// image, or a "none" that has not expired.
    pub async fn artist_image_cached(&self, artist_mbid: &str) -> bool {
        self.settled(&artist_key(artist_mbid)).await
    }

    /// Resolve and cache the artist's image now (the precache and the
    /// page warm jobs, at background priority), waiting up to
    /// [`PRECACHE_WAIT`]. True when an image is cached.
    pub async fn warm_artist_image(&self, artist_mbid: &str) -> bool {
        matches!(
            self.artist_original(artist_mbid, PRECACHE_WAIT, RequestPriority::BackgroundSync)
                .await,
            CoverLookup::Found(_)
        )
    }

    /// Fill the artist's background-only sources (TheAudioDB). When they
    /// name a picture and no image is stored, the image is resolved now,
    /// past any "none" marker; a stored image is kept until the cache
    /// lets it go.
    pub async fn fill_artist_image(&self, artist_mbid: &str) {
        let Some(source) = self.artist_source.clone() else {
            return;
        };
        if !source.fill(artist_mbid).await {
            return;
        }
        let key = artist_key(artist_mbid);
        if let Some(KeyEntry::Hit { hash, .. }) = self.cache.key(&key).await
            && self.cache.has_blob(&hash).await
        {
            return;
        }
        self.resolve_artist_original(
            artist_mbid,
            key,
            PRECACHE_WAIT,
            RequestPriority::BackgroundSync,
        )
        .await;
    }

    /// Whether a release group's 500 px cover is settled: the album's own
    /// art (when preferred), or an archive answer that has not expired.
    pub async fn release_group_cover_cached(&self, mbid: &str) -> bool {
        let mbid = mbid.trim().to_ascii_lowercase();
        if prefers_local(&self.settings)
            && matches!(self.local.by_release_group(&mbid).await, Ok(Some(_)))
        {
            return true;
        }
        self.settled(&format!(
            "caa:{}:{mbid}:500",
            EntityKind::ReleaseGroup.path()
        ))
        .await
    }

    /// Fetch and cache a release group's 500 px cover now (the precache
    /// and the page warm jobs), asking the archive at `priority`.
    pub async fn warm_release_group_cover(&self, mbid: &str, priority: RequestPriority) -> bool {
        matches!(
            self.mbid_cover(
                EntityKind::ReleaseGroup,
                mbid,
                Some("500"),
                PRECACHE_WAIT,
                priority
            )
            .await,
            CoverLookup::Found(_)
        )
    }

    /// A release's front cover (its release group's when the release has
    /// none) at `size` (`None` for the original), through the cache and
    /// the fallbacks the pages use. Library Management embeds it and writes
    /// it next to the album. `Err` when an archive fetch is still running.
    /// The archive is asked at `priority`.
    pub async fn front_cover(
        &self,
        release_mbid: &str,
        release_group_mbid: &str,
        size: Option<&str>,
        priority: RequestPriority,
    ) -> Result<Option<CoverBytes>, ()> {
        let mut warming = false;
        for (entity, mbid) in [
            (EntityKind::Release, release_mbid),
            (EntityKind::ReleaseGroup, release_group_mbid),
        ] {
            match self
                .mbid_cover(entity, mbid, size, PRECACHE_WAIT, priority)
                .await
            {
                CoverLookup::Found(cover) => return Ok(Some(cover)),
                CoverLookup::Warming => warming = true,
                CoverLookup::Missing => {}
            }
        }
        if warming { Err(()) } else { Ok(None) }
    }

    /// Whether TheAudioDB's thumbnail for a release group is settled.
    pub async fn audiodb_cover_cached(&self, mbid: &str) -> bool {
        self.settled(&audiodb_cover_key(mbid)).await
    }

    /// Download and keep TheAudioDB's thumbnail for a release group, which
    /// stands in when the album has no other cover (v2's AudioDB prewarm,
    /// at background priority). `None` records that AudioDB has no
    /// thumbnail.
    pub async fn store_audiodb_cover(&self, mbid: &str, url: Option<&str>) {
        let key = audiodb_cover_key(mbid);
        let Some(url) = url else {
            self.cache.put_miss(&key, MISS_TTL_SECS).await;
            return;
        };
        let Some(images) = &self.images else {
            return;
        };
        match images.fetch(url, RequestPriority::BackgroundSync).await {
            ImageFetch::Found(art) => {
                self.cache
                    .put(Some(&key), art.bytes, &art.content_type)
                    .await;
            }
            ImageFetch::Unusable => self.cache.put_miss(&key, MISS_TTL_SECS).await,
            ImageFetch::Failed => self.cache.put_unreachable(&key, OUTAGE_TTL_SECS).await,
        }
    }

    /// True when `key` holds an image still on disk, or a fresh marker.
    async fn settled(&self, key: &str) -> bool {
        match self.cache.key(key).await {
            Some(KeyEntry::Hit { hash, .. }) => self.cache.has_blob(&hash).await,
            Some(KeyEntry::Miss { until } | KeyEntry::Unreachable { until }) => {
                until > self.cache.unix_now()
            }
            None => false,
        }
    }

    /// Wait, up to `limit`, until no page is fetching an image, so a page's
    /// warm job does not reach the providers alongside the page itself. A
    /// page's images start a moment after its data, so the wait first
    /// gives them [`PAGE_SETTLE`] to begin.
    pub async fn after_page_fetches(&self, limit: Duration) {
        let started = tokio::time::Instant::now();
        tokio::time::sleep(PAGE_SETTLE.min(limit)).await;
        while started.elapsed() < limit
            && (self.flights.any_running("page:").await
                || self.artist_flights.any_running("page:").await)
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
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
    /// background while the caller waits up to `wait`, asking the
    /// providers at `priority`. Resolves are coalesced per priority class,
    /// so a page never waits on a background resolve of the same artist.
    async fn artist_original(
        &self,
        mbid: &str,
        wait: Duration,
        priority: RequestPriority,
    ) -> CoverLookup {
        let key = artist_key(mbid);
        match self.cache.key(&key).await {
            Some(KeyEntry::Hit { .. }) => {
                if let Some(image) = self.cached_image(&key, ARTIST_SOURCE).await {
                    return CoverLookup::Found(image);
                }
            }
            Some(KeyEntry::Miss { until }) if until > self.cache.unix_now() => {
                return CoverLookup::Missing;
            }
            Some(KeyEntry::Unreachable { until }) if until > self.cache.unix_now() => {
                return CoverLookup::Warming;
            }
            _ => {}
        }
        self.resolve_artist_original(mbid, key, wait, priority)
            .await
    }

    /// Resolve the artist's image in the background while the caller
    /// waits up to `wait`, whatever the cache holds.
    async fn resolve_artist_original(
        &self,
        mbid: &str,
        key: String,
        wait: Duration,
        priority: RequestPriority,
    ) -> CoverLookup {
        let (Some(source), Some(images)) = (self.artist_source.clone(), self.images.clone()) else {
            return CoverLookup::Missing;
        };
        let flights = self.artist_flights.clone();
        let cache = self.cache.clone();
        let permits = Arc::clone(&self.artist_permits);
        let mbid = mbid.to_owned();
        let class = if priority.is_user_facing() {
            "page"
        } else {
            "background"
        };
        let flight_key = format!("{class}:{key}");
        // A page asking again (a poll) while its resolve runs is answered
        // at once rather than waiting out another `wait`. Only the web
        // routes, which poll a 202; compat callers wait and join the flight.
        if priority.is_user_facing() && wait <= WARM_WAIT && flights.is_running(&flight_key).await {
            return CoverLookup::Warming;
        }
        // Spawned so a resolve the caller stops waiting for still lands in
        // the cache for the next request.
        let flight = tokio::spawn(async move {
            flights
                .run(&flight_key, move || async move {
                    let resolve = async {
                        let _permit = permits.acquire_owned().await.ok();
                        resolve_artist(
                            (source.as_ref(), images.as_ref()),
                            &cache,
                            &mbid,
                            &key,
                            priority,
                        )
                        .await
                    };
                    Ok(
                        match tokio::time::timeout(ARTIST_FLIGHT_LIMIT, resolve).await {
                            Ok(fetched) => fetched,
                            Err(_) => {
                                tracing::debug!(artist = %mbid, "artist image resolve ran out of time");
                                cache.put_unreachable(&key, OUTAGE_TTL_SECS).await;
                                Fetched::Unreachable
                            }
                        },
                    )
                })
                .await
        });
        match tokio::time::timeout(wait, flight).await {
            Ok(Ok(Ok(fetched))) => fetched.lookup(),
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

    /// Release or release-group cover by MBID, the archive asked at
    /// `priority`.
    async fn mbid_cover(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: Option<&str>,
        wait: Duration,
        priority: RequestPriority,
    ) -> CoverLookup {
        let mbid = mbid.trim().to_ascii_lowercase();
        if !is_valid_mbid(&mbid) {
            return CoverLookup::Missing;
        }
        let prefer_local = prefers_local(&self.settings);
        if prefer_local && let Some(cover) = self.local_for(entity, &mbid, size).await {
            return CoverLookup::Found(cover);
        }
        match self.remote_cover(entity, &mbid, size, wait, priority).await {
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

    /// A scan-recorded art row at `size` (`None` is full size). A sized
    /// request is answered from the rendition recorded for the file's
    /// current stat without opening the file. Otherwise the file is read
    /// and scaled down once, and only the rendition is cached.
    async fn serve_local(&self, art: &LocalArt, size: Option<&str>) -> Option<CoverBytes> {
        let stamp_key = match size.and_then(|size| size.parse::<u32>().ok()) {
            Some(max) => local_art_stamp(art)
                .await
                .map(|stamp| local_rendition_key(art, &stamp, max)),
            None => None,
        };
        if let Some(key) = &stamp_key
            && let Some(cover) = self.cached_image(key, &art.source).await
        {
            return Some(CoverBytes {
                version: Some(art.version),
                ..cover
            });
        }
        let (bytes, content_type) = read_local_art(art).await?;
        let original = CoverBytes {
            hash: sha256_hex(&bytes),
            bytes,
            content_type: content_type.to_owned(),
            source: art.source.clone(),
            version: Some(art.version),
        };
        let cover = self.sized(original, size).await;
        if let Some(key) = stamp_key
            && self.cache.has_blob(&cover.hash).await
        {
            self.cache
                .put_key(&key, &cover.hash, &cover.content_type)
                .await;
        }
        Some(cover)
    }

    /// A library artist's picture from their albums' art (`arts`, in
    /// order) at `size`: a mosaic when two or more covers decode, else the
    /// one cover as an album shows it. Full size is drawn at
    /// [`ARTIST_STORED_MAX`]. The mosaic is cached under the identities and
    /// file stats of the covers it may use, so a changed or added cover
    /// draws a new one.
    async fn artist_mosaic(&self, arts: Vec<LocalArt>, size: Option<&str>) -> Option<CoverBytes> {
        let mut candidates = Vec::new();
        for art in arts {
            if let Some(stamp) = local_art_stamp(&art).await {
                candidates.push((art, stamp));
            }
        }
        if candidates.len() < 2 {
            let (art, _) = candidates.into_iter().next()?;
            return self.serve_local(&art, size).await;
        }
        let max = size
            .and_then(|size| size.parse::<u32>().ok())
            .unwrap_or(ARTIST_STORED_MAX);
        let key = mosaic_key(&candidates, max);
        let arts: Vec<LocalArt> = candidates.into_iter().map(|(art, _)| art).collect();
        match self.cache.key(&key).await {
            Some(KeyEntry::Hit { .. }) => {
                if let Some(cover) = self.cached_image(&key, MOSAIC_SOURCE).await {
                    return Some(cover);
                }
            }
            Some(KeyEntry::Miss { until }) if until > self.cache.unix_now() => {
                return self.serve_local(arts.first()?, size).await;
            }
            _ => {}
        }
        let (cache, permits, sources) = (
            self.cache.clone(),
            self.resize_permits.clone(),
            arts.clone(),
        );
        let flight_key = key.clone();
        let build = self
            .mosaics
            .run(&flight_key, move || async move {
                Ok(build_mosaic(cache, permits, key, sources, max / 2).await)
            })
            .await;
        match build.as_deref() {
            Ok(MosaicBuild::Built(mosaic)) => Some(CoverBytes {
                bytes: mosaic.bytes.clone(),
                content_type: mosaic.content_type.clone(),
                source: MOSAIC_SOURCE.to_owned(),
                hash: mosaic.hash.clone(),
                version: None,
            }),
            Ok(MosaicBuild::Single(index)) => {
                // Only one cover decodes: the mosaic key points at its
                // rendition, so the next request reads nothing again.
                let cover = self.serve_local(arts.get(*index)?, size).await?;
                if self.cache.has_blob(&cover.hash).await {
                    self.cache
                        .put_key(&flight_key, &cover.hash, &cover.content_type)
                        .await;
                }
                Some(cover)
            }
            Ok(MosaicBuild::Nothing) | Err(_) => self.serve_local(arts.first()?, size).await,
        }
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

    /// Archive cover through the cache, waiting up to `wait`. A 250 is made
    /// from the 500 file, so the two sizes share one fetch and one "no art"
    /// answer.
    async fn remote_cover(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: Option<&str>,
        wait: Duration,
        priority: RequestPriority,
    ) -> CoverLookup {
        let asked = download_size(size);
        let file = match asked {
            DownloadSize::Size250 => DownloadSize::Size500,
            other => other,
        };
        match self.archive_file(entity, mbid, file, wait, priority).await {
            CoverLookup::Found(cover) if asked == DownloadSize::Size250 => {
                CoverLookup::Found(self.sized(cover, size).await)
            }
            other => other,
        }
    }

    /// One archive file (the original or a thumbnail width) through the
    /// cache: a stored copy, its "no art" answer, or a fetch shared with
    /// every request for that file. Fetches are coalesced per priority class,
    /// so a page never waits on a background fetch of the same cover.
    async fn archive_file(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: DownloadSize,
        wait: Duration,
        priority: RequestPriority,
    ) -> CoverLookup {
        let Some(remote) = self.remote.clone() else {
            return CoverLookup::Missing;
        };
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
            Some(KeyEntry::Miss { until }) if until > self.cache.unix_now() => {
                return CoverLookup::Missing;
            }
            Some(KeyEntry::Unreachable { until }) if until > self.cache.unix_now() => {
                return CoverLookup::Warming;
            }
            _ => {}
        }
        let flights = self.flights.clone();
        let cache = self.cache.clone();
        let mbid = mbid.to_owned();
        let class = if priority.is_user_facing() {
            "page"
        } else {
            "background"
        };
        let flight_key = format!("{class}:{key}");
        // A page asking again (a poll) while its fetch runs is answered at
        // once rather than waiting out another `wait`. Only the web routes,
        // which poll a 202; compat callers wait and join the flight.
        if priority.is_user_facing() && wait <= WARM_WAIT && flights.is_running(&flight_key).await {
            return CoverLookup::Warming;
        }
        // Spawned so a fetch the caller stops waiting for still lands in
        // the cache for the next request.
        let flight = tokio::spawn(async move {
            flights
                .run(&flight_key, move || async move {
                    let fetch = fetch_and_store(
                        remote.as_ref(),
                        &cache,
                        CoverFetch {
                            entity,
                            mbid: &mbid,
                            size,
                            priority,
                        },
                        &key,
                    );
                    Ok(match tokio::time::timeout(FLIGHT_LIMIT, fetch).await {
                        Ok(fetched) => fetched,
                        Err(_) => {
                            tracing::debug!(mbid = %mbid, "cover fetch ran out of time");
                            cache.put_unreachable(&key, OUTAGE_TTL_SECS).await;
                            Fetched::Unreachable
                        }
                    })
                })
                .await
        });
        match tokio::time::timeout(wait, flight).await {
            Ok(Ok(Ok(fetched))) => fetched.lookup(),
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

/// Scale art down to `max` under a resize permit and cache the rendition
/// under `key`. Art already small enough is stored as its own rendition
/// (written once, by its existing hash) so it is not decoded again. A
/// failed resize task caches nothing.
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
    let shrunk = tokio::task::spawn_blocking(move || {
        let shrunk = resize::shrink_to_fit(&source, max);
        (shrunk, source)
    })
    .await;
    match shrunk {
        Ok((Some((bytes, content_type)), _)) => {
            let hash = cache.put(Some(&key), bytes.clone(), content_type).await;
            Some(Rendition {
                bytes,
                content_type: content_type.to_owned(),
                hash,
            })
        }
        Ok((None, source)) => {
            if cache.has_blob(&original_hash).await {
                cache.put_key(&key, &original_hash, &original_type).await;
            } else {
                cache.put(Some(&key), source, &original_type).await;
            }
            None
        }
        Err(error) => {
            tracing::error!(%error, "art resize task failed; serving the original");
            None
        }
    }
}

/// Draw an artist's mosaic from the first four of `arts` whose art decodes,
/// each cut to a `side` pixel square, and cache it under `key`. Decoding
/// and drawing hold a resize permit; reading the files does not. Art that
/// cannot be read or decoded is passed over with a log line. When none
/// decodes, `key` remembers that for a while so the files are not read
/// again on every request.
async fn build_mosaic(
    cache: ArtworkCache,
    permits: Arc<tokio::sync::Semaphore>,
    key: String,
    arts: Vec<LocalArt>,
    side: u32,
) -> MosaicBuild {
    let mut tiles = Vec::new();
    for (index, art) in arts.iter().enumerate() {
        if tiles.len() == 4 {
            break;
        }
        let Some((bytes, _)) = read_local_art(art).await else {
            tracing::warn!(album = %art.album_id, "album art unreadable; left out of the artist mosaic");
            continue;
        };
        let Some(_permit) = resize_permit(&permits).await else {
            return MosaicBuild::Nothing;
        };
        match cache::blocking(move || resize::tile(&bytes, side)).await {
            Some(Ok(tile)) => tiles.push((index, tile)),
            Some(Err(error)) => {
                tracing::warn!(album = %art.album_id, %error, "album art did not decode; left out of the artist mosaic");
            }
            None => {}
        }
    }
    match tiles.as_slice() {
        [] => {
            cache.put_miss(&key, UNUSABLE_TTL_SECS).await;
            return MosaicBuild::Nothing;
        }
        [(index, _)] => return MosaicBuild::Single(*index),
        _ => {}
    }
    let tiles: Vec<_> = tiles.into_iter().map(|(_, tile)| tile).collect();
    let Some(_permit) = resize_permit(&permits).await else {
        return MosaicBuild::Nothing;
    };
    match cache::blocking(move || resize::mosaic(&tiles, side)).await {
        Some(Some(bytes)) => {
            let hash = cache.put(Some(&key), bytes.clone(), "image/jpeg").await;
            MosaicBuild::Built(Rendition {
                bytes,
                content_type: "image/jpeg".to_owned(),
                hash,
            })
        }
        Some(None) | None => MosaicBuild::Nothing,
    }
}

/// One of the resize permits, or `None` (logged) if they are closed.
async fn resize_permit(
    permits: &Arc<tokio::sync::Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    match permits.clone().acquire_owned().await {
        Ok(permit) => Some(permit),
        Err(error) => {
            tracing::error!(%error, "resize permits closed; no artist mosaic");
            None
        }
    }
}

/// One archive fetch.
struct CoverFetch<'a> {
    entity: EntityKind,
    mbid: &'a str,
    size: DownloadSize,
    priority: RequestPriority,
}

/// One archive fetch: store the bytes, a miss marker, or an unreachable
/// marker.
async fn fetch_and_store(
    remote: &dyn RemoteCovers,
    cache: &ArtworkCache,
    fetch: CoverFetch<'_>,
    key: &str,
) -> Fetched {
    let CoverFetch {
        entity,
        mbid,
        size,
        priority,
    } = fetch;
    match remote.front(entity, mbid, size, priority).await {
        Ok(Some(art)) => {
            let hash = cache
                .put(Some(key), art.bytes.clone(), &art.content_type)
                .await;
            Fetched::Found(CoverBytes {
                bytes: art.bytes,
                content_type: art.content_type,
                source: CAA_SOURCE.to_owned(),
                hash,
                version: None,
            })
        }
        Ok(None) => {
            cache.put_miss(key, MISS_TTL_SECS).await;
            Fetched::Missing
        }
        Err(error) => {
            tracing::warn!(%error, entity = entity.path(), mbid, "cover art archive fetch failed");
            match failure_marker(&error) {
                Marker::Unreachable(ttl) => {
                    cache.put_unreachable(key, ttl).await;
                    Fetched::Unreachable
                }
                Marker::Miss(ttl) => {
                    cache.put_miss(key, ttl).await;
                    Fetched::Missing
                }
            }
        }
    }
}

/// How a failed archive fetch is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// Nothing was learned (an outage, a refusal, a rate limit, or our own
    /// limiter turning the call down): ask again after this many seconds.
    Unreachable(u64),
    /// The archive answered with something unusable: treat it as no art
    /// for this many seconds.
    Miss(u64),
}

/// How a failed archive fetch is remembered, and for how long. A wait the
/// archive or our limiter named is honoured up to [`OUTAGE_TTL_SECS`]; an
/// image too large to use is asked again after [`UNUSABLE_TTL_SECS`];
/// anything else (a refused redirect, bytes that are not an image, a
/// malformed MBID) is remembered like "no cover".
fn failure_marker(error: &ProviderError) -> Marker {
    let named = |wait: &Duration| {
        (wait.as_secs() + u64::from(wait.subsec_nanos() > 0)).clamp(1, OUTAGE_TTL_SECS)
    };
    match error {
        ProviderError::RateLimited {
            retry_after: Some(wait),
            ..
        }
        | ProviderError::Unavailable {
            retry_after: Some(wait),
            ..
        } => Marker::Unreachable(named(wait)),
        ProviderError::RateLimited { .. }
        | ProviderError::Unavailable { .. }
        | ProviderError::Server { .. }
        | ProviderError::Transport { .. } => Marker::Unreachable(OUTAGE_TTL_SECS),
        ProviderError::Payload {
            kind: PayloadKind::TooLarge,
            ..
        } => Marker::Miss(UNUSABLE_TTL_SECS),
        _ => Marker::Miss(MISS_TTL_SECS),
    }
}

/// Cache key of a local art rendition: the file and the stat it had when the
/// rendition was made, so a replaced image misses and is read again.
fn local_rendition_key(art: &LocalArt, stamp: &str, max: u32) -> String {
    format!(
        "local:{}:{}:{stamp}:{max}",
        art.source,
        art.path.to_string_lossy()
    )
}

/// Cache key of an artist's mosaic at `max` pixels: every cover it may draw
/// on, with the stat its file had, in order.
fn mosaic_key(candidates: &[(LocalArt, String)], max: u32) -> String {
    let sources: Vec<String> = candidates
        .iter()
        .map(|(art, stamp)| {
            format!(
                "{}:{}:{}:{stamp}:{}:{}",
                art.album_id,
                art.source,
                art.path.to_string_lossy(),
                art.version,
                art.content_hash.as_deref().unwrap_or("")
            )
        })
        .collect();
    format!("mosaic:{}:{max}", sha256_hex(sources.join("\n").as_bytes()))
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
/// cached. With none, a miss marker, or an unreachable marker when a
/// source could not answer.
async fn resolve_artist(
    (source, images): (&dyn ArtistImageSource, &dyn RemoteImages),
    cache: &ArtworkCache,
    mbid: &str,
    key: &str,
    priority: RequestPriority,
) -> Fetched {
    let candidates = source.candidates(mbid, priority).await;
    let mut failed = candidates.incomplete;
    for candidate in &candidates.images {
        match images.fetch(&candidate.url, priority).await {
            ImageFetch::Found(art) => {
                let art = shrink_artist_image(art).await;
                let hash = cache
                    .put_credited(
                        Some(key),
                        art.bytes.clone(),
                        &art.content_type,
                        candidate.credit.as_ref(),
                    )
                    .await;
                return Fetched::Found(CoverBytes {
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
    if failed {
        cache.put_unreachable(key, OUTAGE_TTL_SECS).await;
        Fetched::Unreachable
    } else {
        cache.put_miss(key, MISS_TTL_SECS).await;
        Fetched::Missing
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
        Box::pin(self.mbid_cover(
            EntityKind::ReleaseGroup,
            release_group_id,
            size,
            WARM_WAIT,
            RequestPriority::ImageFetch,
        ))
    }

    fn release_cover<'a>(
        &'a self,
        release_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(self.mbid_cover(
            EntityKind::Release,
            release_id,
            size,
            WARM_WAIT,
            RequestPriority::ImageFetch,
        ))
    }

    fn artist_image<'a>(
        &'a self,
        artist_id: &'a str,
        size_px: Option<u32>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(async move {
            let mbid = artist_id.trim().to_ascii_lowercase();
            if !is_valid_mbid(&mbid) {
                return CoverLookup::Missing;
            }
            match self
                .artist_original(&mbid, WARM_WAIT, RequestPriority::ImageFetch)
                .await
            {
                CoverLookup::Found(original) => {
                    CoverLookup::Found(self.sized(original, artist_size(size_px)).await)
                }
                other => other,
            }
        })
    }

    fn artist_image_credit<'a>(&'a self, artist_id: &'a str) -> BoxFuture<'a, Option<ImageCredit>> {
        Box::pin(self.artist_credit(artist_id))
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
            let prefer_local = prefers_local(&self.settings);
            if prefer_local
                && let Some(art) = &context.local
                && let Some(cover) = self.serve_local(art, size).await
            {
                return Some(cover);
            }
            if let Some(group) = &context.release_group_mbid {
                let group = group.trim().to_ascii_lowercase();
                if is_valid_mbid(&group)
                    && let CoverLookup::Found(cover) = self
                        .remote_cover(
                            EntityKind::ReleaseGroup,
                            &group,
                            size,
                            COMPAT_WAIT,
                            RequestPriority::ImageFetch,
                        )
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

    fn local_artist_art<'a>(
        &'a self,
        artist_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        Box::pin(async move {
            match self.local.by_artist(artist_id, MOSAIC_CANDIDATES).await {
                Ok(arts) => self.artist_mosaic(arts, size).await,
                Err(error) => {
                    tracing::warn!(%error, "artist art lookup failed");
                    None
                }
            }
        })
    }
}
