//! Fake discover ports: deterministic content, charts, previews, and stores
//! over in-memory state. Production still runs these; real providers
//! belong behind the same traits, and handlers never know.
//!
//! Fakes fail only when armed to: `fail_content` / `fail_charts` take a
//! log-only cause the leak tests assert never reaches the wire.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crate::reads::discover::{
    models::{
        ChartAlbum, ChartArtist, ChartRange, ChartSection, ChartSource, DiscoverActivityResponse,
        DiscoverQueuePreview, DiscoverQueueStatusResponse, DiscoverResponse, GenreArtwork,
        GenreArtworkAlbum, GenreDetailResponse, GenreLibrarySection, GenrePopularSection,
        HomeResponse, IgnoredRelease, IntegrationStatus, NowPlayingEntry, PopularAlbumsPage,
        QueueEnrichment, QueueGenerateResponse, QueueIgnoreRequest, SectionItem, ServicePrompt,
        TopPicksSection, TrackPreviewResponse, TrendingArtistsPage, WeeklyExploration,
        YouTubeQuotaResponse,
    },
    ports::{
        AlbumSamples, BatchItemRow, BatchRow, BatchStore, BoxFuture, ChartsSource, Clock,
        DiscoverContent, NowPlayingStore, PreviewSource, ProviderFailure, QueueDeck, QueueStore,
        QueueTrigger, RadioPlanner, YouTubeSource,
    },
};

/// Manual clock for loop and TTL tests. Starts at `start`, moves only when
/// told.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<i64>>,
}

impl ManualClock {
    /// Build a clock pinned at `start` (unix seconds).
    pub fn new(start: i64) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> i64 {
        self.now.lock().map(|now| *now).unwrap_or(0)
    }
}

fn fake_artist(name: &str, rank: i64) -> ChartArtist {
    ChartArtist {
        name: name.to_owned(),
        mbid: Some(format!("artist-mbid-{rank}")),
        local_id: None,
        image_url: Some(format!("https://art.example.invalid/a{rank}.jpg")),
        listen_count: Some(10_000 - rank * 100),
        in_library: rank % 3 == 0,
        source: Some("listenbrainz".to_owned()),
    }
}

fn fake_album(name: &str, artist: &str, rank: i64) -> ChartAlbum {
    ChartAlbum {
        name: name.to_owned(),
        mbid: Some(format!("rg-mbid-{rank}")),
        local_id: if rank % 2 == 0 {
            Some(format!("local-{rank}"))
        } else {
            None
        },
        artist_name: Some(artist.to_owned()),
        artist_mbid: Some(format!("artist-mbid-{rank}")),
        image_url: Some(format!("https://art.example.invalid/b{rank}.jpg")),
        release_date: Some("2025-01-01".to_owned()),
        listen_count: Some(5_000 - rank * 50),
        in_library: rank % 2 == 0,
        requested: false,
        source: Some("listenbrainz".to_owned()),
    }
}

fn album_section(title: &str, count: i64) -> ChartSection {
    ChartSection {
        title: title.to_owned(),
        section_type: "album".to_owned(),
        items: (0..count)
            .map(|rank| {
                SectionItem::Album(fake_album(
                    &format!("Album {rank}"),
                    &format!("Artist {rank}"),
                    rank,
                ))
            })
            .collect(),
        source: Some("listenbrainz".to_owned()),
        fallback_message: None,
        connect_service: None,
        radio_seed_type: Some("genre".to_owned()),
        radio_seed_id: Some("trip-hop".to_owned()),
    }
}

fn artist_section(title: &str, count: i64) -> ChartSection {
    ChartSection {
        title: title.to_owned(),
        section_type: "artist".to_owned(),
        items: (0..count)
            .map(|rank| SectionItem::Artist(fake_artist(&format!("Artist {rank}"), rank)))
            .collect(),
        source: Some("listenbrainz".to_owned()),
        fallback_message: None,
        connect_service: None,
        radio_seed_type: Some("artist".to_owned()),
        radio_seed_id: Some(format!("artist-mbid-{count}")),
    }
}

