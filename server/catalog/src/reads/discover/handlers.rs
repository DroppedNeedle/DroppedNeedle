//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the service,
//! render. Status mapping lives in `From<ServiceError> for ApiError`
//! (`super::error`).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Deserialize;

use super::{
    models::{
        AlbumPreviewResponse, AlbumRef, ArtistRef, ChartPage, ChartSection,
        DiscoverActivityRequest, DiscoverActivityResponse, DiscoverQueuePreview,
        DiscoverQueueResponse, DiscoverQueueStatusResponse, DiscoverResponse, DiscoveryBatchCreate,
        DiscoveryBatchDetail, DiscoveryBatchListResponse, DiscoveryBatchRemoveResult,
        GenreDetailResponse, HomeResponse, IgnoredReleasesResponse, IntegrationStatus,
        ListenSource, PlaylistSuggestionsRequest, PlaylistSuggestionsResponse, QueueEnrichment,
        QueueGenerateRequest, QueueGenerateResponse, QueueIgnoreRequest, QueueValidateRequest,
        QueueValidateResponse, RadioPlanRequest, RadioPlanResponse, RadioRequest, RefreshResponse,
        StatsRange, TrackCacheCheckRequest, TrackCacheCheckResponse, TrackPreviewResponse,
        YouTubeQuotaResponse, YouTubeSearchResponse,
    },
    services::{self, ReadsDeps},
};

// ---------------------------------------------------------------------------
// Query shapes
// ---------------------------------------------------------------------------

/// `GET /discover/queue` params.
#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    /// Wanted card count (clamped to 20).
    pub count: Option<i64>,
}

/// Chart page params shared by the three redesigned routes.
#[derive(Debug, Deserialize)]
pub struct ChartQuery {
    /// Range (`this_week` default).
    pub range: Option<StatsRange>,
    /// Page size.
    pub limit: Option<i64>,
    /// Page offset.
    pub offset: Option<i64>,
    /// Provider preference (`listenbrainz` default).
    pub source: Option<ListenSource>,
}

/// Genre detail params.
#[derive(Debug, Deserialize)]
pub struct GenreQuery {
    /// Rows per lane.
    pub limit: Option<i64>,
    /// Artist lane offset.
    pub artist_offset: Option<i64>,
    /// Album lane offset.
    pub album_offset: Option<i64>,
}

/// YouTube album search params.
#[derive(Debug, Deserialize)]
pub struct YouTubeAlbumQuery {
    /// Artist name.
    pub artist: String,
    /// Album name.
    pub album: String,
}

/// YouTube track search params.
#[derive(Debug, Deserialize)]
pub struct YouTubeTrackQuery {
    /// Artist name.
    pub artist: String,
    /// Track name.
    pub track: String,
}

/// Track preview params.
#[derive(Debug, Deserialize)]
pub struct TrackPreviewQuery {
    /// Artist name.
    pub artist: String,
    /// Track name.
    pub track: String,
}

/// Album preview params.
#[derive(Debug, Deserialize)]
pub struct AlbumPreviewQuery {
    /// Artist name.
    pub artist: String,
    /// Album name.
    pub album: String,
    /// Wanted sample count (1-8, default 4).
    pub count: Option<i64>,
}

/// Batch removal params.
#[derive(Debug, Deserialize)]
pub struct RemoveBatchQuery {
    /// Also remove/cancel the batch's albums (default true).
    pub remove_albums: Option<bool>,
}

// ---------------------------------------------------------------------------
// Discover
// ---------------------------------------------------------------------------

