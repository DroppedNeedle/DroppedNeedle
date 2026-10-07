//! What the library precache and the artist images ask of the catalog.
//!
//! [`CatalogPrecache`] implements the precache's lookups over the catalog
//! (page data, discovery sections, TheAudioDB answers) and the artwork
//! service (artist images, covers). [`CatalogArtistImages`] names an
//! artist's image URLs for the artwork service. Every call goes through
//! the catalog's own caches and provider pacing, so a precache run fills
//! exactly what a page visit would, at the pace the providers allow.

use std::sync::Arc;
use std::time::Duration;

use crate::jobs::precache_phases::{
    LibraryAlbum, LibraryArtist, PrecacheLibrary, PrecacheSources, PrecacheTuning,
};
use crate::jobs::registry::BoxFuture;
use crate::reads::platform::artwork::{ArtistImageCandidates, ArtistImageSource, ArtworkService};
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::AdvancedSettings;

use super::Catalog;

/// Discovery section sizes the precache warms: the page defaults, so the
/// cached answers are the ones an artist page asks for.
const SIMILAR_COUNT: u32 = 15;
const TOP_SONGS_COUNT: u32 = 10;
const TOP_ALBUMS_COUNT: u32 = 10;

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
        if let Err(error) = self.build_artist(mbid).await {
            tracing::debug!(artist = mbid, %error, "precache: artist page not cached");
        }
    }

    /// Fetch and cache the artist's discovery sections as `user_id` sees
    /// them.
    async fn warm_artist_discovery(&self, user_id: &str, mbid: &str) {
        let (similar, songs, albums) = tokio::join!(
            self.similar_artists(user_id, mbid, SIMILAR_COUNT, None),
            self.top_songs(user_id, mbid, TOP_SONGS_COUNT, None),
            self.top_albums(user_id, mbid, TOP_ALBUMS_COUNT, None),
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
        self.album_images_lookup(&group)
            .await
            .map(|images| images.and_then(|images| images.album_thumb_url))
    }

    /// Candidate image URLs for an artist: TheAudioDB's thumbnail, else
    /// the Wikidata portrait.
    pub async fn artist_image_candidates(&self, mbid: &str) -> ArtistImageCandidates {
        let detail = match self.artist_detail(mbid).await {
            Ok(Some(detail)) => detail,
            Ok(None) => return ArtistImageCandidates::default(),
            Err(error) => {
                tracing::debug!(artist = mbid, %error, "artist image: MusicBrainz unavailable");
                return ArtistImageCandidates {
                    urls: Vec::new(),
                    incomplete: true,
                };
            }
        };
        let mut candidates = ArtistImageCandidates::default();
        match self.artist_images_lookup(&detail.mbid, &detail.name).await {
            Some(images) => {
                if let Some(thumb) = images.and_then(|images| images.thumb_url) {
                    candidates.urls.push(thumb);
                }
            }
            None => candidates.incomplete = true,
        }
        if candidates.urls.is_empty()
            && let Some(portrait) = self.biography(&detail).await.image
        {
            candidates.urls.push(portrait);
        }
        candidates
    }
}

/// The artwork service's artist image source.
pub struct CatalogArtistImages(pub Catalog);

impl ArtistImageSource for CatalogArtistImages {
    fn candidates<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, ArtistImageCandidates> {
        Box::pin(self.0.artist_image_candidates(artist_mbid))
    }
}

/// The library precache's lookups over the catalog and the artwork service.
pub struct CatalogPrecache {
    catalog: Catalog,
    artwork: ArtworkService,
    config: Arc<ConfigStore>,
}

impl CatalogPrecache {
    /// Bundle the catalog, the artwork service, and the settings store.
    pub fn new(catalog: Catalog, artwork: ArtworkService, config: Arc<ConfigStore>) -> Self {
        Self {
            catalog,
            artwork,
            config,
        }
    }
}

fn clamp_workers(value: i64) -> usize {
    usize::try_from(value.clamp(1, 8)).unwrap_or(1)
}

fn clamp_batch(value: i64) -> usize {
    usize::try_from(value.clamp(1, 20)).unwrap_or(1)
}

fn seconds(value: f64) -> Duration {
    Duration::from_secs_f64(value.clamp(0.0, 5.0))
}

impl PrecacheSources for CatalogPrecache {
    fn tuning(&self) -> PrecacheTuning {
        let settings = self
            .config
            .get_raw::<AdvancedSettings>()
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read advanced settings; precache uses defaults");
                AdvancedSettings::default()
            });
        PrecacheTuning {
            artist_batch: clamp_batch(settings.batch_artist_images),
            artist_delay: seconds(settings.delay_artist),
            album_batch: clamp_batch(settings.batch_albums),
            album_delay: seconds(settings.delay_albums),
            discovery_workers: clamp_workers(settings.artist_discovery_precache_concurrency),
            discovery_delay: seconds(settings.artist_discovery_precache_delay),
            audiodb_enabled: settings.audiodb_enabled,
            audiodb_workers: clamp_workers(settings.audiodb_prewarm_concurrency),
            audiodb_delay: seconds(settings.audiodb_prewarm_delay),
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
                self.artwork
                    .warm_release_group_cover(&album.release_group_mbid),
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
                .artist_images_lookup(&artist.mbid, &artist.name)
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