fn fake_integration_status() -> IntegrationStatus {
    IntegrationStatus {
        listenbrainz: true,
        jellyfin: false,
        download_client: true,
        youtube: true,
        lastfm: true,
        navidrome: false,
        youtube_api: true,
        plex: false,
        library: true,
        localfiles: true,
    }
}

/// Scripted shelf content plus queue/batch-adjacent reads.
pub struct FakeContent {
    clock: ManualClock,
    fail_with: Mutex<Option<String>>,
    refreshes: Mutex<Vec<String>>,
    activities: Mutex<Vec<(String, String)>>,
}

impl FakeContent {
    /// Build scripted content pinned to `clock`.
    pub fn new(clock: ManualClock) -> Self {
        Self {
            clock,
            fail_with: Mutex::new(None),
            refreshes: Mutex::new(Vec::new()),
            activities: Mutex::new(Vec::new()),
        }
    }

    /// Arm every fallible read to fail with `cause` (log-only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_content(&self, cause: &str) {
        if let Ok(mut fail) = self.fail_with.lock() {
            *fail = Some(cause.to_owned());
        }
    }

    /// Users a refresh was triggered for, in order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn refreshes_for(&self) -> Vec<String> {
        self.refreshes
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default()
    }

    /// Recorded (user, feature) activity pairs, in order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn activities(&self) -> Vec<(String, String)> {
        self.activities
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default()
    }

    fn failure(&self) -> Option<ProviderFailure> {
        self.fail_with
            .lock()
            .ok()
            .and_then(|fail| fail.clone())
            .map(ProviderFailure::Failed)
    }
}

