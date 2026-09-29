//! Ports behind the discover slice: content, charts, previews, and stores.
//!
//! Stage 4 runs these on fakes (see `fakes.rs`); stage 5 wires real
//! providers behind the same traits without touching the handlers. Every
//! fallible method returns a plain string cause: provider detail stays in
//! the log, never on the wire.

use std::collections::HashMap;

use crate::reads::discover::models::{
    ChartRange, ChartSource, DiscoverActivityResponse, DiscoverQueuePreview, DiscoverResponse,
    GenreDetailResponse, HomeResponse, IgnoredRelease, IntegrationStatus, NowPlayingEntry,
    PopularAlbumsPage, PreviewTrackItem, QueueEnrichment, QueueGenerateResponse, QueueItem,
    TrackPreviewResponse, TrendingArtistsPage, YouTubeQuotaResponse,
};

/// Failure talking to a provider. The string is a log-only cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure(pub String);

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Clock seam. Loops and TTL math read time through this so tests pin it.
pub trait Clock: Send + Sync {
    /// Current unix timestamp in seconds.
    fn now_unix(&self) -> i64;
}

/// System clock for production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Discover and home shelf content for one user.
pub trait DiscoverContent: Send + Sync {
    /// Cached discover shelves for the user, with section status attached.
    fn discover(&self, user_id: &str) -> Result<DiscoverResponse, ProviderFailure>;
    /// Cached home shelves for the user.
    fn home(&self, user_id: &str) -> Result<HomeResponse, ProviderFailure>;
    /// Integration availability behind the shelves.
    fn integration_status(&self) -> Result<IntegrationStatus, ProviderFailure>;
    /// Whether local files back playback (refines the status row).
    fn has_local_files(&self) -> Result<bool, ProviderFailure>;
    /// Record one discover interaction; returns the personalization cursor.
    fn record_activity(
        &self,
        user_id: &str,
        feature: &str,
        artist_mbid: Option<&str>,
        section: Option<&str>,
        provider: Option<&str>,
    ) -> Result<DiscoverActivityResponse, ProviderFailure>;
    /// Trigger a background discover rebuild for the user.
    fn trigger_refresh(&self, user_id: &str) -> Result<(), ProviderFailure>;
    /// Enrichment behind one queue card.
    fn enrich_queue_item(
        &self,
        release_group_mbid: &str,
    ) -> Result<QueueEnrichment, ProviderFailure>;
    /// On-demand preview behind one queue card.
    fn preview_queue_item(
        &self,
        release_group_mbid: &str,
    ) -> Result<DiscoverQueuePreview, ProviderFailure>;
    /// Library membership behind the given release-group ids.
    fn validate_queue_mbids(&self, mbids: &[String]) -> Result<Vec<String>, ProviderFailure>;
}

/// Queue build state for one user.
#[derive(Debug, Clone)]
pub struct QueueBuild {
    /// `ready`, `building`, `stale`, or `error`.
    pub status: String,
    /// Build id.
    pub queue_id: String,
    /// Deck cards.
    pub items: Vec<QueueItem>,
    /// Build timestamp (unix seconds).
    pub built_at: i64,
    /// Failure text, when the build failed.
    pub error: Option<String>,
}

/// The queue deck store: builds, cards, and the ignore ledger.
pub trait QueueStore: Send + Sync {
    /// Fresh deck for the user, when a live build exists.
    fn consume(&self, user_id: &str) -> Option<QueueBuild>;
    /// Build a lightweight deck synchronously (fallback when no build lives).
    fn build_lightweight(
        &self,
        user_id: &str,
        count: Option<i64>,
    ) -> Result<QueueBuild, ProviderFailure>;
    /// Make sure state for the user is loaded.
    fn ensure_loaded(&self, user_id: &str);
    /// Current build status for the user.
    fn status(&self, user_id: &str) -> QueueBuild;
    /// Start a build, or report the live one. `force` rebuilds regardless.
    fn start_build(&self, user_id: &str, force: bool) -> QueueGenerateResponse;
    /// Ignore one release for the user and kick a rebuild.
    fn ignore_release(
        &self,
        user_id: &str,
        release_group_mbid: &str,
        artist_mbid: &str,
        release_name: &str,
        artist_name: &str,
    );
    /// The user's ignore ledger, newest first.
    fn ignored(&self, user_id: &str) -> Vec<IgnoredRelease>;
}