/// Cached discover shelves for the caller.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover",
    responses(
        (status = 200, description = "Discover shelves", body = DiscoverResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_discover(
    State(deps): State<ReadsDeps>,
    User(user): User,
) -> Result<Json<DiscoverResponse>, ApiError> {
    services::discover(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Trigger a background discover rebuild for the caller.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/refresh",
    responses(
        (status = 202, description = "Refresh triggered", body = RefreshResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn refresh_discover(
    State(deps): State<ReadsDeps>,
    User(user): User,
) -> Result<(StatusCode, Json<RefreshResponse>), ApiError> {
    services::refresh_discover(&deps, user.user_id())
        .await
        .map(|body| (StatusCode::ACCEPTED, Json(body)))
        .map_err(ApiError::from)
}

/// Record one discover interaction for personalization.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/activity",
    request_body = DiscoverActivityRequest,
    responses(
        (status = 200, description = "Personalization cursor", body = DiscoverActivityResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn record_activity(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoverActivityRequest>,
) -> Result<Json<DiscoverActivityResponse>, ApiError> {
    services::record_activity(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Generate one radio shelf from a seed.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/radio",
    request_body = RadioRequest,
    responses(
        (status = 200, description = "Radio shelf", body = ChartSection),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown seed album or playlist"),
        (status = 502, description = "A music provider failed"),
    )
)]
pub async fn discover_radio(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<RadioRequest>,
) -> Result<Json<ChartSection>, ApiError> {
    services::radio_shelf(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Build a complete track-level radio plan.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/radio/plan",
    request_body = RadioPlanRequest,
    responses(
        (status = 200, description = "Radio plan", body = RadioPlanResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown seed album or playlist"),
        (status = 502, description = "A music provider failed"),
    )
)]
pub async fn radio_plan(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<RadioPlanRequest>,
) -> Result<Json<RadioPlanResponse>, ApiError> {
    services::radio_plan(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Suggest rows extending one playlist.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/playlist-suggestions",
    request_body = PlaylistSuggestionsRequest,
    responses(
        (status = 200, description = "Playlist suggestions", body = PlaylistSuggestionsResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown seed album or playlist"),
        (status = 502, description = "A music provider failed"),
    )
)]
pub async fn playlist_suggestions(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<PlaylistSuggestionsRequest>,
) -> Result<Json<PlaylistSuggestionsResponse>, ApiError> {
    services::playlist_suggestions(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// The queue deck: the caller's last built deck, else one built now.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue",
    params(("count" = Option<i64>, Query, description = "Wanted card count, max 20")),
    responses(
        (status = 200, description = "Queue deck", body = DiscoverQueueResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The library check failed"),
    )
)]
pub async fn get_queue(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<QueueQuery>,
) -> Result<Json<DiscoverQueueResponse>, ApiError> {
    services::queue(&deps, user.user_id(), query.count)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Where the caller's background queue build stands: `idle`, `building`,
/// `ready` (with the deck's id, size, age and staleness) or `error`.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue/status",
    responses(
        (status = 200, description = "Queue build status", body = DiscoverQueueStatusResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn queue_status(
    State(deps): State<ReadsDeps>,
    User(user): User,
) -> Json<DiscoverQueueStatusResponse> {
    Json(services::queue_status(&deps, user.user_id()).await)
}

/// Start a background queue build. `action` says what happened:
/// `started`, `already_building`, or `already_ready` (a fresh deck exists
/// and `force` was not set).
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/queue/generate",
    request_body = QueueGenerateRequest,
    responses(
        (status = 200, description = "Build state", body = QueueGenerateResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn queue_generate(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<QueueGenerateRequest>,
) -> Json<QueueGenerateResponse> {
    Json(services::queue_generate(&deps, user.user_id(), &body).await)
}

/// Details behind one queue card: tags, release date, country, artist
/// bio, listen count and a video link when one is known.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue/enrich/{release_group_mbid}",
    params(("release_group_mbid" = String, Path, description = "Release-group id")),
    responses(
        (status = 200, description = "Card details", body = QueueEnrichment),
        (status = 400, description = "Bad release-group id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn enrich_queue_item(
    State(deps): State<ReadsDeps>,
    User(user): User,
    Path(release_group_mbid): Path<String>,
) -> Result<Json<QueueEnrichment>, ApiError> {
    services::enrich_queue_item(&deps, user.user_id(), &release_group_mbid)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// A video preview behind one queue card: a MusicBrainz video link when
/// one exists, else a YouTube search when an API key is set.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/queue/preview/{release_group_mbid}",
    params(("release_group_mbid" = String, Path, description = "Release-group id")),
    responses(
        (status = 200, description = "Card preview", body = DiscoverQueuePreview),
        (status = 400, description = "Bad release-group id"),
        (status = 401, description = "Not authenticated"),
        (status = 502, description = "MusicBrainz or YouTube failed"),
    )
)]
pub async fn preview_queue_item(
    State(deps): State<ReadsDeps>,
    _user: User,
    Path(release_group_mbid): Path<String>,
) -> Result<Json<DiscoverQueuePreview>, ApiError> {
    services::preview_queue_item(&deps, &release_group_mbid)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Ignore one release: later decks skip it, and a new deck starts
/// building at once.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/queue/ignore",
    request_body = QueueIgnoreRequest,
    responses(
        (status = 204, description = "Release ignored"),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The ignore could not be saved"),
    )
)]
pub async fn ignore_queue_item(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<QueueIgnoreRequest>,
) -> Result<StatusCode, ApiError> {
    services::ignore_queue_item(&deps, user.user_id(), &body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// The caller's ignore ledger, newest first.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue/ignored",
    responses(
        (status = 200, description = "Ignore ledger", body = IgnoredReleasesResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The ledger could not be read"),
    )
)]
pub async fn ignored_releases(
    State(deps): State<ReadsDeps>,
    User(user): User,
) -> Result<Json<IgnoredReleasesResponse>, ApiError> {
    services::ignored_releases(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The given cards whose albums the library already holds.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/queue/validate",
    request_body = QueueValidateRequest,
    responses(
        (status = 200, description = "Library membership", body = QueueValidateResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The library check failed"),
    )
)]
pub async fn validate_queue(
    State(deps): State<ReadsDeps>,
    _user: User,
    ValidJson(body): ValidJson<QueueValidateRequest>,
) -> Result<Json<QueueValidateResponse>, ApiError> {
    services::validate_queue(&deps, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Album video lookup with the cached flag.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue/youtube-search",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("album" = String, Query, description = "Album name"),
    ),
    responses(
        (status = 200, description = "Resolved video", body = YouTubeSearchResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 429, description = "YouTube search quota used up for today (RATE_LIMITED)"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn youtube_search(
    State(deps): State<ReadsDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<YouTubeAlbumQuery>,
) -> Result<Json<YouTubeSearchResponse>, ApiError> {
    services::youtube_search(&deps, &query.artist, &query.album)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Track video lookup with the cached flag.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue/youtube-track-search",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("track" = String, Query, description = "Track name"),
    ),
    responses(
        (status = 200, description = "Resolved video", body = YouTubeSearchResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 429, description = "YouTube search quota used up for today (RATE_LIMITED)"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn youtube_track_search(
    State(deps): State<ReadsDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<YouTubeTrackQuery>,
) -> Result<Json<YouTubeSearchResponse>, ApiError> {
    services::youtube_track_search(&deps, &query.artist, &query.track)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// YouTube data-API quota state. 404 when YouTube is unconfigured.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/queue/youtube-quota",
    responses(
        (status = 200, description = "Quota state", body = YouTubeQuotaResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "YouTube unconfigured"),
    )
)]
pub async fn youtube_quota(
    State(deps): State<ReadsDeps>,
    _user: User,
) -> Result<Json<YouTubeQuotaResponse>, ApiError> {
    services::youtube_quota(&deps)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Bulk YouTube cache membership. Unconfigured YouTube answers empty.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/queue/youtube-cache-check",
    request_body = TrackCacheCheckRequest,
    responses(
        (status = 200, description = "Cache membership", body = TrackCacheCheckResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn youtube_cache_check(
    State(deps): State<ReadsDeps>,
    _user: User,
    ValidJson(body): ValidJson<TrackCacheCheckRequest>,
) -> Result<Json<TrackCacheCheckResponse>, ApiError> {
    services::youtube_cache_check(&deps, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// A 30-second track preview. Empty means no provider had one.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/track-preview",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("track" = String, Query, description = "Track name"),
    ),
    responses(
        (status = 200, description = "Track preview", body = TrackPreviewResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn track_preview(
    State(deps): State<ReadsDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<TrackPreviewQuery>,
) -> Result<Json<TrackPreviewResponse>, ApiError> {
    services::track_preview(&deps, &query.artist, &query.track)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Ordered 30-second samples of an album's first tracks.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/album-preview",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("album" = String, Query, description = "Album name"),
        ("count" = Option<i64>, Query, description = "Wanted samples, 1-8"),
    ),
    responses(
        (status = 200, description = "Album samples", body = AlbumPreviewResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn album_preview(
    State(deps): State<ReadsDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<AlbumPreviewQuery>,
) -> Result<Json<AlbumPreviewResponse>, ApiError> {
    services::album_preview(&deps, &query.artist, &query.album, query.count)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

/// Create a discovery batch: every album the library lacks and nobody
/// has requested yet is filed as an album request, under the caller's
/// quota and approval rules.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/batches",
    request_body = DiscoveryBatchCreate,
    responses(
        (status = 202, description = "Batch created", body = DiscoveryBatchDetail),
        (status = 400, description = "Empty or oversized batch, or every album is already owned or requested"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Storage full (STORAGE_FULL)"),
        (status = 429, description = "Request quota spent (QUOTA_EXCEEDED)"),
        (status = 409, description = "Acquisition is not running (NOT_AVAILABLE)"),
    )
)]
pub async fn create_batch(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoveryBatchCreate>,
) -> Result<(StatusCode, Json<DiscoveryBatchDetail>), ApiError> {
    services::create_batch(&deps, &user, &body)
        .await
        .map(|detail| (StatusCode::ACCEPTED, Json(detail)))
        .map_err(ApiError::from)
}

/// The caller's batches, newest first.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/batches",
    responses(
        (status = 200, description = "Batch list", body = DiscoveryBatchListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The batch store failed"),
    )
)]
pub async fn list_batches(
    State(deps): State<ReadsDeps>,
    User(user): User,
) -> Result<Json<DiscoveryBatchListResponse>, ApiError> {
    services::list_batches(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// One batch with each album's current state. Admins may open any batch;
/// anyone else's reads as missing.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/batches/{batch_id}",
    params(("batch_id" = String, Path, description = "Batch id")),
    responses(
        (status = 200, description = "Batch detail", body = DiscoveryBatchDetail),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown batch id"),
    )
)]
pub async fn get_batch(
    State(deps): State<ReadsDeps>,
    User(user): User,
    Path(batch_id): Path<String>,
) -> Result<Json<DiscoveryBatchDetail>, ApiError> {
    services::get_batch(&deps, &user, &batch_id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Remove one batch. With `remove_albums` (the default) its pending
/// requests are cancelled and the albums it brought in go to the recycle
/// bin; albums that were already owned or requested are never touched.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/batches/{batch_id}",
    params(
        ("batch_id" = String, Path, description = "Batch id"),
        ("remove_albums" = Option<bool>, Query, description = "Also remove the batch albums"),
    ),
    responses(
        (status = 200, description = "Removal outcome", body = DiscoveryBatchRemoveResult),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown batch id"),
    )
)]
pub async fn remove_batch(
    State(deps): State<ReadsDeps>,
    User(user): User,
    Path(batch_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveBatchQuery>,
) -> Result<Json<DiscoveryBatchRemoveResult>, ApiError> {
    services::remove_batch(&deps, &user, &batch_id, query.remove_albums.unwrap_or(true))
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

/// Cached home shelves for the caller.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home",
    responses(
        (status = 200, description = "Home shelves", body = HomeResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_home(
    State(deps): State<ReadsDeps>,
    User(user): User,
) -> Result<Json<HomeResponse>, ApiError> {
    services::home(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Integration availability behind the shelves.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home/integration-status",
    responses(
        (status = 200, description = "Integration availability", body = IntegrationStatus),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn integration_status(
    State(deps): State<ReadsDeps>,
    _user: User,
) -> Result<Json<IntegrationStatus>, ApiError> {
    services::integration_status(&deps)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Genre detail with owned and popular rows.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home/genre/{genre_name}",
    params(
        ("genre_name" = String, Path, description = "Genre name"),
        ("limit" = Option<i64>, Query, description = "Rows per lane"),
        ("artist_offset" = Option<i64>, Query, description = "Artist lane offset"),
        ("album_offset" = Option<i64>, Query, description = "Album lane offset"),
    ),
    responses(
        (status = 200, description = "Genre detail", body = GenreDetailResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn genre_detail(
    State(deps): State<ReadsDeps>,
    _user: User,
    Path(genre_name): Path<String>,
    ValidQuery(query): ValidQuery<GenreQuery>,
) -> Result<Json<GenreDetailResponse>, ApiError> {
    services::genre_detail(
        &deps,
        &genre_name,
        query.limit,
        query.artist_offset,
        query.album_offset,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// One trending-artists page. Range rides the `range` query param (one
/// route replaces the v2 base plus `/{range_key}` pair).
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home/trending/artists",
    params(
        ("range" = Option<StatsRange>, Query, description = "Range, this_week default"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
        ("source" = Option<ListenSource>, Query, description = "Provider, listenbrainz default"),
    ),
    responses(
        (status = 200, description = "Trending artists page", body = ChartPage<ArtistRef>),
        (status = 400, description = "Bad range or source"),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn trending_artists(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<ChartQuery>,
) -> Result<Json<ChartPage<ArtistRef>>, ApiError> {
    services::trending_artists(
        &deps,
        user.user_id(),
        query.range,
        query.limit,
        query.offset,
        query.source,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// One popular-albums page (same range-param redesign).
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home/popular/albums",
    params(
        ("range" = Option<StatsRange>, Query, description = "Range, this_week default"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
        ("source" = Option<ListenSource>, Query, description = "Provider, listenbrainz default"),
    ),
    responses(
        (status = 200, description = "Popular albums page", body = ChartPage<AlbumRef>),
        (status = 400, description = "Bad range or source"),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn popular_albums(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<ChartQuery>,
) -> Result<Json<ChartPage<AlbumRef>>, ApiError> {
    services::popular_albums(
        &deps,
        user.user_id(),
        query.range,
        query.limit,
        query.offset,
        query.source,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// One your-top-albums page for the caller (same redesign, per-user).
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home/your-top/albums",
    params(
        ("range" = Option<StatsRange>, Query, description = "Range, this_week default"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
        ("source" = Option<ListenSource>, Query, description = "Provider, listenbrainz default"),
    ),
    responses(
        (status = 200, description = "Your-top albums page", body = ChartPage<AlbumRef>),
        (status = 400, description = "Bad range or source"),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED) or not built in this version yet (NOT_AVAILABLE)"),
    )
)]
pub async fn your_top_albums(
    State(deps): State<ReadsDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<ChartQuery>,
) -> Result<Json<ChartPage<AlbumRef>>, ApiError> {
    services::your_top_albums(
        &deps,
        user.user_id(),
        query.range,
        query.limit,
        query.offset,
        query.source,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// Discover and home routes at their full `/api/v3` paths, for any
/// signed-in user. `GET /now-playing` is served by `playback`.
pub fn routes() -> Routes<ReadsDeps> {
    Routes::new()
        .user(routes!(get_discover))
        .user(routes!(refresh_discover))
        .user(routes!(record_activity))
        .user(routes!(discover_radio))
        .user(routes!(radio_plan))
        .user(routes!(playlist_suggestions))
        .user(routes!(get_queue))
        .user(routes!(queue_status))
        .user(routes!(queue_generate))
        .user(routes!(enrich_queue_item))
        .user(routes!(preview_queue_item))
        .user(routes!(ignore_queue_item))
        .user(routes!(ignored_releases))
        .user(routes!(validate_queue))
        .user(routes!(youtube_search))
        .user(routes!(youtube_track_search))
        .user(routes!(youtube_quota))
        .user(routes!(youtube_cache_check))
        .user(routes!(track_preview))
        .user(routes!(album_preview))
        .user(routes!(create_batch, list_batches))
        .user(routes!(get_batch, remove_batch))
        .user(routes!(get_home))
        .user(routes!(integration_status))
        .user(routes!(genre_detail))
        .user(routes!(trending_artists))
        .user(routes!(popular_albums))
        .user(routes!(your_top_albums))
}