impl DiscoverContent for FakeContent {
    fn discover<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverResponse, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            Ok(DiscoverResponse {
                because_you_listen_to: vec![crate::reads::discover::models::BecauseYouListenTo {
                    seed_artist: "Portishead".to_owned(),
                    seed_artist_mbid: "artist-mbid-0".to_owned(),
                    section: album_section("Because you listen to Portishead", 4),
                    listen_count: 120,
                    banner_url: None,
                    wide_thumb_url: None,
                    fanart_url: None,
                }],
                discover_queue_enabled: true,
                fresh_releases: Some(album_section("Fresh releases", 4)),
                missing_essentials: None,
                rediscover: None,
                artists_you_might_like: Some(artist_section("Artists you might like", 4)),
                popular_in_your_genres: None,
                genre_list: None,
                globally_trending: Some(artist_section("Globally trending", 4)),
                weekly_exploration: Some(WeeklyExploration {
                    title: "Weekly exploration".to_owned(),
                    playlist_date: "2026-09-28".to_owned(),
                    tracks: Vec::new(),
                    source_url: String::new(),
                }),
                integration_status: Some(fake_integration_status()),
                service_prompts: vec![ServicePrompt {
                    service: "listenbrainz".to_owned(),
                    title: "Connect ListenBrainz".to_owned(),
                    description: "Personal charts and history.".to_owned(),
                    icon: "chart".to_owned(),
                    color: "accent".to_owned(),
                    features: vec!["history".to_owned()],
                }],
                genre_artwork: HashMap::from([(
                    "trip-hop".to_owned(),
                    GenreArtwork {
                        kind: "gradient".to_owned(),
                        version: "v2".to_owned(),
                        albums: Vec::new(),
                    },
                )]),
                genre_artwork_schema_version: "v2".to_owned(),
                lastfm_weekly_artist_chart: None,
                lastfm_weekly_album_chart: None,
                lastfm_recent_scrobbles: None,
                daily_mixes: Vec::new(),
                radio_sections: vec![album_section("Radio: trip-hop", 3)],
                top_picks: Some(TopPicksSection {
                    title: "Top Picks for You".to_owned(),
                    items: Vec::new(),
                    source: Some("listenbrainz".to_owned()),
                    personalizing: false,
                }),
                listeners_like_you: None,
                anniversaries: None,
                new_from_followed: None,
                unexplored_genres: None,
                generated_at: Some(self.clock.now_unix()),
                refresh_started_at: None,
                section_status: HashMap::from([
                    ("fresh_releases".to_owned(), "ready".to_owned()),
                    ("globally_trending".to_owned(), "ready".to_owned()),
                ]),
                refreshing: false,
                service_status: Some(HashMap::from([
                    ("listenbrainz".to_owned(), "ok".to_owned()),
                    ("lastfm".to_owned(), "ok".to_owned()),
                ])),
            })
        })
    }

    fn home<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<HomeResponse, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            Ok(HomeResponse {
                recently_added: Some(album_section("Recently added", 4)),
                library_artists: Some(artist_section("Library artists", 4)),
                library_albums: Some(album_section("Library albums", 4)),
                recommended_artists: Some(artist_section("Recommended artists", 4)),
                trending_artists: Some(artist_section("Trending artists", 4)),
                popular_albums: Some(album_section("Popular albums", 4)),
                recently_played: None,
                top_genres: None,
                genre_list: None,
                fresh_releases: Some(album_section("Fresh releases", 4)),
                favorite_artists: None,
                your_top_albums: Some(album_section("Your top albums", 4)),
                weekly_exploration: None,
                service_prompts: Vec::new(),
                integration_status: Some(fake_integration_status()),
                genre_artwork: HashMap::new(),
                genre_artwork_schema_version: "v2".to_owned(),
                discover_preview: None,
                service_status: Some(HashMap::from([(
                    "listenbrainz".to_owned(),
                    "ok".to_owned(),
                )])),
                refreshing: false,
            })
        })
    }

    fn integration_status(&self) -> BoxFuture<'_, Result<IntegrationStatus, ProviderFailure>> {
        Box::pin(async move {
            self.failure()
                .map_or_else(|| Ok(fake_integration_status()), Err)
        })
    }

    fn has_local_files(&self) -> BoxFuture<'_, Result<bool, ProviderFailure>> {
        Box::pin(async move { self.failure().map_or(Ok(true), Err) })
    }

    fn record_activity<'a>(
        &'a self,
        user_id: &'a str,
        feature: &'a str,
        _artist_mbid: Option<&'a str>,
        _section: Option<&'a str>,
        _provider: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DiscoverActivityResponse, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            if let Ok(mut rows) = self.activities.lock() {
                rows.push((user_id.to_owned(), feature.to_owned()));
            }
            Ok(DiscoverActivityResponse {
                source_mode: "hybrid".to_owned(),
                source_id: "listenbrainz".to_owned(),
                generation: 7,
            })
        })
    }

    fn trigger_refresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            if let Ok(mut rows) = self.refreshes.lock() {
                rows.push(user_id.to_owned());
            }
            Ok(())
        })
    }
}

/// A queue with no decks and no providers, for rigs that never touch the
/// queue routes. The real queue runs over SQLite and scripted sources in
/// its own journey test.
pub struct FakeQueues;

impl QueueStore for FakeQueues {
    fn current<'a>(&'a self, _user_id: &'a str) -> BoxFuture<'a, Option<QueueDeck>> {
        Box::pin(async { None })
    }

