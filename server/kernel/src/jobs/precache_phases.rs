//! The library precache phases, in v2's order:
//!
//! 1. `artists`: each library artist's page data and image.
//! 2. `discovery`: similar artists, top songs and top albums for each
//!    artist, as the admin who started the run would see them. Skipped
//!    when nobody started it (no user to read the discovery sources as).
//! 3. `albums`: each library album's page data and its 500 px cover.
//! 4. `audiodb_prewarm`: TheAudioDB artwork for artists and albums whose
//!    AudioDB answer is not cached yet. Skipped while AudioDB is off.
//!
//! Every phase first drops the items already cached, fresh or stale: a
//! person's visit refreshes a stale answer, so a run after a scan fetches
//! only what is new, and a rerun after a failure or a cancel picks up where
//! the last one stopped. A phase with
//! nothing left reports zero items, which the progress pill shows as
//! skipped. Pacing follows the advanced settings, re-read per phase: batch
//! sizes and delays for artists and albums (the album batch adapts to how
//! fast the providers answer, as in v2), and a worker count plus a delay
//! for discovery and AudioDB. The provider clients add their own rate
//! limits on top, so a run never outpaces what an upstream allows.
//!
//! The lookups themselves sit behind [`PrecacheSources`]; the catalog crate
//! implements it over its pages and the artwork service.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::future::join_all;
use futures_util::stream;

use super::BoxFuture;
use super::cache_sync::CacheSyncStatus;
use super::precache::{PrecacheOutcome, PrecacheWork, Progress};

/// Cache checks between two watchdog beats while a phase sorts out what
/// still needs fetching.
pub const CHECK_BEAT_EVERY: usize = 25;
/// Cache keys checked per batched lookup chunk.
pub const CHECK_BATCH_SIZE: usize = 500;

/// One library artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryArtist {
    /// MusicBrainz artist id, lowercase.
    pub mbid: String,
    /// Display name.
    pub name: String,
}

/// One library album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryAlbum {
    /// MusicBrainz release-group id, lowercase.
    pub release_group_mbid: String,
    /// Album title.
    pub title: String,
    /// Album artist name.
    pub artist_name: String,
}

/// What the library holds, identified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrecacheLibrary {
    /// Artists with a MusicBrainz id.
    pub artists: Vec<LibraryArtist>,
    /// Albums with a release-group id.
    pub albums: Vec<LibraryAlbum>,
}

/// Pacing for one run. Production uses the defaults, with the AudioDB
/// switch from the advanced settings; tests pass their own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrecacheTuning {
    /// Artists warmed at once.
    pub artist_batch: usize,
    /// Pause after each artist batch.
    pub artist_delay: Duration,
    /// Starting album batch.
    pub album_batch: usize,
    /// Pause after each album batch.
    pub album_delay: Duration,
    /// Discovery workers.
    pub discovery_workers: usize,
    /// Pause after each discovery artist.
    pub discovery_delay: Duration,
    /// Whether TheAudioDB is on (`audiodb_enabled`).
    pub audiodb_enabled: bool,
    /// Whether the admin configured a personal/paid AudioDB API key (rather
    /// than the default/shared free key). Automatic post-scan precache skips
    /// bulk AudioDB prewarming on the shared free key.
    pub audiodb_custom_key: bool,
    /// AudioDB workers.
    pub audiodb_workers: usize,
    /// Pause before each AudioDB lookup.
    pub audiodb_delay: Duration,
}

/// The shipped pacing. It is fixed, not a setting: the provider rate
/// limits pace the outbound calls, and these only keep a run from flooding
/// them.
impl Default for PrecacheTuning {
    fn default() -> Self {
        Self {
            artist_batch: 10,
            artist_delay: Duration::from_millis(500),
            album_batch: 8,
            album_delay: Duration::from_millis(300),
            discovery_workers: 5,
            discovery_delay: Duration::from_millis(200),
            audiodb_enabled: true,
            audiodb_custom_key: true,
            audiodb_workers: 4,
            audiodb_delay: Duration::from_millis(300),
        }
    }
}

