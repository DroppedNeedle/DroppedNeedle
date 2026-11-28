//! What the library precache and the artist images ask of the catalog.
//!
//! [`CatalogPrecache`] implements the precache's lookups over the catalog
//! (page data, discovery sections, TheAudioDB answers) and the artwork
//! service (artist images, covers). [`CatalogArtistImages`] names an
//! artist's image URLs for the artwork service. Every call goes through
//! the catalog's own caches and provider pacing, so a precache run fills
//! exactly what a page visit would, at the pace the providers allow.

use crate::artwork::{
    ArtistImageCandidate, ArtistImageCandidates, ArtistImageSource, ArtworkService,
};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::precache_phases::{
    LibraryAlbum, LibraryArtist, PrecacheLibrary, PrecacheSources, PrecacheTuning,
};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_kernel::runtime_config::sections::ListenSource;
use droppedneedle_media::playback::listening::ListeningStore;

use super::Catalog;
use super::error::CatalogError;
use crate::discover::facts::WantOrder;
use crate::discover::listening::similar;

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

    /// Identified artists and albums added or identified at or after
    /// `since_ms` (unix ms), for automatic post-scan precache passes.
    pub async fn precache_library_since(&self, since_ms: i64) -> Result<PrecacheLibrary, String> {
        let artists = self
            .local()
            .identified_artists_since(since_ms)
            .await
            .map_err(|error| format!("Could not read the library's artists: {error}"))?;
        let albums = self
            .local()
            .identified_albums_since(since_ms)
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

    /// Whether each artist's MusicBrainz page data (detail + release groups)
    /// is held, checked in two batched cache reads.
    async fn artist_pages_held(&self, artists: &[LibraryArtist]) -> Vec<bool> {
        let detail_keys: Vec<_> = artists
            .iter()
            .map(|artist| self.artist_detail_key(&artist.mbid))
            .collect();
        let group_keys: Vec<_> = artists
            .iter()
            .map(|artist| self.release_groups_key(&artist.mbid))
            .collect();
        let cache = self.upstream().cache();
        let details = cache.peek_many::<serde::de::IgnoredAny>(&detail_keys).await;
        let groups = cache.peek_many::<serde::de::IgnoredAny>(&group_keys).await;
        details
            .into_iter()
            .zip(groups)
            .map(|(d, g)| d.is_some() && g.is_some())
            .collect()
    }

    /// Whether each album's MusicBrainz release-group detail is held, checked
    /// in one batched cache read.
    async fn album_pages_held(&self, albums: &[LibraryAlbum]) -> Vec<bool> {
        let keys: Vec<_> = albums
            .iter()
            .map(|album| self.group_key(&album.release_group_mbid))
            .collect();
        self.upstream()
            .cache()
            .peek_many::<serde::de::IgnoredAny>(&keys)
            .await
            .into_iter()
            .map(|entry| entry.is_some())
            .collect()
    }

    /// Whether each artist's TheAudioDB answer is held, checked in one
    /// batched cache read.
    async fn audiodb_artists_held(&self, artists: &[LibraryArtist]) -> Vec<bool> {
        let keys: Vec<_> = artists
            .iter()
            .map(|artist| self.audiodb_artist_key(&artist.mbid))
            .collect();
        self.upstream()
            .cache()
            .peek_many::<serde::de::IgnoredAny>(&keys)
            .await
            .into_iter()
            .map(|entry| entry.is_some())
            .collect()
    }

    /// Whether the artist page's MusicBrainz data is held, fresh or stale:
    /// a visit refreshes a stale page, so the precache fills only what is
    /// missing.
    async fn artist_page_cached(&self, mbid: &str) -> bool {
        self.held::<super::artist::ArtistDetail>(&self.artist_detail_key(mbid))
            .await
            && self
                .held::<super::artist::ReleaseGroupList>(&self.release_groups_key(mbid))
                .await
    }

    /// Fetch and cache the artist page's data.
    async fn warm_artist_page(&self, mbid: &str) {
        if let Err(error) = self.build_artist(mbid, BACKGROUND).await {
            tracing::debug!(artist = mbid, %error, "precache: artist page not cached");
        }
    }

    /// Fetch and cache the artist's discovery sections as `user_id` sees
    /// them, from `source`; similar artists only `with_similar`, as
    /// ListenBrainz's lists are kept facts the refresh job reads.
    async fn warm_artist_discovery(
        &self,
        user_id: &str,
        mbid: &str,
        source: ListenSource,
        with_similar: bool,
    ) {
        let source = Some(source);
        let (similar, songs, albums) = tokio::join!(
            async {
                if !with_similar {
                    return Ok(());
                }
                self.similar_artists(user_id, mbid, SIMILAR_COUNT, source, BACKGROUND)
                    .await
                    .map(|_| ())
            },
            self.top_songs(user_id, mbid, TOP_SONGS_COUNT, source, BACKGROUND),
            self.top_albums(user_id, mbid, TOP_ALBUMS_COUNT, source, BACKGROUND),
        );
        if let Some(error) = [similar.err(), songs.err(), albums.err()]
            .into_iter()
            .flatten()
            .next()
        {
            tracing::debug!(artist = mbid, %error, "precache: discovery partly cached");
        }
    }

    /// Whether the artist's discovery sections are held, fresh or stale,
    /// as `access` reads them.
    async fn artist_discovery_cached(&self, access: RunAccess, mbid: &str) -> bool {
        self.artist_sections_held(
            access.lastfm,
            access.source() == ListenSource::Lastfm,
            mbid,
            (SIMILAR_COUNT, TOP_SONGS_COUNT, TOP_ALBUMS_COUNT),
        )
        .await
    }

    /// Whether the album page's MusicBrainz data is held, fresh or stale.
    async fn album_page_cached(&self, mbid: &str) -> bool {
        self.held::<super::album::GroupDetail>(&self.group_key(mbid))
            .await
    }

    /// Fetch and cache the album page's data.
    async fn warm_album_page(&self, mbid: &str) {
        if let Err(error) = self.build_album(mbid, BACKGROUND).await {
            tracing::debug!(album = mbid, %error, "precache: album page not cached");
        }
    }

    /// TheAudioDB thumbnail URL for a release group: `Some(None)` when it
    /// has none, `None` when the answer is not settled.
    async fn album_audiodb_thumb(&self, mbid: &str) -> Option<Option<String>> {
        // A held group, stale or not, names the album well enough.
        let held = self.upstream().cache().peek(&self.group_key(mbid)).await;
        let found = match held {
            Some(group) => Ok(group),
            None => self.group_detail_at(mbid, BACKGROUND).await,
        };
        let group = match found {
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
        if priority.is_user_facing() {
            let cached = self.page_artist_images(mbid, mbid).await;
            if let Some(thumb) = cached.and_then(|images| images.thumb_url) {
                candidates
                    .images
                    .push(ArtistImageCandidate::uncredited(thumb));
                return candidates;
            }
        } else {
            let advanced = self.upstream().settings().advanced();
            if self.upstream().audiodb(&advanced).is_some() {
                match self.artist_images_lookup(mbid, "", priority).await {
                    Some(images) => {
                        if let Some(thumb) = images.and_then(|images| images.thumb_url) {
                            candidates
                                .images
                                .push(ArtistImageCandidate::uncredited(thumb));
                            return candidates;
                        }
                    }
                    None => candidates.incomplete = true,
                }
            }
        }
        match self.wikidata_artist_pictures(mbid, priority).await {
            WikidataPictures::Matched { images, failed } => {
                candidates.images = images;
                candidates.incomplete |= failed;
                return candidates;
            }
            WikidataPictures::NoMatch { failed } => candidates.incomplete |= failed,
        }
        if !priority.is_user_facing() && !self.has_background_mb_budget().await {
            candidates.incomplete = true;
            return candidates;
        }
        let detail = match self.artist_detail(mbid, priority).await {
            Ok(Some(found)) => {
                // A merged MBID: TheAudioDB's answer is kept under the
                // artist's current one.
                if found.mbid != mbid
                    && let Some(thumb) = self
                        .cached_artist_images(&found.mbid)
                        .await
                        .and_then(|images| images.thumb_url)
                {
                    candidates
                        .images
                        .push(ArtistImageCandidate::uncredited(thumb));
                    return candidates;
                }
                found
            }
            Ok(None) => return candidates,
            Err(error) => return musicbrainz_unavailable(mbid, &error),
        };
        let biography = self.biography(&detail, priority).await;
        // A failed portrait lookup keeps the short outage marker.
        candidates.incomplete |= biography.image_failed;
        candidates
            .images
            .extend(biography.image.map(|url| ArtistImageCandidate {
                url,
                credit: biography.image_credit,
            }));
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
                    images: Vec::new(),
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
                        images: vec![ArtistImageCandidate::uncredited(url)],
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
        let mut images = Vec::new();
        if let Some(file) = &ids.commons_file {
            match wiki.commons_image(file, priority).await {
                Ok(image) => images.extend(image.map(ArtistImageCandidate::commons)),
                Err(error) => {
                    tracing::debug!(artist = mbid, %error, "artist image: Commons failed");
                    failed = true;
                }
            }
        }
        WikidataPictures::Matched { images, failed }
    }

    /// Fetch and cache TheAudioDB's answer for an artist at background
    /// priority. True when it names a thumbnail. Safe to repeat: a cached
    /// answer is not asked again, and MusicBrainz is only consulted when
    /// AudioDB is enabled and name-search fallback actually needs the name.
    pub async fn fill_artist_images(&self, mbid: &str) -> bool {
        let advanced = self.upstream().settings().advanced();
        if self.upstream().audiodb(&advanced).is_none() {
            return false;
        }
        let cache = self.upstream().cache();
        if cache.fresh(&self.audiodb_artist_key(mbid)).await {
            return self
                .cached_artist_images(mbid)
                .await
                .is_some_and(|images| images.thumb_url.is_some());
        }
        self.artist_images_lookup(mbid, "", BACKGROUND)
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
    Matched {
        images: Vec<ArtistImageCandidate>,
        failed: bool,
    },
}

/// Candidates for an artist MusicBrainz could not look up: none, asked
/// again soon.
fn musicbrainz_unavailable(mbid: &str, error: &CatalogError) -> ArtistImageCandidates {
    tracing::debug!(artist = mbid, %error, "artist image: MusicBrainz unavailable");
    ArtistImageCandidates {
        images: Vec::new(),
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
    listening: ListeningStore,
    /// What the run's user can read and follows, found once per run: it
    /// decrypts their key.
    access: tokio::sync::Mutex<Option<(String, RunAccess)>>,
}

/// What a precache run's user can read and follows.
#[derive(Debug, Clone, Copy)]
struct RunAccess {
    lastfm: bool,
    follows_lastfm: bool,
}

impl RunAccess {
    /// The service whose artist sections this user's pages ask for.
    fn source(self) -> ListenSource {
        if self.lastfm && self.follows_lastfm {
            ListenSource::Lastfm
        } else {
            ListenSource::Listenbrainz
        }
    }
}

impl CatalogPrecache {
    /// Bundle the catalog, the artwork service, the advanced settings
    /// whose AudioDB switch is read per run, and the store holding which
    /// service each user follows.
    pub fn new(
        catalog: Catalog,
        artwork: ArtworkService,
        settings: Live<AdvancedSettings>,
        listening: ListeningStore,
    ) -> Self {
        Self {
            catalog,
            artwork,
            settings,
            listening,
            access: tokio::sync::Mutex::new(None),
        }
    }

    /// What `user_id` can read and follows, asked once per run.
    async fn run_access(&self, user_id: &str) -> RunAccess {
        let mut known = self.access.lock().await;
        if let Some((user, access)) = known.as_ref()
            && user == user_id
        {
            return *access;
        }
        let access = RunAccess {
            lastfm: self.catalog.lastfm_readable(user_id).await,
            follows_lastfm: self.listening.prefs(user_id).await.primary_music_source
                == ListenSource::Lastfm,
        };
        *known = Some((user_id.to_owned(), access));
        access
    }

    /// Want `artist_mbid`'s similar-artist list and its neighbours' counts
    /// for the kept facts' refresh, which reads them under the daily
    /// ceiling: the precache itself reads no list. True when ListenBrainz
    /// answers the section, so nothing more is read for it here.
    async fn warm_similar(&self, artist_mbid: &str) -> bool {
        if self.catalog.upstream().listenbrainz().is_none() {
            return false;
        }
        let artist = [artist_mbid.to_owned()];
        let (facts, cache) = (self.catalog.facts(), self.catalog.upstream().cache());
        similar::want(facts, cache, &artist, WantOrder::Library).await;
        true
    }
}

/// Whether the configured AudioDB API key is a personal/paid key rather than
/// empty or one of TheAudioDB's shared public free keys (`1`, `2`, `123`).
fn has_custom_audiodb_key(settings: &AdvancedSettings) -> bool {
    let key = settings.audiodb_api_key.expose().trim();
    !key.is_empty() && !matches!(key, "1" | "2" | "123")
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
            audiodb_custom_key: has_custom_audiodb_key(&settings),
            ..PrecacheTuning::default()
        }
    }

    fn library(&self) -> BoxFuture<'_, Result<PrecacheLibrary, String>> {
        Box::pin(async move {
            // A run starts here: the user's access is asked again.
            *self.access.lock().await = None;
            self.catalog.precache_library().await
        })
    }

    fn library_since(&self, since_ms: i64) -> BoxFuture<'_, Result<PrecacheLibrary, String>> {
        Box::pin(async move {
            *self.access.lock().await = None;
            self.catalog.precache_library_since(since_ms).await
        })
    }

    fn artist_cached<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.catalog.artist_page_cached(&artist.mbid).await
                && self.artwork.artist_image_cached(&artist.mbid).await
        })
    }

    fn uncached_artists<'a>(&'a self, artists: &'a [LibraryArtist]) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            let pages = self.catalog.artist_pages_held(artists).await;
            let mut needed = Vec::new();
            for (index, (artist, page_held)) in artists.iter().zip(pages).enumerate() {
                if !page_held || !self.artwork.artist_image_cached(&artist.mbid).await {
                    needed.push(index);
                }
            }
            needed
        })
    }

    fn warm_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if !self.catalog.artist_page_cached(&artist.mbid).await {
                self.catalog.warm_artist_page(&artist.mbid).await;
            }
            if !self.artwork.artist_image_cached(&artist.mbid).await {
                self.artwork.warm_artist_image(&artist.mbid).await;
            }
        })
    }

    fn discovery_cached<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let access = self.run_access(user_id).await;
            self.catalog
                .artist_discovery_cached(access, artist_mbid)
                .await
        })
    }

    fn warm_discovery<'a>(&'a self, user_id: &'a str, artist_mbid: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let source = self.run_access(user_id).await.source();
            let similar_wanted = self.warm_similar(artist_mbid).await;
            self.catalog
                .warm_artist_discovery(user_id, artist_mbid, source, !similar_wanted)
                .await;
        })
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

    fn uncached_albums<'a>(&'a self, albums: &'a [LibraryAlbum]) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            let pages = self.catalog.album_pages_held(albums).await;
            let mut needed = Vec::new();
            for (index, (album, page_held)) in albums.iter().zip(pages).enumerate() {
                if !page_held
                    || !self
                        .artwork
                        .release_group_cover_cached(&album.release_group_mbid)
                        .await
                {
                    needed.push(index);
                }
            }
            needed
        })
    }

    fn warm_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let (_, _) = tokio::join!(
                async {
                    if !self
                        .catalog
                        .album_page_cached(&album.release_group_mbid)
                        .await
                    {
                        self.catalog
                            .warm_album_page(&album.release_group_mbid)
                            .await;
                    }
                },
                async {
                    if !self
                        .artwork
                        .release_group_cover_cached(&album.release_group_mbid)
                        .await
                    {
                        self.artwork
                            .warm_release_group_cover(
                                &album.release_group_mbid,
                                droppedneedle_providers::RequestPriority::BackgroundSync,
                            )
                            .await;
                    }
                },
            );
        })
    }

    fn audiodb_artist_cached<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, bool> {
        Box::pin(self.catalog.artist_images_known(&artist.mbid))
    }

    fn uncached_audiodb_artists<'a>(
        &'a self,
        artists: &'a [LibraryArtist],
    ) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            self.catalog
                .audiodb_artists_held(artists)
                .await
                .into_iter()
                .enumerate()
                .filter_map(|(index, held)| (!held).then_some(index))
                .collect()
        })
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
