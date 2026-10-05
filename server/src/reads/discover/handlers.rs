//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the service,
//! render. Status mapping lives in [`ReadsError`](super::error::ReadsError).
//! Bodies parse through [`ValidJson`] and query strings through [`ValidQuery`],
//! which keep malformed input inside the shared error envelope instead of
//! Axum's default plain-text 400.

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Path, Query, State},
    http::{StatusCode, request::Parts},
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{
    error::ReadsError,
    models::{
        AlbumPreviewResponse, ChartSection, DiscoverActivityRequest, DiscoverActivityResponse,
        DiscoverQueuePreview, DiscoverQueueResponse, DiscoverQueueStatusResponse, DiscoverResponse,
        DiscoveryBatchCreate, DiscoveryBatchDetail, DiscoveryBatchListResponse,
        DiscoveryBatchRemoveResult, GenreDetailResponse, HomeResponse, IgnoredReleasesResponse,
        IntegrationStatus, NowPlayingSnapshot, PlaylistSuggestionsRequest,
        PlaylistSuggestionsResponse, PopularAlbumsPage, QueueEnrichment, QueueGenerateRequest,
        QueueGenerateResponse, QueueIgnoreRequest, QueueValidateRequest, QueueValidateResponse,
        RadioPlanRequest, RadioPlanResponse, RadioRequest, RefreshResponse, TrackCacheCheckRequest,
        TrackCacheCheckResponse, TrackPreviewResponse, TrendingArtistsPage, YouTubeQuotaResponse,
        YouTubeSearchResponse,
    },
    services::{self, ReadsDeps, ServiceError},
};
use crate::auth::session::middleware::CurrentSession;

/// The authenticated caller. The deny-by-default session middleware stashes
/// the session; this extractor reads it without a store round-trip (all
/// slice routes are plain authenticated-user routes, no role gating).
///
/// Stale sessions need no user-row re-read here: `auth_tokens.user_id`
/// references `auth_users(id)` ON DELETE CASCADE (migration 0001), so
/// deleting an account revokes its sessions and the middleware 401s before
/// any handler runs (pinned by the journey_b E2E). The library/collections
/// extractors re-read the row only to resolve role and username fresh.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// Owning user id.
    pub user_id: String,
}

impl AuthenticatedUser {
    /// Reject unauthenticated callers with the 401 envelope.
    pub fn require(parts: &Parts) -> Result<Self, ReadsError> {
        parts
            .extensions
            .get::<CurrentSession>()
            .map(|session| Self {
                user_id: session.user_id.clone(),
            })
            .ok_or_else(|| ReadsError::Unauthorized {
                message: "Authentication required".to_owned(),
            })
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedUser {
    type Rejection = ReadsError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Self::require(parts)
    }
}

/// JSON body extractor that renders failures in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = ReadsError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| ReadsError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

/// Query extractor that renders failures in the shared envelope.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = ReadsError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| ReadsError::InvalidInput {
                message: format!("Invalid query string: {cause}"),
            })
    }
}