    fn build_now<'a>(
        &'a self,
        _user_id: &'a str,
        _count: Option<usize>,
    ) -> BoxFuture<'a, Result<QueueDeck, ProviderFailure>> {
        Box::pin(async {
            Ok(QueueDeck {
                queue_id: "queue-empty".to_owned(),
                items: Vec::new(),
            })
        })
    }

    fn status<'a>(&'a self, _user_id: &'a str) -> BoxFuture<'a, DiscoverQueueStatusResponse> {
        Box::pin(async {
            DiscoverQueueStatusResponse {
                status: "idle".to_owned(),
                queue_id: None,
                item_count: None,
                built_at: None,
                stale: None,
                error: None,
            }
        })
    }

    fn start_build<'a>(
        &'a self,
        _user_id: &'a str,
        _trigger: QueueTrigger,
    ) -> BoxFuture<'a, QueueGenerateResponse> {
        Box::pin(async {
            QueueGenerateResponse {
                action: "disabled".to_owned(),
                status: "idle".to_owned(),
                queue_id: None,
                item_count: None,
                built_at: None,
                stale: None,
                error: None,
            }
        })
    }

    fn ignore_release<'a>(
        &'a self,
        _user_id: &'a str,
        _release: &'a QueueIgnoreRequest,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>> {
        Box::pin(async { Ok(()) })
    }

    fn ignored<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<IgnoredRelease>, ProviderFailure>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn enrich<'a>(
        &'a self,
        _user_id: &'a str,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<QueueEnrichment, ProviderFailure>> {
        Box::pin(async {
            Ok(QueueEnrichment {
                artist_mbid: None,
                release_date: None,
                country: None,
                tags: Vec::new(),
                youtube_url: None,
                youtube_search_url: String::new(),
                youtube_search_available: false,
                artist_description: None,
                listen_count: None,
            })
        })
    }

    fn preview<'a>(
        &'a self,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverQueuePreview, ProviderFailure>> {
        Box::pin(async {
            Ok(DiscoverQueuePreview {
                status: "unavailable".to_owned(),
                youtube_url: None,
                youtube_search_url: None,
            })
        })
    }

    fn validate<'a>(
        &'a self,
        _mbids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderFailure>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// Scripted chart pages and genre detail.
pub struct FakeCharts {
    fail_with: Mutex<Option<String>>,
    calls: Mutex<Vec<String>>,
}

impl FakeCharts {
    /// Build scripted charts.
    pub fn new() -> Self {
        Self {
            fail_with: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Arm every read to fail with `cause` (log-only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_charts(&self, cause: &str) {
        if let Ok(mut fail) = self.fail_with.lock() {
            *fail = Some(cause.to_owned());
        }
    }

    /// Recorded calls (`trending:this_week:listenbrainz`, ...), in order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default()
    }

    fn failure(&self) -> Option<ProviderFailure> {
        self.fail_with
            .lock()
            .ok()
            .and_then(|fail| fail.clone())
            .map(ProviderFailure::Failed)
    }

    fn record(&self, what: &str, range: ChartRange, source: ChartSource) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(format!("{what}:{}:{}", range.as_str(), source_name(source)));
        }
    }
}

impl Default for FakeCharts {
    fn default() -> Self {
        Self::new()
    }
}

fn source_name(source: ChartSource) -> &'static str {
    match source {
        ChartSource::Listenbrainz => "listenbrainz",
        ChartSource::Lastfm => "lastfm",
    }
}

impl ChartsSource for FakeCharts {
    fn trending_artists(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            self.record("trending", range, source);
            let items = (offset..offset + limit)
                .map(|rank| fake_artist(&format!("Artist {rank}"), rank))
                .collect();
            Ok(TrendingArtistsPage {
                range_key: range.as_str().to_owned(),
                label: range.label().to_owned(),
                items,
                offset,
                limit,
                has_more: true,
            })
        })
    }

    fn popular_albums(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            self.record("popular", range, source);
            let items = (offset..offset + limit)
                .map(|rank| fake_album(&format!("Album {rank}"), &format!("Artist {rank}"), rank))
                .collect();
            Ok(PopularAlbumsPage {
                range_key: range.as_str().to_owned(),
                label: range.label().to_owned(),
                items,
                offset,
                limit,
                has_more: true,
            })
        })
    }

    fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            self.record(&format!("your_top:{user_id}"), range, source);
            let items = (offset..offset + limit)
                .map(|rank| {
                    fake_album(
                        &format!("Top album {rank}"),
                        &format!("Top artist {rank}"),
                        rank,
                    )
                })
                .collect();
            Ok(PopularAlbumsPage {
                range_key: range.as_str().to_owned(),
                label: range.label().to_owned(),
                items,
                offset,
                limit,
                has_more: offset + limit < 40,
            })
        })
    }

    fn genre_detail<'a>(
        &'a self,
        genre: &'a str,
        limit: i64,
        artist_offset: i64,
        album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>> {
        Box::pin(async move {
            if let Some(failure) = self.failure() {
                return Err(failure);
            }
            let artists = (artist_offset..artist_offset + limit)
                .map(|rank| fake_artist(&format!("{genre} artist {rank}"), rank))
                .collect::<Vec<_>>();
            let albums = (album_offset..album_offset + limit)
                .map(|rank| fake_album(&format!("{genre} album {rank}"), genre, rank))
                .collect::<Vec<_>>();
            Ok(GenreDetailResponse {
                genre: genre.to_owned(),
                genre_artwork: GenreArtwork {
                    kind: "collage".to_owned(),
                    version: "v2".to_owned(),
                    albums: vec![GenreArtworkAlbum {
                        album_id: "local-1".to_owned(),
                        album_title: format!("{genre} album 1"),
                        cover_version: 3,
                        album_artist_name: Some(genre.to_owned()),
                    }],
                },
                library: Some(GenreLibrarySection {
                    artists: artists.clone(),
                    albums: albums.clone(),
                    artist_count: artists.len() as i64,
                    album_count: albums.len() as i64,
                }),
                popular: Some(GenrePopularSection {
                    artists,
                    albums,
                    has_more_artists: true,
                    has_more_albums: false,
                }),
                artists: Vec::new(),
                total_count: Some(120),
            })
        })
    }
}

