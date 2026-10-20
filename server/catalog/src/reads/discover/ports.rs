//! Ports behind discover: content, charts, previews, and stores.
//!
//! Production wires the adapters in [`adapters`](super::adapters): live
//! ListenBrainz charts, Deezer/iTunes previews, YouTube search, and honest
//! empty or "not available" answers for the shelves whose builders are not
//! ported yet. Tests run the fakes. Every fallible method returns a
//! [`ProviderFailure`]: a provider's typed error, or a local cause that
//! stays in the log.

use std::collections::HashMap;

pub use futures_util::future::BoxFuture;

use droppedneedle_auth::Principal;
use droppedneedle_providers::ProviderError;

use crate::reads::discover::models::{
    ChartRange, ChartSection, ChartSource, DiscoverActivityResponse, DiscoverQueuePreview,
    DiscoverQueueStatusResponse, DiscoverResponse, GenreDetailResponse, HomeResponse,
    IgnoredRelease, IntegrationStatus, NowPlayingEntry, PopularAlbumsPage, PreviewTrackItem,
    QueueEnrichment, QueueGenerateResponse, QueueIgnoreRequest, QueueItem, TrackPreviewResponse,
    TrendingArtistsPage, YouTubeQuotaResponse,
};
use crate::reads::discover::models::{
    DiscoveryBatchCreate, DiscoveryBatchDetail, DiscoveryBatchRemoveResult, DiscoveryBatchSummary,
    PlaylistSuggestionsRequest, PlaylistSuggestionsResponse, RadioPlanRequest, RadioPlanResponse,
    RadioRequest,
};

/// Why a port could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFailure {
    /// An outside service failed. It answers by its kind: an outage names
    /// the service and its wait.
    Provider(ProviderError),
    /// A store or another local read failed. The string is a log-only
    /// cause.
    Failed(String),
    /// The feature needs setup first (a key, an account). The string is a
    /// full sentence for the user.
    NotConfigured(String),
    /// The feature is not built in this version yet. The string is a full
    /// sentence for the user.
    NotBuilt(String),
    /// A usage limit is spent for now. The string is a full sentence for
    /// the user.
    Exhausted(String),
}

impl ProviderFailure {
    /// A local failure with a log-only cause.
    pub fn failed(cause: impl Into<String>) -> Self {
        Self::Failed(cause.into())
    }

    /// A feature that needs setup first; `sentence` tells the user.
    pub fn not_configured(sentence: impl Into<String>) -> Self {
        Self::NotConfigured(sentence.into())
    }

    /// A feature not built yet; `sentence` tells the user.
    pub fn not_built(sentence: impl Into<String>) -> Self {
        Self::NotBuilt(sentence.into())
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => write!(f, "{error}"),
            Self::Failed(cause) => f.write_str(cause),
            Self::NotConfigured(sentence)
            | Self::NotBuilt(sentence)
            | Self::Exhausted(sentence) => f.write_str(sentence),
        }
    }
}