fn map(error: ServiceError, deps: &ReadsDeps) -> ReadsError {
    error.into_reads_error(deps.ids.as_ref())
}

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
    /// Range key (`this_week` default).
    pub range: Option<String>,
    /// Page size.
    pub limit: Option<i64>,
    /// Page offset.
    pub offset: Option<i64>,
    /// Provider preference (`listenbrainz` default).
    pub source: Option<String>,
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
    path = "/api/v3/discover",
    responses(
        (status = 200, description = "Discover shelves", body = DiscoverResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_discover(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
) -> Result<Json<DiscoverResponse>, ReadsError> {
    services::discover(&deps, &user.user_id)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Trigger a background discover rebuild for the caller.
#[utoipa::path(
    post,
    path = "/api/v3/discover/refresh",
    responses(
        (status = 202, description = "Refresh triggered", body = RefreshResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn refresh_discover(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
) -> Result<(StatusCode, Json<RefreshResponse>), ReadsError> {
    services::refresh_discover(&deps, &user.user_id)
        .map(|body| (StatusCode::ACCEPTED, Json(body)))
        .map_err(|error| map(error, &deps))
}

/// Record one discover interaction for personalization.
#[utoipa::path(
    post,
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
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<DiscoverActivityRequest>,
) -> Result<Json<DiscoverActivityResponse>, ReadsError> {
    services::record_activity(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Generate one radio shelf from a seed.
#[utoipa::path(
    post,
    path = "/api/v3/discover/radio",
    request_body = RadioRequest,
    responses(
        (status = 200, description = "Radio shelf", body = ChartSection),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn discover_radio(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<RadioRequest>,
) -> Result<Json<ChartSection>, ReadsError> {
    services::radio_shelf(&deps, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Build a complete track-level radio plan.
#[utoipa::path(
    post,
    path = "/api/v3/discover/radio/plan",
    request_body = RadioPlanRequest,
    responses(
        (status = 200, description = "Radio plan", body = RadioPlanResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn radio_plan(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<RadioPlanRequest>,
) -> Result<Json<RadioPlanResponse>, ReadsError> {
    services::radio_plan(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Suggest rows extending one playlist.
#[utoipa::path(
    post,
    path = "/api/v3/discover/playlist-suggestions",
    request_body = PlaylistSuggestionsRequest,
    responses(
        (status = 200, description = "Playlist suggestions", body = PlaylistSuggestionsResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn playlist_suggestions(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<PlaylistSuggestionsRequest>,
) -> Result<Json<PlaylistSuggestionsResponse>, ReadsError> {
    services::playlist_suggestions(&deps, &user.user_id, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// The queue deck: a live build when one exists, else a lightweight build.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue",
    params(("count" = Option<i64>, Query, description = "Wanted card count, max 20")),
    responses(
        (status = 200, description = "Queue deck", body = DiscoverQueueResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_queue(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<QueueQuery>,
) -> Result<Json<DiscoverQueueResponse>, ReadsError> {
    services::queue(&deps, &user.user_id, query.count)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Current queue build status for polling.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue/status",
    responses(
        (status = 200, description = "Queue build status", body = DiscoverQueueStatusResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn queue_status(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
) -> Json<DiscoverQueueStatusResponse> {
    Json(services::queue_status(&deps, &user.user_id))
}

/// Trigger a queue build.
#[utoipa::path(
    post,
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
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<QueueGenerateRequest>,
) -> Json<QueueGenerateResponse> {
    Json(services::queue_generate(&deps, &user.user_id, &body))
}

/// Enrichment behind one queue card.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue/enrich/{release_group_mbid}",
    params(("release_group_mbid" = String, Path, description = "Release-group id")),
    responses(
        (status = 200, description = "Card enrichment", body = QueueEnrichment),
        (status = 400, description = "Bad release-group id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn enrich_queue_item(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    Path(release_group_mbid): Path<String>,
) -> Result<Json<QueueEnrichment>, ReadsError> {
    services::enrich_queue_item(&deps, &release_group_mbid)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// On-demand preview behind one queue card.
#[utoipa::path(
    post,
    path = "/api/v3/discover/queue/preview/{release_group_mbid}",
    params(("release_group_mbid" = String, Path, description = "Release-group id")),
    responses(
        (status = 200, description = "Card preview", body = DiscoverQueuePreview),
        (status = 400, description = "Bad release-group id"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn preview_queue_item(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    Path(release_group_mbid): Path<String>,
) -> Result<Json<DiscoverQueuePreview>, ReadsError> {
    services::preview_queue_item(&deps, &release_group_mbid)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Ignore one release: ledger it, rebuild the queue, refresh discover.
#[utoipa::path(
    post,
    path = "/api/v3/discover/queue/ignore",
    request_body = QueueIgnoreRequest,
    responses(
        (status = 204, description = "Release ignored"),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn ignore_queue_item(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<QueueIgnoreRequest>,
) -> Result<StatusCode, ReadsError> {
    services::ignore_queue_item(&deps, &user.user_id, &body)
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| map(error, &deps))
}

/// The caller's ignore ledger, newest first.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue/ignored",
    responses(
        (status = 200, description = "Ignore ledger", body = IgnoredReleasesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn ignored_releases(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
) -> Json<IgnoredReleasesResponse> {
    Json(services::ignored_releases(&deps, &user.user_id))
}

/// Library membership behind the given cards.
#[utoipa::path(
    post,
    path = "/api/v3/discover/queue/validate",
    request_body = QueueValidateRequest,
    responses(
        (status = 200, description = "Library membership", body = QueueValidateResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn validate_queue(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<QueueValidateRequest>,
) -> Result<Json<QueueValidateResponse>, ReadsError> {
    services::validate_queue(&deps, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Album video lookup with the cached flag.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue/youtube-search",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("album" = String, Query, description = "Album name"),
    ),
    responses(
        (status = 200, description = "Resolved video", body = YouTubeSearchResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn youtube_search(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<YouTubeAlbumQuery>,
) -> Result<Json<YouTubeSearchResponse>, ReadsError> {
    services::youtube_search(&deps, &query.artist, &query.album)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Track video lookup with the cached flag.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue/youtube-track-search",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("track" = String, Query, description = "Track name"),
    ),
    responses(
        (status = 200, description = "Resolved video", body = YouTubeSearchResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn youtube_track_search(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<YouTubeTrackQuery>,
) -> Result<Json<YouTubeSearchResponse>, ReadsError> {
    services::youtube_track_search(&deps, &query.artist, &query.track)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// YouTube data-API quota state. 404 when YouTube is unconfigured.
#[utoipa::path(
    get,
    path = "/api/v3/discover/queue/youtube-quota",
    responses(
        (status = 200, description = "Quota state", body = YouTubeQuotaResponse),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "YouTube unconfigured"),
    )
)]
pub async fn youtube_quota(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
) -> Result<Json<YouTubeQuotaResponse>, ReadsError> {
    services::youtube_quota(&deps)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Bulk YouTube cache membership. Unconfigured YouTube answers empty.
#[utoipa::path(
    post,
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
    _user: AuthenticatedUser,
    ValidJson(body): ValidJson<TrackCacheCheckRequest>,
) -> Result<Json<TrackCacheCheckResponse>, ReadsError> {
    services::youtube_cache_check(&deps, &body)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// A 30-second track preview. Empty means no provider had one.
#[utoipa::path(
    get,
    path = "/api/v3/discover/track-preview",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("track" = String, Query, description = "Track name"),
    ),
    responses(
        (status = 200, description = "Track preview", body = TrackPreviewResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn track_preview(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<TrackPreviewQuery>,
) -> Result<Json<TrackPreviewResponse>, ReadsError> {
    services::track_preview(&deps, &query.artist, &query.track)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Ordered 30-second samples of an album's first tracks.
#[utoipa::path(
    get,
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
    )
)]
pub async fn album_preview(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<AlbumPreviewQuery>,
) -> Result<Json<AlbumPreviewResponse>, ReadsError> {
    services::album_preview(&deps, &query.artist, &query.album, query.count)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

// ---------------------------------------------------------------------------
// Batches
// ---------------------------------------------------------------------------

/// Create a discovery batch: one request per album.
#[utoipa::path(
    post,
    path = "/api/v3/discover/batches",
    request_body = DiscoveryBatchCreate,
    responses(
        (status = 202, description = "Batch created", body = DiscoveryBatchDetail),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn create_batch(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
    ValidJson(body): ValidJson<DiscoveryBatchCreate>,
) -> Result<(StatusCode, Json<DiscoveryBatchDetail>), ReadsError> {
    services::create_batch(&deps, &user.user_id, &body)
        .map(|detail| (StatusCode::ACCEPTED, Json(detail)))
        .map_err(|error| map(error, &deps))
}

/// The caller's batches, newest first.
#[utoipa::path(
    get,
    path = "/api/v3/discover/batches",
    responses(
        (status = 200, description = "Batch list", body = DiscoveryBatchListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_batches(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
) -> Json<DiscoveryBatchListResponse> {
    Json(services::list_batches(&deps, &user.user_id))
}

/// One batch. Foreign ids read as missing.
#[utoipa::path(
    get,
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
    user: AuthenticatedUser,
    Path(batch_id): Path<String>,
) -> Result<Json<DiscoveryBatchDetail>, ReadsError> {
    services::get_batch(&deps, &user.user_id, &batch_id)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Remove one batch. Foreign ids read as missing.
#[utoipa::path(
    delete,
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
    user: AuthenticatedUser,
    Path(batch_id): Path<String>,
    ValidQuery(query): ValidQuery<RemoveBatchQuery>,
) -> Result<Json<DiscoveryBatchRemoveResult>, ReadsError> {
    services::remove_batch(
        &deps,
        &user.user_id,
        &batch_id,
        query.remove_albums.unwrap_or(true),
    )
    .map(Json)
    .map_err(|error| map(error, &deps))
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

/// Cached home shelves for the caller.
#[utoipa::path(
    get,
    path = "/api/v3/home",
    responses(
        (status = 200, description = "Home shelves", body = HomeResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_home(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
) -> Result<Json<HomeResponse>, ReadsError> {
    services::home(&deps, &user.user_id)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Integration availability behind the shelves.
#[utoipa::path(
    get,
    path = "/api/v3/home/integration-status",
    responses(
        (status = 200, description = "Integration availability", body = IntegrationStatus),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn integration_status(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
) -> Result<Json<IntegrationStatus>, ReadsError> {
    services::integration_status(&deps)
        .map(Json)
        .map_err(|error| map(error, &deps))
}

/// Genre detail with owned and popular rows.
#[utoipa::path(
    get,
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
    _user: AuthenticatedUser,
    Path(genre_name): Path<String>,
    ValidQuery(query): ValidQuery<GenreQuery>,
) -> Result<Json<GenreDetailResponse>, ReadsError> {
    services::genre_detail(
        &deps,
        &genre_name,
        query.limit,
        query.artist_offset,
        query.album_offset,
    )
    .map(Json)
    .map_err(|error| map(error, &deps))
}

/// One trending-artists page. Range rides the `range` query param (one
/// route replaces the v2 base plus `/{range_key}` pair).
#[utoipa::path(
    get,
    path = "/api/v3/home/trending/artists",
    params(
        ("range" = Option<String>, Query, description = "Range key, this_week default"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
        ("source" = Option<String>, Query, description = "listenbrainz or lastfm"),
    ),
    responses(
        (status = 200, description = "Trending artists page", body = TrendingArtistsPage),
        (status = 400, description = "Bad range or source"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn trending_artists(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<ChartQuery>,
) -> Result<Json<TrendingArtistsPage>, ReadsError> {
    services::trending_artists(
        &deps,
        query.range.as_deref(),
        query.limit,
        query.offset,
        query.source.as_deref(),
    )
    .map(Json)
    .map_err(|error| map(error, &deps))
}

/// One popular-albums page (same range-param redesign).
#[utoipa::path(
    get,
    path = "/api/v3/home/popular/albums",
    params(
        ("range" = Option<String>, Query, description = "Range key, this_week default"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
        ("source" = Option<String>, Query, description = "listenbrainz or lastfm"),
    ),
    responses(
        (status = 200, description = "Popular albums page", body = PopularAlbumsPage),
        (status = 400, description = "Bad range or source"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn popular_albums(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<ChartQuery>,
) -> Result<Json<PopularAlbumsPage>, ReadsError> {
    services::popular_albums(
        &deps,
        query.range.as_deref(),
        query.limit,
        query.offset,
        query.source.as_deref(),
    )
    .map(Json)
    .map_err(|error| map(error, &deps))
}

/// One your-top-albums page for the caller (same redesign, per-user).
#[utoipa::path(
    get,
    path = "/api/v3/home/your-top/albums",
    params(
        ("range" = Option<String>, Query, description = "Range key, this_week default"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("offset" = Option<i64>, Query, description = "Page offset"),
        ("source" = Option<String>, Query, description = "listenbrainz or lastfm"),
    ),
    responses(
        (status = 200, description = "Your-top albums page", body = PopularAlbumsPage),
        (status = 400, description = "Bad range or source"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn your_top_albums(
    State(deps): State<ReadsDeps>,
    user: AuthenticatedUser,
    ValidQuery(query): ValidQuery<ChartQuery>,
) -> Result<Json<PopularAlbumsPage>, ReadsError> {
    services::your_top_albums(
        &deps,
        &user.user_id,
        query.range.as_deref(),
        query.limit,
        query.offset,
        query.source.as_deref(),
    )
    .map(Json)
    .map_err(|error| map(error, &deps))
}

// ---------------------------------------------------------------------------
// Now playing
// ---------------------------------------------------------------------------

/// The live now-playing snapshot across users. Presence writes
/// (POST/DELETE) land in stage 6; this slice only reads.
#[utoipa::path(
    get,
    path = "/api/v3/now-playing",
    responses(
        (status = 200, description = "Live snapshot", body = NowPlayingSnapshot),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn now_playing(
    State(deps): State<ReadsDeps>,
    _user: AuthenticatedUser,
) -> Json<NowPlayingSnapshot> {
    Json(services::now_playing(&deps))
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// Discover routes: shelves, radio, queue, batches. Mount under `/api/v3`.
pub fn discover_router(deps: ReadsDeps) -> axum::Router {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/discover", get(get_discover))
        .route("/discover/refresh", post(refresh_discover))
        .route("/discover/activity", post(record_activity))
        .route("/discover/radio", post(discover_radio))
        .route("/discover/radio/plan", post(radio_plan))
        .route("/discover/playlist-suggestions", post(playlist_suggestions))
        .route("/discover/queue", get(get_queue))
        .route("/discover/queue/status", get(queue_status))
        .route("/discover/queue/generate", post(queue_generate))
        .route(
            "/discover/queue/enrich/{release_group_mbid}",
            get(enrich_queue_item),
        )
        .route(
            "/discover/queue/preview/{release_group_mbid}",
            post(preview_queue_item),
        )
        .route("/discover/queue/ignore", post(ignore_queue_item))
        .route("/discover/queue/ignored", get(ignored_releases))
        .route("/discover/queue/validate", post(validate_queue))
        .route("/discover/queue/youtube-search", get(youtube_search))
        .route(
            "/discover/queue/youtube-track-search",
            get(youtube_track_search),
        )
        .route("/discover/queue/youtube-quota", get(youtube_quota))
        .route(
            "/discover/queue/youtube-cache-check",
            post(youtube_cache_check),
        )
        .route("/discover/track-preview", get(track_preview))
        .route("/discover/album-preview", get(album_preview))
        .route("/discover/batches", post(create_batch).get(list_batches))
        .route(
            "/discover/batches/{batch_id}",
            get(get_batch).delete(remove_batch),
        )
        .with_state(deps)
}

/// Home routes: shelves, genre, and the redesigned chart pages.
pub fn home_router(deps: ReadsDeps) -> axum::Router {
    use axum::routing::get;

    axum::Router::new()
        .route("/home", get(get_home))
        .route("/home/integration-status", get(integration_status))
        .route("/home/genre/{genre_name}", get(genre_detail))
        .route("/home/trending/artists", get(trending_artists))
        .route("/home/popular/albums", get(popular_albums))
        .route("/home/your-top/albums", get(your_top_albums))
        .with_state(deps)
}

/// Now-playing snapshot route. Presence writes are stage 6, not here.
pub fn now_playing_router(deps: ReadsDeps) -> axum::Router {
    use axum::routing::get;

    axum::Router::new()
        .route("/now-playing", get(now_playing))
        .with_state(deps)
}

/// Every route in this slice. Mount under `/api/v3` behind the session
/// gate; see `mod.rs` for the wiring note.
///
/// `GET /now-playing` is deliberately absent: stage 6 serves it from the
/// live presence registry (same JSON shape), and merging both routers
/// would panic on the duplicate route.
pub fn reads_router(deps: ReadsDeps) -> axum::Router {
    axum::Router::new()
        .merge(discover_router(deps.clone()))
        .merge(home_router(deps))
}