/// The lookups behind the phases. Every warm call fails soft: a source
/// that cannot answer is logged by the implementation and the run moves on.
pub trait PrecacheSources: Send + Sync + 'static {
    /// Pacing for the next run, with the AudioDB switch read on every call.
    fn tuning(&self) -> PrecacheTuning;
    /// The library's identified artists and albums.
    fn library(&self) -> BoxFuture<'_, Result<PrecacheLibrary, String>>;
    /// Identified artists and albums added or identified at or after
    /// `since_ms` (unix ms), for automatic post-scan precache passes.
    fn library_since(&self, _since_ms: i64) -> BoxFuture<'_, Result<PrecacheLibrary, String>> {
        self.library()
    }
    /// Whether the artist's page data and image are both cached.
    fn artist_cached<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, bool>;
    /// Indices into `artists` whose page data or image still needs warming.
    /// Production overrides this to batch provider-cache checks.
    fn uncached_artists<'a>(&'a self, artists: &'a [LibraryArtist]) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            let mut needed = Vec::new();
            for (index, artist) in artists.iter().enumerate() {
                if !self.artist_cached(artist).await {
                    needed.push(index);
                }
            }
            needed
        })
    }
    /// Fetch and cache the artist's page data and image.
    fn warm_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()>;
    /// Whether the artist's discovery sections are cached as `user_id`
    /// sees them.
    fn discovery_cached<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, bool>;
    /// Indices into `artists` whose discovery sections still need warming.
    fn uncached_discovery<'a>(
        &'a self,
        user_id: &'a str,
        artists: &'a [LibraryArtist],
    ) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            let mut needed = Vec::new();
            for (index, artist) in artists.iter().enumerate() {
                if !self.discovery_cached(user_id, &artist.mbid).await {
                    needed.push(index);
                }
            }
            needed
        })
    }
    /// Fetch and cache the artist's discovery sections as `user_id` sees
    /// them.
    fn warm_discovery<'a>(&'a self, user_id: &'a str, artist_mbid: &'a str) -> BoxFuture<'a, ()>;
    /// Whether the album's page data and cover are both cached.
    fn album_cached<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, bool>;
    /// Indices into `albums` whose page data or cover still needs warming.
    fn uncached_albums<'a>(&'a self, albums: &'a [LibraryAlbum]) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            let mut needed = Vec::new();
            for (index, album) in albums.iter().enumerate() {
                if !self.album_cached(album).await {
                    needed.push(index);
                }
            }
            needed
        })
    }
    /// Fetch and cache the album's page data and cover.
    fn warm_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()>;
    /// Whether TheAudioDB's answer for the artist is cached.
    fn audiodb_artist_cached<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, bool>;
    /// Indices into `artists` whose AudioDB answer is not cached yet.
    fn uncached_audiodb_artists<'a>(
        &'a self,
        artists: &'a [LibraryArtist],
    ) -> BoxFuture<'a, Vec<usize>> {
        Box::pin(async move {
            let mut needed = Vec::new();
            for (index, artist) in artists.iter().enumerate() {
                if !self.audiodb_artist_cached(artist).await {
                    needed.push(index);
                }
            }
            needed
        })
    }
    /// Ask TheAudioDB about the artist and cache the answer and image.
    fn warm_audiodb_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()>;
    /// Whether TheAudioDB's answer for the album is cached.
    fn audiodb_album_cached<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, bool>;
    /// Ask TheAudioDB about the album and cache the answer and thumbnail.
    fn warm_audiodb_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()>;
}

/// One precache run over the library.
pub struct LibraryPrecache {
    sources: Arc<dyn PrecacheSources>,
    status: CacheSyncStatus,
    user_id: Option<String>,
    since_ms: Option<i64>,
    generation: Arc<Mutex<Option<u64>>>,
}

impl LibraryPrecache {
    /// A full-library run reporting to `status`. `user_id` is whoever started
    /// it; the discovery phase reads as them and is skipped without one.
    pub fn new(
        sources: Arc<dyn PrecacheSources>,
        status: CacheSyncStatus,
        user_id: Option<String>,
    ) -> Self {
        Self {
            sources,
            status,
            user_id,
            since_ms: None,
            generation: Arc::new(Mutex::new(None)),
        }
    }