/// Chart pages and genre detail.
pub trait ChartsSource: Send + Sync {
    /// One trending-artists page.
    fn trending_artists(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> Result<TrendingArtistsPage, ProviderFailure>;
    /// One popular-albums page.
    fn popular_albums(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> Result<PopularAlbumsPage, ProviderFailure>;
    /// One your-top-albums page for the user.
    fn your_top_albums(
        &self,
        user_id: &str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> Result<PopularAlbumsPage, ProviderFailure>;
    /// Genre detail with owned and popular rows.
    fn genre_detail(
        &self,
        genre: &str,
        limit: i64,
        artist_offset: i64,
        album_offset: i64,
    ) -> Result<GenreDetailResponse, ProviderFailure>;
}

/// Keyless 30-second previews (Deezer, then iTunes).
pub trait PreviewSource: Send + Sync {
    /// Preview behind one track. `None` fields mean no provider had one.
    fn track_preview(
        &self,
        artist: &str,
        track: &str,
    ) -> Result<TrackPreviewResponse, ProviderFailure>;
    /// Ordered album samples plus the serving provider.
    fn album_preview(
        &self,
        artist: &str,
        album: &str,
        count: i64,
    ) -> Result<(Vec<PreviewTrackItem>, Option<String>), ProviderFailure>;
}

/// YouTube lookups behind the queue deck.
pub trait YouTubeSource: Send + Sync {
    /// Whether the data API (quota'd) is configured.
    fn is_configured(&self) -> bool;
    /// Album video lookup. Returns the video id, when found.
    fn search_video(&self, artist: &str, album: &str) -> Result<Option<String>, ProviderFailure>;
    /// Track video lookup. Returns the video id, when found.
    fn search_track(&self, artist: &str, track: &str) -> Result<Option<String>, ProviderFailure>;
    /// Whether the pair resolves from cache.
    fn is_cached(&self, artist: &str, name: &str, track: bool) -> bool;
    /// Cache membership for deduped pairs, keyed `artist|track` (lowercased).
    fn cached_tracks(&self, pairs: &[(String, String)]) -> HashMap<String, bool>;
    /// Quota state. `None` when unconfigured (the route answers 404).
    fn quota(&self) -> Option<YouTubeQuotaResponse>;
}

/// One discovery batch row.
#[derive(Debug, Clone)]
pub struct BatchRow {
    /// Batch id.
    pub id: String,
    /// Owning user id.
    pub owner_id: String,
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    pub source_section: String,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Album rows: (release_group_mbid, artist_mbid, album_name, artist_name,
    /// outcome, request_status, in_library).
    pub items: Vec<BatchItemRow>,
}

/// One album row inside a batch.
#[derive(Debug, Clone)]
pub struct BatchItemRow {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// MusicBrainz artist id.
    pub artist_mbid: String,
    /// Album name.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// `requested`, `skipped_in_library`, or `skipped_duplicate`.
    pub outcome: String,
    /// Request state, when requested.
    pub request_status: Option<String>,
    /// True when already owned.
    pub in_library: bool,
}

/// Discovery batch store. Reads are owner-scoped: foreign ids read as
/// missing so callers answer 404 without leaking existence.
pub trait BatchStore: Send + Sync {
    /// Create a batch for the user; returns the stored row.
    fn create(
        &self,
        owner_id: &str,
        name: &str,
        source_section: &str,
        items: Vec<BatchItemRow>,
    ) -> BatchRow;
    /// The user's batches, newest first.
    fn list_for_user(&self, owner_id: &str) -> Vec<BatchRow>;
    /// One batch, when owned by the user.
    fn get_for_user(&self, owner_id: &str, batch_id: &str) -> Option<BatchRow>;
    /// Remove one batch. Returns (removed, cancelled, kept) when owned.
    fn remove(
        &self,
        owner_id: &str,
        batch_id: &str,
        remove_albums: bool,
    ) -> Option<(i64, i64, i64)>;
}

/// Live now-playing presence, already privacy-projected per owner setting.
pub trait NowPlayingStore: Send + Sync {
    /// Current live sessions.
    fn snapshot(&self) -> Vec<NowPlayingEntry>;
}

/// Radio plans and playlist suggestions.
pub trait RadioPlanner: Send + Sync {
    /// Build a complete radio plan for the user.
    fn plan(
        &self,
        user_id: &str,
        seed_type: &str,
        seed_id: Option<&str>,
        mode: &str,
        count: i64,
        exclude: &[String],
    ) -> Result<crate::reads::discover::models::RadioPlanResponse, ProviderFailure>;
    /// Build one radio shelf.
    fn shelf(
        &self,
        seed_type: &str,
        seed_id: &str,
        count: i64,
        source: ChartSource,
    ) -> Result<crate::reads::discover::models::ChartSection, ProviderFailure>;
    /// Build suggestions for one playlist.
    fn playlist_suggestions(
        &self,
        user_id: &str,
        playlist_id: &str,
        count: i64,
        source: ChartSource,
    ) -> Result<crate::reads::discover::models::PlaylistSuggestionsResponse, ProviderFailure>;
}