/// Scripted 30-second previews.
#[derive(Debug, Default)]
pub struct FakePreviews;

impl PreviewSource for FakePreviews {
    /// Empty until real providers sit behind this port: no fake may invent
    /// provider names or preview URLs.
    fn track_preview<'a>(
        &'a self,
        _artist: &'a str,
        _track: &'a str,
    ) -> BoxFuture<'a, Result<TrackPreviewResponse, ProviderFailure>> {
        Box::pin(async move {
            Ok(TrackPreviewResponse {
                preview_url: None,
                title: None,
                duration_s: None,
                provider: None,
            })
        })
    }

    /// Empty until real providers sit behind this port.
    fn album_preview<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _count: i64,
    ) -> BoxFuture<'a, Result<AlbumSamples, ProviderFailure>> {
        Box::pin(async move { Ok((Vec::new(), None)) })
    }
}

/// Scripted YouTube lookups.
pub struct FakeYouTube {
    configured: bool,
    cached: Mutex<HashMap<String, bool>>,
}

impl FakeYouTube {
    /// Build a configured fake with one cached pair.
    #[cfg(any(test, feature = "test-support"))]
    pub fn configured() -> Self {
        Self {
            configured: true,
            cached: Mutex::new(HashMap::from([("portishead|roads".to_owned(), true)])),
        }
    }

    /// Build an unconfigured fake (quota 404s, cache checks answer empty).
    pub fn unconfigured() -> Self {
        Self {
            configured: false,
            cached: Mutex::new(HashMap::new()),
        }
    }
}

/// Deterministic stand-in video id. The `fake-` prefix keeps it visibly
/// canned (it pins handler mapping, never provider data), while hashing the
/// inputs keeps distinct lookups distinct. The YouTube Data API client
/// would replace this.
fn fake_video_id(artist: &str, name: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(artist.as_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    let short: String = hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("fake-{short}")
}

impl YouTubeSource for FakeYouTube {
    fn is_configured(&self) -> bool {
        self.configured
    }

    fn search_video<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async move {
            if !self.configured || album == "no-video-here" {
                return Ok(None);
            }
            Ok(Some(fake_video_id(artist, album)))
        })
    }

    fn search_track<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async move {
            if !self.configured || track == "no-video-here" {
                return Ok(None);
            }
            Ok(Some(fake_video_id(artist, track)))
        })
    }

    fn is_cached(&self, artist: &str, name: &str, _track: bool) -> bool {
        let key = format!("{}|{}", artist.to_lowercase(), name.to_lowercase());
        self.cached
            .lock()
            .map(|cached| cached.get(&key).copied().unwrap_or(false))
            .unwrap_or(false)
    }

    fn cached_tracks(&self, pairs: &[(String, String)]) -> HashMap<String, bool> {
        pairs
            .iter()
            .map(|(artist, track)| {
                let key = format!("{}|{}", artist.to_lowercase(), track.to_lowercase());
                let hit = self
                    .cached
                    .lock()
                    .map(|cached| cached.get(&key).copied().unwrap_or(false))
                    .unwrap_or(false);
                (key, hit)
            })
            .collect()
    }

    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>> {
        Box::pin(async move {
            if !self.configured {
                return None;
            }
            Some(YouTubeQuotaResponse {
                used: 120,
                limit: 10_000,
                resets_at: 1_790_000_000,
            })
        })
    }
}

