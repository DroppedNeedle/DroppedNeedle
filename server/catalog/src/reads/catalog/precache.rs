//! What the library precache and the artist images ask of the catalog.
//!
//! [`CatalogPrecache`] implements the precache's lookups over the catalog
//! (page data, discovery sections, TheAudioDB answers) and the artwork
//! service (artist images, covers). [`CatalogArtistImages`] names an
//! artist's image URLs for the artwork service. Every call goes through
//! the catalog's own caches and provider pacing, so a precache run fills
//! exactly what a page visit would, at the pace the providers allow.

use crate::reads::platform::artwork::{ArtistImageCandidates, ArtistImageSource, ArtworkService};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::precache_phases::{
    LibraryAlbum, LibraryArtist, PrecacheLibrary, PrecacheSources, PrecacheTuning,
};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;

use super::Catalog;
use super::error::CatalogError;

/// Discovery section sizes the precache warms: the page defaults, so the
/// cached answers are the ones an artist page asks for.
const SIMILAR_COUNT: u32 = 15;
const TOP_SONGS_COUNT: u32 = 10;
const TOP_ALBUMS_COUNT: u32 = 10;
/// The precache warms ahead of any page, so it waits behind them.
const BACKGROUND: droppedneedle_providers::RequestPriority =
    droppedneedle_providers::RequestPriority::BackgroundSync;

impl Catalog {
    /// The library's identified artists and albums.
    pub async fn precache_library(&self) -> Result<PrecacheLibrary, String> {
        let artists = self
            .local()
            .identified_artists()
            .await
            .map_err(|error| format!("Could not read the library's artists: {error}"))?;
        let albums = self
            .local()
            .identified_albums()
            .await
            .map_err(|error| format!("Could not read the library's albums: {error}"))?;
        Ok(PrecacheLibrary {
            artists: artists
                .into_iter()
                .map(|(mbid, name)| LibraryArtist { mbid, name })
                .collect(),
            albums: albums
                .into_iter()
                .map(|(release_group_mbid, title, artist_name)| LibraryAlbum {
                    release_group_mbid,
                    title,
                    artist_name,
                })
                .collect(),
        })
    }

    /// Whether the artist page's MusicBrainz data is cached.
    async fn artist_page_cached(&self, mbid: &str) -> bool {
        let cache = self.upstream().cache();
        cache
            .get_bytes(&self.artist_detail_key(mbid))
            .await
            .is_some()
            && cache
                .get_bytes(&self.release_groups_key(mbid))
                .await
                .is_some()
    }

    /// Fetch and cache the artist page's data.
    async fn warm_artist_page(&self, mbid: &str) {
        if let Err(error) = self.build_artist(mbid, BACKGROUND).await {
            tracing::debug!(artist = mbid, %error, "precache: artist page not cached");
        }
    }

    /// Fetch and cache the artist's discovery sections as `user_id` sees
    /// them.
    async fn warm_artist_discovery(&self, user_id: &str, mbid: &str) {
        let (similar, songs, albums) = tokio::join!(
            self.similar_artists(user_id, mbid, SIMILAR_COUNT, None, BACKGROUND),
            self.top_songs(user_id, mbid, TOP_SONGS_COUNT, None, BACKGROUND),
            self.top_albums(user_id, mbid, TOP_ALBUMS_COUNT, None, BACKGROUND),
        );
        if let Some(error) = [similar.err(), songs.err(), albums.err()]
            .into_iter()
            .flatten()
            .next()
        {
            tracing::debug!(artist = mbid, %error, "precache: discovery partly cached");
        }
    }

    /// Whether the album page's MusicBrainz data is cached.
    async fn album_page_cached(&self, mbid: &str) -> bool {
        self.upstream()
            .cache()
            .get_bytes(&self.group_key(mbid))
            .await
            .is_some()
    }

    /// Fetch and cache the album page's data.
    async fn warm_album_page(&self, mbid: &str) {
        if let Err(error) = self.album(mbid).await {
            tracing::debug!(album = mbid, %error, "precache: album page not cached");
        }
    }

    /// TheAudioDB thumbnail URL for a release group: `Some(None)` when it
    /// has none, `None` when the answer is not settled.
    async fn album_audiodb_thumb(&self, mbid: &str) -> Option<Option<String>> {
        let group = match self.group_detail(mbid).await {
            Ok(Some(group)) => group,
            Ok(None) => return Some(None),
            Err(error) => {
                tracing::debug!(album = mbid, %error, "precache: album unknown to MusicBrainz");
                return None;
            }
        };
        self.album_images_lookup(&group, BACKGROUND)
            .await
            .map(|images| images.and_then(|images| images.album_thumb_url))
    }