    /// An automatic post-scan run scoped only to artists and albums added or
    /// identified at or after `since_ms` (unix ms).
    pub fn new_since(
        sources: Arc<dyn PrecacheSources>,
        status: CacheSyncStatus,
        since_ms: i64,
    ) -> Self {
        Self {
            sources,
            status,
            user_id: None,
            since_ms: Some(since_ms),
            generation: Arc::new(Mutex::new(None)),
        }
    }

    /// A handle that settles this run's status once the supervisor says
    /// how it ended.
    pub fn settler(&self) -> Settler {
        Settler {
            status: self.status.clone(),
            generation: Arc::clone(&self.generation),
        }
    }

    async fn run_phases(&self, progress: Progress) -> Result<(), String> {
        let library = match self.since_ms {
            Some(since_ms) => self.sources.library_since(since_ms).await?,
            None => self.sources.library().await?,
        };
        let artists = dedupe(library.artists, |artist| artist.mbid.clone());
        let albums = dedupe(library.albums, |album| album.release_group_mbid.clone());
        let generation = self
            .status
            .start("artists", artists.len() as u64, albums.len() as u64);
        *self
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(generation);
        let run = Run {
            sources: self.sources.as_ref(),
            status: &self.status,
            progress: &progress,
            generation,
            automatic: self.since_ms.is_some(),
        };
        run.artists(&artists).await;
        match &self.user_id {
            Some(user_id) => run.discovery(user_id, &artists).await,
            None => run.skip("discovery"),
        }
        run.albums(&albums).await;
        run.audiodb(&artists, &albums).await;
        Ok(())
    }
}

impl PrecacheWork for LibraryPrecache {
    fn run(&self, progress: Progress) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(self.run_phases(progress))
    }
}

/// Settles a run's status after the supervisor reports.
#[derive(Clone)]
pub struct Settler {
    status: CacheSyncStatus,
    generation: Arc<Mutex<Option<u64>>>,
}

impl Settler {
    /// Idle on success or cancel, idle with the reason on failure. A run
    /// cancelled from the route already moved the status on, so this is
    /// then a no-op.
    pub fn settle(&self, outcome: &PrecacheOutcome) {
        let error = match outcome {
            PrecacheOutcome::Done | PrecacheOutcome::Cancelled => None,
            PrecacheOutcome::Watchdog(reason) | PrecacheOutcome::Failed(reason) => {
                Some(reason.clone())
            }
        };
        let current = *self
            .generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(current) = current {
            self.status.finish(current, error);
        }
    }
}

/// Keep the first of each id, in order.
fn dedupe<T>(items: Vec<T>, id: impl Fn(&T) -> String) -> Vec<T> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(id(item)))
        .collect()
}

/// One run's shared handles.
struct Run<'a> {
    sources: &'a dyn PrecacheSources,
    status: &'a CacheSyncStatus,
    progress: &'a Progress,
    generation: u64,
    automatic: bool,
}