/// In-memory discovery batches.
pub struct FakeBatches {
    clock: ManualClock,
    rows: Mutex<HashMap<String, BatchRow>>,
    order: Mutex<Vec<String>>,
    seq: Mutex<i64>,
}

impl FakeBatches {
    /// Build empty batch state pinned to `clock`.
    pub fn new(clock: ManualClock) -> Self {
        Self {
            clock,
            rows: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            seq: Mutex::new(0),
        }
    }
}

impl BatchStore for FakeBatches {
    fn create(
        &self,
        owner_id: &str,
        name: &str,
        source_section: &str,
        items: Vec<BatchItemRow>,
    ) -> Result<BatchRow, ProviderFailure> {
        let seq = self
            .seq
            .lock()
            .map(|mut seq| {
                *seq += 1;
                *seq
            })
            .unwrap_or(1)
            .max(1);
        let row = BatchRow {
            id: format!("batch-{seq}"),
            owner_id: owner_id.to_owned(),
            name: name.to_owned(),
            source_section: source_section.to_owned(),
            created_at: format!("2026-09-28T00:00:{:02}Z", seq % 60),
            items,
        };
        if let Ok(mut rows) = self.rows.lock() {
            rows.insert(row.id.clone(), row.clone());
        }
        if let Ok(mut order) = self.order.lock() {
            order.insert(0, row.id.clone());
        }
        let _ = self.clock.now_unix();
        Ok(row)
    }

    fn list_for_user(&self, owner_id: &str) -> Vec<BatchRow> {
        let order = self
            .order
            .lock()
            .map(|order| order.clone())
            .unwrap_or_default();
        let rows = self
            .rows
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default();
        order
            .into_iter()
            .filter_map(|id| rows.get(&id).cloned())
            .filter(|row| row.owner_id == owner_id)
            .collect()
    }

    fn get_for_user(&self, owner_id: &str, batch_id: &str) -> Option<BatchRow> {
        self.rows
            .lock()
            .ok()
            .and_then(|rows| rows.get(batch_id).cloned())
            .filter(|row| row.owner_id == owner_id)
    }

    fn remove(
        &self,
        owner_id: &str,
        batch_id: &str,
        remove_albums: bool,
    ) -> Option<(i64, i64, i64)> {
        let row = self
            .rows
            .lock()
            .ok()
            .and_then(|rows| rows.get(batch_id).cloned())
            .filter(|row| row.owner_id == owner_id)?;
        if let Ok(mut rows) = self.rows.lock() {
            rows.remove(batch_id);
        }
        if let Ok(mut order) = self.order.lock() {
            order.retain(|id| id != batch_id);
        }
        let albums = row.items.len() as i64;
        if remove_albums {
            Some((albums, albums, 0))
        } else {
            Some((0, 0, albums))
        }
    }
}

/// Scripted radio plans and suggestions.
#[derive(Debug, Default)]
pub struct FakeRadio;