    /// Candidate image URLs for an artist, best first, the providers asked
    /// at `priority`: TheAudioDB's thumbnail, else the pictures of the one
    /// Wikidata item carrying the artist's MBID (Deezer's, then Commons'),
    /// else, when no single item carries it, the portrait MusicBrainz's
    /// Wikidata link leads to. A page reads TheAudioDB from the cache only
    /// and queues a fill on a miss, so a cold page asks neither MusicBrainz
    /// nor TheAudioDB when Wikidata knows the MBID; background work asks
    /// TheAudioDB directly.
    pub async fn artist_image_candidates(
        &self,
        mbid: &str,
        priority: droppedneedle_providers::RequestPriority,
    ) -> ArtistImageCandidates {
        let mut candidates = ArtistImageCandidates::default();
        let mut detail = None;
        if priority.is_user_facing() {
            let cached = self.page_artist_images(mbid, mbid).await;
            if let Some(thumb) = cached.and_then(|images| images.thumb_url) {
                candidates.urls.push(thumb);
                return candidates;
            }
        } else {
            let found = match self.artist_detail(mbid, priority).await {
                Ok(Some(found)) => found,
                Ok(None) => return ArtistImageCandidates::default(),
                Err(error) => return musicbrainz_unavailable(mbid, &error),
            };
            match self
                .artist_images_lookup(&found.mbid, &found.name, priority)
                .await
            {
                Some(images) => {
                    if let Some(thumb) = images.and_then(|images| images.thumb_url) {
                        candidates.urls.push(thumb);
                        return candidates;
                    }
                }
                None => candidates.incomplete = true,
            }
            detail = Some(found);
        }
        let canonical = detail.as_ref().map_or(mbid, |found| found.mbid.as_str());
        match self.wikidata_artist_pictures(canonical, priority).await {
            WikidataPictures::Matched { urls, failed } => {
                candidates.urls = urls;
                candidates.incomplete |= failed;
                return candidates;
            }
            WikidataPictures::NoMatch { failed } => candidates.incomplete |= failed,
        }
        let detail = match detail {
            Some(found) => found,
            None => match self.artist_detail(mbid, priority).await {
                Ok(Some(found)) => {
                    // A merged MBID: TheAudioDB's answer is kept under the
                    // artist's current one.
                    if found.mbid != mbid
                        && let Some(thumb) = self
                            .cached_artist_images(&found.mbid)
                            .await
                            .and_then(|images| images.thumb_url)
                    {
                        candidates.urls.push(thumb);
                        return candidates;
                    }
                    found
                }
                Ok(None) => return candidates,
                Err(error) => return musicbrainz_unavailable(mbid, &error),
            },
        };
        let biography = self.biography(&detail, priority).await;
        // A failed portrait lookup keeps the short outage marker.
        candidates.incomplete |= biography.image_failed;
        candidates.urls.extend(biography.image);
        candidates
    }

    /// The pictures of the one Wikidata item whose MusicBrainz artist id
    /// (P434) is `mbid`: Deezer's picture by the item's Deezer artist id
    /// (P2722), else its Commons image (P18). Matched by exact id, never by
    /// name.
    async fn wikidata_artist_pictures(
        &self,
        mbid: &str,
        priority: droppedneedle_providers::RequestPriority,
    ) -> WikidataPictures {
        let wiki = self.upstream().wikidata();
        let item = match wiki.item_for_musicbrainz_artist(mbid, priority).await {
            Ok(Some(item)) => item,
            Ok(None) => return WikidataPictures::NoMatch { failed: false },
            Err(error) => {
                tracing::debug!(artist = mbid, %error, "artist image: Wikidata search failed");
                return WikidataPictures::NoMatch { failed: true };
            }
        };
        let ids = match wiki.artist_picture_ids(&item, priority).await {
            Ok(ids) => ids.unwrap_or_default(),
            Err(error) => {
                tracing::debug!(artist = mbid, %error, "artist image: Wikidata claims failed");
                return WikidataPictures::Matched {
                    urls: Vec::new(),
                    failed: true,
                };
            }
        };
        let mut failed = false;
        if let Some(deezer_id) = ids.deezer_artist {
            match self
                .upstream()
                .deezer()
                .deezer_artist_picture(deezer_id, priority)
                .await
            {
                Ok(Some(url)) => {
                    return WikidataPictures::Matched {
                        urls: vec![url],
                        failed: false,
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(artist = mbid, %error, "artist image: Deezer failed");
                    failed = true;
                }
            }
        }
        let mut urls = Vec::new();
        if let Some(file) = &ids.commons_file {
            match wiki.commons_image(file, priority).await {
                Ok(url) => urls.extend(url),
                Err(error) => {
                    tracing::debug!(artist = mbid, %error, "artist image: Commons failed");
                    failed = true;
                }
            }
        }
        WikidataPictures::Matched { urls, failed }
    }

    /// Fetch and cache TheAudioDB's answer for an artist at background
    /// priority. True when it names a thumbnail. Safe to repeat: a cached
    /// answer is not asked again.
    pub async fn fill_artist_images(&self, mbid: &str) -> bool {
        let detail = match self.artist_detail(mbid, BACKGROUND).await {
            Ok(Some(detail)) => detail,
            Ok(None) => return false,
            Err(error) => {
                tracing::debug!(artist = mbid, %error, "AudioDB fill: MusicBrainz unavailable");
                return false;
            }
        };
        self.artist_images_lookup(&detail.mbid, &detail.name, BACKGROUND)
            .await
            .flatten()
            .is_some_and(|images| images.thumb_url.is_some())
    }
}

/// What the exact-id Wikidata step found for an artist.
enum WikidataPictures {
    /// No single item carries the MBID; `failed` when the search failed.
    NoMatch { failed: bool },
    /// The item's pictures, best first; `failed` when a lookup behind them
    /// failed.
    Matched { urls: Vec<String>, failed: bool },
}

/// Candidates for an artist MusicBrainz could not look up: none, asked
/// again soon.
fn musicbrainz_unavailable(mbid: &str, error: &CatalogError) -> ArtistImageCandidates {
    tracing::debug!(artist = mbid, %error, "artist image: MusicBrainz unavailable");
    ArtistImageCandidates {
        urls: Vec::new(),
        incomplete: true,
    }
}

/// The artwork service's artist image source.
pub struct CatalogArtistImages(pub Catalog);

impl ArtistImageSource for CatalogArtistImages {
    fn candidates<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: droppedneedle_providers::RequestPriority,
    ) -> futures_util::future::BoxFuture<'a, ArtistImageCandidates> {
        Box::pin(self.0.artist_image_candidates(artist_mbid, priority))
    }

    fn fill<'a>(&'a self, artist_mbid: &'a str) -> futures_util::future::BoxFuture<'a, bool> {
        Box::pin(self.0.fill_artist_images(artist_mbid))
    }
}