impl Run<'_> {
    /// Count one cache check. A large, fully cached library takes a while
    /// to check, so every [`CHECK_BEAT_EVERY`] checks count as progress for
    /// the stall watchdog.
    fn checked(&self, index: usize) {
        if index.is_multiple_of(CHECK_BEAT_EVERY) {
            self.progress.beat(None);
        }
    }

    fn audiodb_allowed(&self, tuning: PrecacheTuning) -> bool {
        tuning.audiodb_enabled && (!self.automatic || tuning.audiodb_custom_key)
    }

    fn skip(&self, phase: &str) {
        self.progress.beat(Some(phase));
        self.status.phase(self.generation, phase, 0);
    }

    fn enter(&self, phase: &str, total: usize) {
        self.progress.beat(Some(phase));
        self.status.phase(self.generation, phase, total as u64);
    }

    async fn artists(&self, artists: &[LibraryArtist]) {
        let mut needed = Vec::new();
        for chunk in artists.chunks(CHECK_BATCH_SIZE) {
            self.progress.beat(None);
            for index in self.sources.uncached_artists(chunk).await {
                if let Some(artist) = chunk.get(index) {
                    needed.push(artist);
                }
            }
        }
        if needed.is_empty() {
            self.status.progress(
                self.generation,
                0,
                "Artists already cached",
                Some(artists.len() as u64),
                None,
            );
            return self.skip("artists");
        }
        self.enter("artists", needed.len());
        let cached = (artists.len() - needed.len()) as u64;
        let done = &AtomicU64::new(0);
        let tuning = self.sources.tuning();
        for batch in needed.chunks(tuning.artist_batch.max(1)) {
            join_all(batch.iter().map(|artist| async move {
                self.sources.warm_artist(artist).await;
                let count = done.fetch_add(1, Ordering::Relaxed) + 1;
                self.progress.beat(None);
                self.status.progress(
                    self.generation,
                    count,
                    &artist.name,
                    Some(cached + count),
                    None,
                );
            }))
            .await;
            tokio::time::sleep(tuning.artist_delay).await;
        }
    }

    async fn discovery(&self, user_id: &str, artists: &[LibraryArtist]) {
        let mut needed = Vec::new();
        for chunk in artists.chunks(CHECK_BATCH_SIZE) {
            self.progress.beat(None);
            for index in self.sources.uncached_discovery(user_id, chunk).await {
                if let Some(artist) = chunk.get(index) {
                    needed.push(artist);
                }
            }
        }
        if needed.is_empty() {
            return self.skip("discovery");
        }
        self.enter("discovery", needed.len());
        let tuning = self.sources.tuning();
        let done = &AtomicU64::new(0);
        stream::iter(needed)
            .for_each_concurrent(tuning.discovery_workers.max(1), |artist| async move {
                self.sources.warm_discovery(user_id, &artist.mbid).await;
                let count = done.fetch_add(1, Ordering::Relaxed) + 1;
                self.progress.beat(None);
                self.status
                    .progress(self.generation, count, &artist.name, None, None);
                tokio::time::sleep(tuning.discovery_delay).await;
            })
            .await;
    }

    async fn albums(&self, albums: &[LibraryAlbum]) {
        let mut needed = Vec::new();
        for chunk in albums.chunks(CHECK_BATCH_SIZE) {
            self.progress.beat(None);
            for index in self.sources.uncached_albums(chunk).await {
                if let Some(album) = chunk.get(index) {
                    needed.push(album);
                }
            }
        }
        if needed.is_empty() {
            self.status.progress(
                self.generation,
                0,
                "Albums already cached",
                None,
                Some(albums.len() as u64),
            );
            return self.skip("albums");
        }
        self.enter("albums", needed.len());
        let cached = (albums.len() - needed.len()) as u64;
        let tuning = self.sources.tuning();
        let mut batch = AdaptiveBatch::new(tuning.album_batch);
        let done = &AtomicU64::new(0);
        let mut start = 0;
        while start < needed.len() {
            let end = (start + batch.size).min(needed.len());
            let slice = needed.get(start..end).unwrap_or_default();
            let began = Instant::now();
            join_all(slice.iter().map(|album| async move {
                self.sources.warm_album(album).await;
                let count = done.fetch_add(1, Ordering::Relaxed) + 1;
                self.progress.beat(None);
                self.status.progress(
                    self.generation,
                    count,
                    &format!("{} - {}", album.artist_name, album.title),
                    None,
                    Some(cached + count),
                );
            }))
            .await;
            let per_item = began.elapsed().as_secs_f64() / slice.len().max(1) as f64;
            batch.observe(per_item);
            start = end;
            tokio::time::sleep(tuning.album_delay).await;
        }
    }

    async fn audiodb(&self, artists: &[LibraryArtist], albums: &[LibraryAlbum]) {
        if !self.audiodb_allowed(self.sources.tuning()) {
            return self.skip("audiodb_prewarm");
        }
        let mut needed_artists = Vec::new();
        for chunk in artists.chunks(CHECK_BATCH_SIZE) {
            self.progress.beat(None);
            for index in self.sources.uncached_audiodb_artists(chunk).await {
                if let Some(artist) = chunk.get(index) {
                    needed_artists.push(artist);
                }
            }
        }
        let mut needed_albums = Vec::new();
        for (index, album) in albums.iter().enumerate() {
            self.checked(index);
            if !self.sources.audiodb_album_cached(album).await {
                needed_albums.push(album);
            }
        }
        let total = needed_artists.len() + needed_albums.len();
        if total == 0 {
            return self.skip("audiodb_prewarm");
        }
        self.enter("audiodb_prewarm", total);
        let done = &AtomicU64::new(0);
        let workers = self.sources.tuning().audiodb_workers.max(1);
        let step = &|label: String| {
            let count = done.fetch_add(1, Ordering::Relaxed) + 1;
            self.progress.beat(None);
            self.status.progress(
                self.generation,
                count,
                &format!("AudioDB: {label}"),
                None,
                None,
            );
        };
        stream::iter(needed_artists)
            .for_each_concurrent(workers, |artist| async move {
                // Re-read per item: turning AudioDB off stops the phase.
                let tuning = self.sources.tuning();
                if self.audiodb_allowed(tuning) {
                    tokio::time::sleep(tuning.audiodb_delay).await;
                    self.sources.warm_audiodb_artist(artist).await;
                }
                step(artist.name.clone());
            })
            .await;
        stream::iter(needed_albums)
            .for_each_concurrent(workers, |album| async move {
                let tuning = self.sources.tuning();
                if self.audiodb_allowed(tuning) {
                    tokio::time::sleep(tuning.audiodb_delay).await;
                    self.sources.warm_audiodb_album(album).await;
                }
                step(album.title.clone());
            })
            .await;
    }
}