impl RadioPlanner for FakeRadio {
    fn plan<'a>(
        &'a self,
        _user_id: &'a str,
        seed_type: &'a str,
        seed_id: Option<&'a str>,
        mode: &'a str,
        count: i64,
        exclude: &'a [String],
    ) -> BoxFuture<'a, Result<crate::reads::discover::models::RadioPlanResponse, ProviderFailure>>
    {
        Box::pin(async move {
            let tracks = (0..count)
                .filter(|rank| !exclude.contains(&format!("recording-{rank}")))
                .map(|rank| crate::reads::discover::models::RadioPlanTrack {
                    track_name: format!("Radio track {rank}"),
                    artist_name: format!("Radio artist {rank}"),
                    artist_mbid: format!("artist-mbid-{rank}"),
                    recording_mbid: Some(format!("recording-{rank}")),
                    album_mbid: Some(format!("release-{rank}")),
                    album_name: Some(format!("Radio album {rank}")),
                    in_library: mode == "library" || rank % 2 == 0,
                    local_file_id: if mode == "library" || rank % 2 == 0 {
                        Some(format!("file-{rank}"))
                    } else {
                        None
                    },
                    file_format: Some("flac".to_owned()),
                    duration_s: Some(180.0 + f64::from(rank as i32)),
                })
                .collect();
            Ok(crate::reads::discover::models::RadioPlanResponse {
                title: format!("Radio: {} {}", seed_type, seed_id.unwrap_or("mix")),
                tracks,
            })
        })
    }

    fn shelf<'a>(
        &'a self,
        seed_type: &'a str,
        seed_id: &'a str,
        count: i64,
        _source: ChartSource,
    ) -> BoxFuture<'a, Result<crate::reads::discover::models::ChartSection, ProviderFailure>> {
        Box::pin(async move {
            Ok(ChartSection {
                title: format!("Radio: {seed_type} {seed_id}"),
                section_type: "album".to_owned(),
                items: (0..count)
                    .map(|rank| {
                        SectionItem::Album(fake_album(
                            &format!("Radio album {rank}"),
                            &format!("Radio artist {rank}"),
                            rank,
                        ))
                    })
                    .collect(),
                source: Some("listenbrainz".to_owned()),
                fallback_message: None,
                connect_service: None,
                radio_seed_type: Some(seed_type.to_owned()),
                radio_seed_id: Some(seed_id.to_owned()),
            })
        })
    }

    fn playlist_suggestions<'a>(
        &'a self,
        _user_id: &'a str,
        playlist_id: &'a str,
        count: i64,
        _source: ChartSource,
    ) -> BoxFuture<
        'a,
        Result<crate::reads::discover::models::PlaylistSuggestionsResponse, ProviderFailure>,
    > {
        Box::pin(async move {
            Ok(
                crate::reads::discover::models::PlaylistSuggestionsResponse {
                    suggestions: album_section(&format!("For playlist {playlist_id}"), count),
                    playlist_id: playlist_id.to_owned(),
                    profile: crate::reads::discover::models::PlaylistProfile {
                        artist_mbids: vec!["artist-mbid-0".to_owned()],
                        genre_distribution: HashMap::from([(
                            "trip-hop".to_owned(),
                            vec!["Portishead".to_owned()],
                        )]),
                        track_count: 12,
                    },
                },
            )
        })
    }
}

/// Scripted presence: one live row plus one redacted row.
#[derive(Debug, Default)]
pub struct FakeNowPlaying;

impl NowPlayingStore for FakeNowPlaying {
    fn snapshot(&self) -> Vec<NowPlayingEntry> {
        vec![
            NowPlayingEntry {
                id: "user-1:web".to_owned(),
                user_name: "Molly".to_owned(),
                track_name: "Roads".to_owned(),
                artist_name: "Portishead".to_owned(),
                album_name: Some("Dummy".to_owned()),
                cover_url: "https://art.example.invalid/dummy.jpg".to_owned(),
                device_name: "Web".to_owned(),
                is_paused: false,
                source: "local".to_owned(),
                progress_ms: Some(61_000),
                duration_ms: Some(295_000),
                redacted: false,
            },
            NowPlayingEntry {
                id: "user-2:web".to_owned(),
                user_name: "Hidden Listener".to_owned(),
                track_name: String::new(),
                artist_name: String::new(),
                album_name: None,
                cover_url: String::new(),
                device_name: "Web".to_owned(),
                is_paused: true,
                source: "local".to_owned(),
                progress_ms: Some(10_000),
                duration_ms: Some(200_000),
                redacted: true,
            },
        ]
    }
}