/// The library precache's lookups over the catalog and the artwork service.
pub struct CatalogPrecache {
    catalog: Catalog,
    artwork: ArtworkService,
    settings: Live<AdvancedSettings>,
}

impl CatalogPrecache {
    /// Bundle the catalog, the artwork service, and the advanced settings
    /// whose AudioDB switch is read per run.
    pub fn new(
        catalog: Catalog,
        artwork: ArtworkService,
        settings: Live<AdvancedSettings>,
    ) -> Self {
        Self {
            catalog,
            artwork,
            settings,
        }
    }
}

impl PrecacheSources for CatalogPrecache {
    fn tuning(&self) -> PrecacheTuning {
        // Unreadable settings never prewarm AudioDB, whose switch and key
        // live in this section.
        let settings = self
            .settings
            .read()
            .map_or_else(super::upstream::audiodb_off, |settings| {
                (**settings).clone()
            });
        PrecacheTuning {
            audiodb_enabled: settings.audiodb_enabled,
            ..PrecacheTuning::default()
        }
    }

    fn library(&self) -> BoxFuture<'_, Result<PrecacheLibrary, String>> {
        Box::pin(self.catalog.precache_library())
    }

    fn artist_cached<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.catalog.artist_page_cached(&artist.mbid).await
                && self.artwork.artist_image_cached(&artist.mbid).await
        })
    }

    fn warm_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.catalog.warm_artist_page(&artist.mbid).await;
            if !self.artwork.artist_image_cached(&artist.mbid).await {
                self.artwork.warm_artist_image(&artist.mbid).await;
            }
        })
    }

    fn warm_discovery<'a>(&'a self, user_id: &'a str, artist_mbid: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(self.catalog.warm_artist_discovery(user_id, artist_mbid))
    }

    fn album_cached<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.catalog
                .album_page_cached(&album.release_group_mbid)
                .await
                && self
                    .artwork
                    .release_group_cover_cached(&album.release_group_mbid)
                    .await
        })
    }

    fn warm_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let (_, _) = tokio::join!(
                self.catalog.warm_album_page(&album.release_group_mbid),
                self.artwork.warm_release_group_cover(
                    &album.release_group_mbid,
                    droppedneedle_providers::RequestPriority::BackgroundSync
                ),
            );
        })
    }

    fn audiodb_artist_cached<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, bool> {
        Box::pin(self.catalog.artist_images_known(&artist.mbid))
    }

    fn warm_audiodb_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if self
                .catalog
                .artist_images_lookup(&artist.mbid, &artist.name, BACKGROUND)
                .await
                .is_some()
                && !self.artwork.artist_image_cached(&artist.mbid).await
            {
                self.artwork.warm_artist_image(&artist.mbid).await;
            }
        })
    }

    fn audiodb_album_cached<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, bool> {
        Box::pin(self.artwork.audiodb_cover_cached(&album.release_group_mbid))
    }

    fn warm_audiodb_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(thumb) = self
                .catalog
                .album_audiodb_thumb(&album.release_group_mbid)
                .await
            {
                self.artwork
                    .store_audiodb_cover(&album.release_group_mbid, thumb.as_deref())
                    .await;
            }
        })
    }
}