/// Album batch size that follows provider speed (v2 `AlbumPhase`): a slow
/// batch (over 1.5 s per item) shrinks it by one, three slow batches in a
/// row by two; a fast one (under 0.8 s per item) grows it by one. It stays
/// between two under the configured size and twelve over it, capped at 20.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdaptiveBatch {
    size: usize,
    min: usize,
    max: usize,
    slow_streak: u32,
}

impl AdaptiveBatch {
    fn new(configured: usize) -> Self {
        let configured = configured.max(1);
        Self {
            size: configured,
            min: configured.saturating_sub(2).max(1),
            max: (configured + 12).min(20).max(configured),
            slow_streak: 0,
        }
    }

    fn observe(&mut self, secs_per_item: f64) {
        if secs_per_item > 1.5 {
            self.slow_streak += 1;
            let step = if self.slow_streak >= 3 { 2 } else { 1 };
            self.size = self.size.saturating_sub(step).max(self.min);
        } else {
            self.slow_streak = 0;
            if secs_per_item < 0.8 {
                self.size = (self.size + 1).min(self.max);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::events::EventHub;
    use crate::jobs::precache::PrecacheWork;

    struct CountingSources {
        custom_key: bool,
        calls: Mutex<Vec<String>>,
    }

    impl CountingSources {
        fn new(custom_key: bool) -> Self {
            Self {
                custom_key,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn push(&self, entry: String) {
            self.calls.lock().unwrap().push(entry);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl PrecacheSources for CountingSources {
        fn tuning(&self) -> PrecacheTuning {
            PrecacheTuning {
                artist_delay: Duration::ZERO,
                album_delay: Duration::ZERO,
                discovery_delay: Duration::ZERO,
                audiodb_delay: Duration::ZERO,
                audiodb_enabled: true,
                audiodb_custom_key: self.custom_key,
                ..PrecacheTuning::default()
            }
        }

        fn library(&self) -> BoxFuture<'_, Result<PrecacheLibrary, String>> {
            Box::pin(async {
                self.push("library:full".to_owned());
                Ok(PrecacheLibrary {
                    artists: vec![
                        LibraryArtist {
                            mbid: "a-old".to_owned(),
                            name: "Old".to_owned(),
                        },
                        LibraryArtist {
                            mbid: "a-new".to_owned(),
                            name: "New".to_owned(),
                        },
                    ],
                    albums: vec![
                        LibraryAlbum {
                            release_group_mbid: "rg-old".to_owned(),
                            title: "Old".to_owned(),
                            artist_name: "Old".to_owned(),
                        },
                        LibraryAlbum {
                            release_group_mbid: "rg-new".to_owned(),
                            title: "New".to_owned(),
                            artist_name: "New".to_owned(),
                        },
                    ],
                })
            })
        }

        fn library_since(&self, since_ms: i64) -> BoxFuture<'_, Result<PrecacheLibrary, String>> {
            Box::pin(async move {
                self.push(format!("library:since:{since_ms}"));
                Ok(PrecacheLibrary {
                    artists: vec![LibraryArtist {
                        mbid: "a-new".to_owned(),
                        name: "New".to_owned(),
                    }],
                    albums: vec![LibraryAlbum {
                        release_group_mbid: "rg-new".to_owned(),
                        title: "New".to_owned(),
                        artist_name: "New".to_owned(),
                    }],
                })
            })
        }

        fn artist_cached<'a>(&'a self, _artist: &'a LibraryArtist) -> BoxFuture<'a, bool> {
            Box::pin(async { false })
        }

        fn warm_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()> {
            Box::pin(async move { self.push(format!("artist:{}", artist.mbid)) })
        }

        fn discovery_cached<'a>(&'a self, _u: &'a str, _a: &'a str) -> BoxFuture<'a, bool> {
            Box::pin(async { true })
        }

        fn warm_discovery<'a>(&'a self, _u: &'a str, _a: &'a str) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }

        fn album_cached<'a>(&'a self, _album: &'a LibraryAlbum) -> BoxFuture<'a, bool> {
            Box::pin(async { false })
        }

        fn warm_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()> {
            Box::pin(async move { self.push(format!("album:{}", album.release_group_mbid)) })
        }

        fn audiodb_artist_cached<'a>(&'a self, _artist: &'a LibraryArtist) -> BoxFuture<'a, bool> {
            Box::pin(async { false })
        }

        fn warm_audiodb_artist<'a>(&'a self, artist: &'a LibraryArtist) -> BoxFuture<'a, ()> {
            Box::pin(async move { self.push(format!("audiodb-artist:{}", artist.mbid)) })
        }

        fn audiodb_album_cached<'a>(&'a self, _album: &'a LibraryAlbum) -> BoxFuture<'a, bool> {
            Box::pin(async { false })
        }

        fn warm_audiodb_album<'a>(&'a self, album: &'a LibraryAlbum) -> BoxFuture<'a, ()> {
            Box::pin(
                async move { self.push(format!("audiodb-album:{}", album.release_group_mbid)) },
            )
        }
    }

    #[tokio::test]
    async fn automatic_precache_scopes_to_new_items_and_skips_shared_audiodb_key() {
        let shared = Arc::new(CountingSources::new(false));
        let status = CacheSyncStatus::new(EventHub::new(), Arc::new(SystemClock));
        let work = LibraryPrecache::new_since(shared.clone(), status, 4_000);
        work.run(Progress::new("start")).await.unwrap();
        let calls = shared.calls();
        assert!(
            calls.contains(&"library:since:4000".to_owned()),
            "{calls:?}"
        );
        assert!(calls.contains(&"artist:a-new".to_owned()), "{calls:?}");
        assert!(calls.contains(&"album:rg-new".to_owned()), "{calls:?}");
        assert!(!calls.contains(&"artist:a-old".to_owned()), "{calls:?}");
        assert!(!calls.contains(&"album:rg-old".to_owned()), "{calls:?}");
        assert!(
            !calls.iter().any(|c| c.starts_with("audiodb-")),
            "automatic precache must skip bulk AudioDB on shared free key: {calls:?}"
        );

        let custom = Arc::new(CountingSources::new(true));
        let status = CacheSyncStatus::new(EventHub::new(), Arc::new(SystemClock));
        let work = LibraryPrecache::new_since(custom.clone(), status, 4_000);
        work.run(Progress::new("start")).await.unwrap();
        let calls = custom.calls();
        assert!(
            calls.contains(&"audiodb-artist:a-new".to_owned())
                && calls.contains(&"audiodb-album:rg-new".to_owned()),
            "custom AudioDB key should still run AudioDB prewarm: {calls:?}"
        );

        let manual = Arc::new(CountingSources::new(false));
        let status = CacheSyncStatus::new(EventHub::new(), Arc::new(SystemClock));
        let work = LibraryPrecache::new(manual.clone(), status, None);
        work.run(Progress::new("start")).await.unwrap();
        let calls = manual.calls();
        assert!(calls.contains(&"library:full".to_owned()), "{calls:?}");
        assert!(calls.contains(&"artist:a-old".to_owned()), "{calls:?}");
        assert!(calls.contains(&"album:rg-old".to_owned()), "{calls:?}");
    }
}