/// Discover and home shelf content for one user.
pub trait DiscoverContent: Send + Sync {
    /// Cached discover shelves for the user, with section status attached.
    fn discover<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverResponse, ProviderFailure>>;
    /// Cached home shelves for the user.
    fn home<'a>(&'a self, user_id: &'a str)
    -> BoxFuture<'a, Result<HomeResponse, ProviderFailure>>;
    /// Integration availability behind the shelves.
    fn integration_status(&self) -> BoxFuture<'_, Result<IntegrationStatus, ProviderFailure>>;
    /// Whether local files back playback (refines the status row).
    fn has_local_files(&self) -> BoxFuture<'_, Result<bool, ProviderFailure>>;
    /// Record one discover interaction; returns the personalization cursor.
    fn record_activity<'a>(
        &'a self,
        user_id: &'a str,
        feature: &'a str,
        artist_mbid: Option<&'a str>,
        section: Option<&'a str>,
        provider: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DiscoverActivityResponse, ProviderFailure>>;
    /// Trigger a background discover rebuild for the user.
    fn trigger_refresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>>;
    /// One pass of the warm cycle: rebuild what recent activity says is
    /// due. Content without a warm cycle does nothing.
    fn run_due_tick(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// What asked for a queue build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTrigger {
    /// A person asked (generate, or an ignore that reshuffles the deck).
    Request {
        /// Rebuild even when the current deck is fresh.
        force: bool,
    },
    /// The background warm cycle. Skipped while the warm-cycle setting is
    /// off, and replaced by any request that arrives while it runs.
    Scheduled,
}

/// One deck as the queue route serves it.
#[derive(Debug, Clone)]
pub struct QueueDeck {
    /// Build id.
    pub queue_id: String,
    /// Deck cards in order.
    pub items: Vec<QueueItem>,
}

/// The queue deck: background builds, the ignore ledger, and the details
/// behind each card.
pub trait QueueStore: Send + Sync {
    /// The user's last built deck, stale or not, when there is one.
    fn current<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<QueueDeck>>;
    /// Build a deck now, in the request, without keeping it. `count`
    /// overrides the configured queue size.
    fn build_now<'a>(
        &'a self,
        user_id: &'a str,
        count: Option<usize>,
    ) -> BoxFuture<'a, Result<QueueDeck, ProviderFailure>>;
    /// Where the user's background build stands.
    fn status<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, DiscoverQueueStatusResponse>;
    /// Start a background build, or report why none started.
    fn start_build<'a>(
        &'a self,
        user_id: &'a str,
        trigger: QueueTrigger,
    ) -> BoxFuture<'a, QueueGenerateResponse>;
    /// Record one ignored release for the user. Later decks skip it.
    fn ignore_release<'a>(
        &'a self,
        user_id: &'a str,
        release: &'a QueueIgnoreRequest,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>>;
    /// The user's ignore ledger, newest first.
    fn ignored<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<IgnoredRelease>, ProviderFailure>>;
    /// Details behind one card: tags, date, country, bio, listens, video.
    fn enrich<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<QueueEnrichment, ProviderFailure>>;
    /// An on-demand video preview behind one card.
    fn preview<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverQueuePreview, ProviderFailure>>;
    /// The given release groups that the library already holds.
    fn validate<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderFailure>>;
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
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>>;
    /// One popular-albums page.
    fn popular_albums(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>>;
    /// One your-top-albums page for the user.
    fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>>;
    /// Genre detail with owned and popular rows.
    fn genre_detail<'a>(
        &'a self,
        genre: &'a str,
        limit: i64,
        artist_offset: i64,
        album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>>;
}

/// Ordered album samples plus the provider that served them.
pub type AlbumSamples = (Vec<PreviewTrackItem>, Option<String>);

/// Keyless 30-second previews (Deezer, then iTunes).
pub trait PreviewSource: Send + Sync {
    /// Preview behind one track. `None` fields mean no provider had one.
    fn track_preview<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<TrackPreviewResponse, ProviderFailure>>;
    /// Ordered album samples plus the serving provider.
    fn album_preview<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        count: i64,
    ) -> BoxFuture<'a, Result<AlbumSamples, ProviderFailure>>;
}

/// YouTube lookups behind the queue deck.
pub trait YouTubeSource: Send + Sync {
    /// Whether the data API (quota'd) is configured.
    fn is_configured(&self) -> bool;
    /// Album video lookup. Returns the video id, when found.
    fn search_video<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>>;
    /// Track video lookup. Returns the video id, when found.
    fn search_track<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>>;
    /// Whether the pair resolves from cache.
    fn is_cached(&self, artist: &str, name: &str, track: bool) -> bool;
    /// Cache membership for deduped pairs, keyed `artist|track` (lowercased).
    fn cached_tracks(&self, pairs: &[(String, String)]) -> HashMap<String, bool>;
    /// Quota state. `None` when unconfigured (the route answers 404).
    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>>;
}

/// Why a batch action was refused by the request rules (quota spent,
/// storage full). Carries the requests envelope as is, so the page shows
/// the same message and details a plain album request would.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRefusal {
    /// HTTP status the requests routes answer with (429 or 403).
    pub status: u16,
    /// Stable code (`QUOTA_EXCEEDED`, `STORAGE_FULL`).
    pub code: &'static str,
    /// User-facing sentence.
    pub message: String,
    /// The usage numbers behind the refusal, when there are any.
    pub details: Option<serde_json::Value>,
}

/// Why a batch action failed.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchFailure {
    /// Unknown batch, or one the caller may not see.
    NotFound,
    /// The batch itself is wrong. The string is user-facing.
    Invalid(String),
    /// The request rules refused it.
    Refused(BatchRefusal),
    /// A store or the acquisition side failed.
    Provider(ProviderFailure),
}

impl From<ProviderFailure> for BatchFailure {
    fn from(failure: ProviderFailure) -> Self {
        Self::Provider(failure)
    }
}

/// Discovery batches: "download this whole shelf", filed as real album
/// requests and removable again. Owners see their own batches; admins
/// may open and remove anyone's. A batch the caller may not see reads as
/// missing.
pub trait BatchStore: Send + Sync {
    /// File the batch's albums as requests and record the batch.
    fn create<'a>(
        &'a self,
        caller: &'a Principal,
        body: &'a DiscoveryBatchCreate,
    ) -> BoxFuture<'a, Result<DiscoveryBatchDetail, BatchFailure>>;
    /// The caller's batches, newest first.
    fn list<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<DiscoveryBatchSummary>, BatchFailure>>;
    /// One batch with each album's current state.
    fn detail<'a>(
        &'a self,
        caller: &'a Principal,
        batch_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoveryBatchDetail, BatchFailure>>;
    /// Remove one batch, and with `remove_albums` the albums it caused:
    /// pending requests are cancelled, landed albums go to the recycle bin.
    fn remove<'a>(
        &'a self,
        caller: &'a Principal,
        batch_id: &'a str,
        remove_albums: bool,
    ) -> BoxFuture<'a, Result<DiscoveryBatchRemoveResult, BatchFailure>>;
}

/// Live now-playing presence, already privacy-projected per owner setting.
pub trait NowPlayingStore: Send + Sync {
    /// Current live sessions.
    fn snapshot(&self) -> Vec<NowPlayingEntry>;
}

/// Why a radio station or playlist suggestion could not be built.
#[derive(Debug, Clone, PartialEq)]
pub enum RadioFailure {
    /// The seed album or playlist does not exist (or is private).
    NotFound,
    /// The seed cannot make a station. The string is user-facing.
    Invalid(String),
    /// A provider or store failed.
    Provider(ProviderFailure),
}

impl From<ProviderFailure> for RadioFailure {
    fn from(failure: ProviderFailure) -> Self {
        Self::Provider(failure)
    }
}

/// Radio plans, radio shelves and playlist suggestions. Requests arrive
/// validated, with counts already clamped.
pub trait RadioPlanner: Send + Sync {
    /// A complete, finite track plan for the user.
    fn plan<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a RadioPlanRequest,
    ) -> BoxFuture<'a, Result<RadioPlanResponse, RadioFailure>>;
    /// One album shelf seeded by an artist, album or genre.
    fn shelf<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a RadioRequest,
    ) -> BoxFuture<'a, Result<ChartSection, RadioFailure>>;
    /// Albums that extend one playlist.
    fn playlist_suggestions<'a>(
        &'a self,
        user_id: &'a str,
        request: &'a PlaylistSuggestionsRequest,
    ) -> BoxFuture<'a, Result<PlaylistSuggestionsResponse, RadioFailure>>;
}
