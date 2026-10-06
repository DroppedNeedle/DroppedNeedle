//! Domain logic over the discover ports: clamping, validation, and
//! assembly. Services return typed domain failures; handlers map them to
//! HTTP. Provider causes stay strings so nothing internal leaks.

use std::sync::Arc;

use crate::{
    ids::IdGenerator,
    reads::discover::{
        error::ReadsError,
        models::{
            AlbumPreviewResponse, ChartRange, ChartSection, ChartSource, DiscoverActivityResponse,
            DiscoverQueuePreview, DiscoverQueueResponse, DiscoverQueueStatusResponse,
            DiscoverResponse, DiscoveryBatchCreate, DiscoveryBatchDetail,
            DiscoveryBatchListResponse, DiscoveryBatchRemoveResult, DiscoveryBatchSummary,
            GenreDetailResponse, HomeResponse, IgnoredReleasesResponse, IntegrationStatus,
            NowPlayingSnapshot, PlaylistSuggestionsRequest, PlaylistSuggestionsResponse,
            PopularAlbumsPage, QueueEnrichment, QueueGenerateRequest, QueueGenerateResponse,
            QueueIgnoreRequest, QueueValidateRequest, QueueValidateResponse, RadioPlanRequest,
            RadioPlanResponse, RadioRequest, RefreshResponse, TrackCacheCheckRequest,
            TrackCacheCheckResponse, TrackCacheCheckResponseItem, TrackPreviewResponse,
            TrendingArtistsPage, YouTubeQuotaResponse, YouTubeSearchResponse,
        },
        ports::{
            BatchItemRow, BatchStore, ChartsSource, Clock, DiscoverContent, NowPlayingStore,
            PreviewSource, ProviderFailure, QueueStore, RadioPlanner, YouTubeSource,
        },
    },
};

/// Max cards served from one queue read.
pub const QUEUE_COUNT_MAX: i64 = 20;
/// Max rows on a chart page.
pub const CHART_RANGE_LIMIT_MAX: i64 = 100;
/// Max album-preview samples.
pub const ALBUM_PREVIEW_MAX: i64 = 8;
/// Max pairs per cache-check call.
pub const CACHE_CHECK_MAX_ITEMS: usize = 100;
/// Max artist/track string length accepted anywhere in discover.
pub const NAME_MAX_LEN: usize = 200;
/// Max radio plan tracks.
pub const RADIO_PLAN_MAX: i64 = 100;
/// Max radio shelf rows.
pub const RADIO_SHELF_MAX: i64 = 50;

/// Domain failures. Handlers map these to [`ReadsError`]; the mapping is
/// the only place HTTP statuses are chosen.
#[derive(Debug, Clone)]
pub enum ServiceError {
    /// Unknown or foreign id.
    NotFound,
    /// Bad input with a user-facing reason.
    InvalidInput(String),
    /// A provider failed; the cause is log-only.
    Upstream(String),
    /// The store failed; the cause is log-only.
    Internal(String),
    /// The feature has no working source; the reason is shown.
    NotAvailable(String),
}

impl ServiceError {
    /// Map a domain failure to the wire error, logging server-side causes.
    pub fn into_reads_error(self, ids: &dyn IdGenerator) -> ReadsError {
        match self {
            Self::NotFound => ReadsError::NotFound,
            Self::InvalidInput(message) => ReadsError::InvalidInput { message },
            Self::Upstream(cause) => ReadsError::upstream(&cause, ids),
            Self::Internal(cause) => ReadsError::internal(&cause, ids),
            Self::NotAvailable(what) => ReadsError::NotAvailable {
                message: format!("{what} is not available on this server yet"),
            },
        }
    }
}

impl From<ProviderFailure> for ServiceError {
    fn from(failure: ProviderFailure) -> Self {
        match failure {
            ProviderFailure::Failed(cause) => Self::Upstream(cause),
            ProviderFailure::NotAvailable(what) => Self::NotAvailable(what),
        }
    }
}

/// Map a failure of a store this server owns: a failed read is an internal
/// fault (500), not a provider fault (502).
fn owned_store_failure(failure: ProviderFailure) -> ServiceError {
    match failure {
        ProviderFailure::Failed(cause) => ServiceError::Internal(cause),
        ProviderFailure::NotAvailable(what) => ServiceError::NotAvailable(what),
    }
}

/// All dependencies behind discover, injected by constructor.
#[derive(Clone)]
pub struct ReadsDeps {
    /// Shelf content.
    pub content: Arc<dyn DiscoverContent>,
    /// Queue builds and ignore ledger.
    pub queues: Arc<dyn QueueStore>,
    /// Discovery batches.
    pub batches: Arc<dyn BatchStore>,
    /// Chart pages and genre detail.
    pub charts: Arc<dyn ChartsSource>,
    /// 30-second previews.
    pub previews: Arc<dyn PreviewSource>,
    /// YouTube lookups.
    pub youtube: Arc<dyn YouTubeSource>,
    /// Radio plans and suggestions.
    pub radio: Arc<dyn RadioPlanner>,
    /// Live presence.
    pub now_playing: Arc<dyn NowPlayingStore>,
    /// Fresh error ids.
    pub ids: Arc<dyn IdGenerator>,
    /// Clock for TTL math.
    pub clock: Arc<dyn Clock>,
}

/// Clamp a limit into `1..=max`, defaulting when missing.
fn clamp_limit(value: Option<i64>, default: i64, max: i64) -> i64 {
    value.unwrap_or(default).clamp(1, max)
}

/// Parse a `range` query value, defaulting to this week. Unknown values
/// fail: v2 silently fell back, which hid client bugs.
pub fn parse_range(value: Option<&str>) -> Result<ChartRange, ServiceError> {
    match value {
        None => Ok(ChartRange::default()),
        Some(raw) => ChartRange::parse(raw).ok_or_else(|| {
            ServiceError::InvalidInput(format!(
                "Unknown range '{raw}'; want this_week, this_month, this_year, or all_time"
            ))
        }),
    }
}

/// Parse a `source` query value, defaulting to ListenBrainz.
pub fn parse_source(value: Option<&str>) -> Result<ChartSource, ServiceError> {
    match value {
        None => Ok(ChartSource::default()),
        Some(raw) => ChartSource::parse(raw).ok_or_else(|| {
            ServiceError::InvalidInput(format!(
                "Unknown source '{raw}'; want listenbrainz or lastfm"
            ))
        }),
    }
}

/// Reject blank or overlong names with a user-facing reason.
fn check_name(value: &str, what: &str) -> Result<(), ServiceError> {
    if value.trim().is_empty() {
        return Err(ServiceError::InvalidInput(format!(
            "{what} must not be blank"
        )));
    }
    if value.len() > NAME_MAX_LEN {
        return Err(ServiceError::InvalidInput(format!(
            "{what} must be at most {NAME_MAX_LEN} characters"
        )));
    }
    Ok(())
}

/// Cached discover shelves for the user. The cache is ours, so a failed
/// read is an internal fault (500), not a provider fault (502).
pub async fn discover(deps: &ReadsDeps, user_id: &str) -> Result<DiscoverResponse, ServiceError> {
    deps.content
        .discover(user_id)
        .await
        .map_err(owned_store_failure)
}

/// Trigger a background discover rebuild for the user.
pub async fn refresh_discover(
    deps: &ReadsDeps,
    user_id: &str,
) -> Result<RefreshResponse, ServiceError> {
    deps.content.trigger_refresh(user_id).await?;
    Ok(RefreshResponse {
        status: "ok".to_owned(),
        message: "Discover refresh triggered".to_owned(),
    })
}

/// Record one discover interaction.
pub async fn record_activity(
    deps: &ReadsDeps,
    user_id: &str,
    body: &crate::reads::discover::models::DiscoverActivityRequest,
) -> Result<DiscoverActivityResponse, ServiceError> {
    match body.feature.as_str() {
        "home" | "discover" | "queue" | "artist" => {}
        other => {
            return Err(ServiceError::InvalidInput(format!(
                "Unknown feature '{other}'; want home, discover, queue, or artist"
            )));
        }
    }
    if let Some(section) = body.section.as_deref()
        && !matches!(section, "similar" | "top_songs" | "top_albums")
    {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown section '{section}'; want similar, top_songs, or top_albums"
        )));
    }
    if let Some(provider) = body.provider.as_deref()
        && !matches!(provider, "lastfm" | "listenbrainz")
    {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown provider '{provider}'; want lastfm or listenbrainz"
        )));
    }
    Ok(deps
        .content
        .record_activity(
            user_id,
            &body.feature,
            body.artist_mbid.as_deref(),
            body.section.as_deref(),
            body.provider.as_deref(),
        )
        .await?)
}

/// One radio shelf.
pub async fn radio_shelf(
    deps: &ReadsDeps,
    body: &RadioRequest,
) -> Result<ChartSection, ServiceError> {
    if !matches!(body.seed_type.as_str(), "artist" | "album" | "genre") {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown seed type '{}'; want artist, album, or genre",
            body.seed_type
        )));
    }
    if body.seed_id.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "seed_id must not be blank".to_owned(),
        ));
    }
    let source = parse_source(body.source.as_deref())?;
    let count = body.count.clamp(1, RADIO_SHELF_MAX);
    Ok(deps
        .radio
        .shelf(&body.seed_type, &body.seed_id, count, source)
        .await?)
}

/// One complete radio plan.
pub async fn radio_plan(
    deps: &ReadsDeps,
    user_id: &str,
    body: &RadioPlanRequest,
) -> Result<RadioPlanResponse, ServiceError> {
    if !matches!(
        body.seed_type.as_str(),
        "artist" | "album" | "genre" | "items"
    ) {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown seed type '{}'; want artist, album, genre, or items",
            body.seed_type
        )));
    }
    if body.seed_type != "items" && body.seed_id.as_deref().unwrap_or("").trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "seed_id must not be blank for artist, album, and genre plans".to_owned(),
        ));
    }
    if !matches!(body.mode.as_str(), "library" | "hybrid") {
        return Err(ServiceError::InvalidInput(format!(
            "Unknown mode '{}'; want library or hybrid",
            body.mode
        )));
    }
    let count = body.count.clamp(1, RADIO_PLAN_MAX);
    Ok(deps
        .radio
        .plan(
            user_id,
            &body.seed_type,
            body.seed_id.as_deref(),
            &body.mode,
            count,
            &body.exclude_recording_mbids,
        )
        .await?)
}

/// Suggestions extending one playlist.
pub async fn playlist_suggestions(
    deps: &ReadsDeps,
    user_id: &str,
    body: &PlaylistSuggestionsRequest,
) -> Result<PlaylistSuggestionsResponse, ServiceError> {
    if body.playlist_id.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "playlist_id must not be blank".to_owned(),
        ));
    }
    let source = parse_source(body.source.as_deref())?;
    let count = body.count.clamp(1, RADIO_SHELF_MAX);
    Ok(deps
        .radio
        .playlist_suggestions(user_id, &body.playlist_id, count, source)
        .await?)
}

/// The queue deck: a live build when one exists, else a lightweight build.
pub async fn queue(
    deps: &ReadsDeps,
    user_id: &str,
    count: Option<i64>,
) -> Result<DiscoverQueueResponse, ServiceError> {
    if let Some(build) = deps.queues.consume(user_id) {
        let items = match count {
            Some(wanted) => {
                let take = wanted.clamp(1, QUEUE_COUNT_MAX) as usize;
                build.items.into_iter().take(take).collect()
            }
            None => build.items,
        };
        return Ok(DiscoverQueueResponse {
            items,
            queue_id: build.queue_id,
        });
    }
    let clamped = count.map(|wanted| wanted.clamp(1, QUEUE_COUNT_MAX));
    let build = deps.queues.build_lightweight(user_id, clamped)?;
    Ok(DiscoverQueueResponse {
        items: build.items,
        queue_id: build.queue_id,
    })
}

/// Current queue build status.
pub fn queue_status(deps: &ReadsDeps, user_id: &str) -> DiscoverQueueStatusResponse {
    deps.queues.ensure_loaded(user_id);
    let build = deps.queues.status(user_id);
    DiscoverQueueStatusResponse {
        status: build.status,
        queue_id: Some(build.queue_id),
        item_count: Some(build.items.len() as i64),
        built_at: Some(build.built_at),
        stale: Some(is_stale(deps, build.built_at)),
        error: build.error,
    }
}

/// True when the build timestamp is older than the queue TTL.
fn is_stale(deps: &ReadsDeps, built_at: i64) -> bool {
    deps.clock.now_unix() - built_at > crate::reads::discover::refresh::QUEUE_TTL_SECS
}

/// Trigger a queue build.
pub fn queue_generate(
    deps: &ReadsDeps,
    user_id: &str,
    body: &QueueGenerateRequest,
) -> QueueGenerateResponse {
    deps.queues.start_build(user_id, body.force)
}

/// Enrichment behind one queue card.
pub async fn enrich_queue_item(
    deps: &ReadsDeps,
    release_group_mbid: &str,
) -> Result<QueueEnrichment, ServiceError> {
    if release_group_mbid.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "release_group_mbid must not be blank".to_owned(),
        ));
    }
    Ok(deps.content.enrich_queue_item(release_group_mbid).await?)
}

/// On-demand preview behind one queue card.
pub async fn preview_queue_item(
    deps: &ReadsDeps,
    release_group_mbid: &str,
) -> Result<DiscoverQueuePreview, ServiceError> {
    if release_group_mbid.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "release_group_mbid must not be blank".to_owned(),
        ));
    }
    Ok(deps.content.preview_queue_item(release_group_mbid).await?)
}

/// Ignore one release: ledger it, rebuild the queue, refresh discover.
pub async fn ignore_queue_item(
    deps: &ReadsDeps,
    user_id: &str,
    body: &QueueIgnoreRequest,
) -> Result<(), ServiceError> {
    if body.release_group_mbid.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "release_group_mbid must not be blank".to_owned(),
        ));
    }
    deps.queues.ignore_release(
        user_id,
        &body.release_group_mbid,
        &body.artist_mbid,
        &body.release_name,
        &body.artist_name,
    );
    deps.queues.start_build(user_id, true);
    deps.content.trigger_refresh(user_id).await?;
    Ok(())
}

/// The user's ignore ledger.
pub fn ignored_releases(deps: &ReadsDeps, user_id: &str) -> IgnoredReleasesResponse {
    IgnoredReleasesResponse {
        items: deps
            .queues
            .ignored(user_id)
            .into_iter()
            .map(|row| crate::reads::discover::models::IgnoredRelease {
                release_group_mbid: row.release_group_mbid,
                artist_mbid: row.artist_mbid,
                release_name: row.release_name,
                artist_name: row.artist_name,
                ignored_at: row.ignored_at,
            })
            .collect(),
    }
}

/// Library membership behind the given cards.
pub async fn validate_queue(
    deps: &ReadsDeps,
    body: &QueueValidateRequest,
) -> Result<QueueValidateResponse, ServiceError> {
    if body.release_group_mbids.len() > CACHE_CHECK_MAX_ITEMS {
        return Err(ServiceError::InvalidInput(format!(
            "At most {CACHE_CHECK_MAX_ITEMS} ids per call"
        )));
    }
    Ok(QueueValidateResponse {
        in_library: deps
            .content
            .validate_queue_mbids(&body.release_group_mbids)
            .await?,
    })
}

/// Album video lookup with the cached flag.
pub async fn youtube_search(
    deps: &ReadsDeps,
    artist: &str,
    album: &str,
) -> Result<YouTubeSearchResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(album, "album")?;
    let cached = deps.youtube.is_cached(artist, album, false);
    match deps.youtube.search_video(artist, album).await? {
        Some(video_id) => Ok(YouTubeSearchResponse {
            video_id: Some(video_id.clone()),
            embed_url: Some(format!("https://www.youtube.com/embed/{video_id}")),
            error: None,
            cached,
        }),
        None => Ok(YouTubeSearchResponse {
            video_id: None,
            embed_url: None,
            error: Some("not_found".to_owned()),
            cached,
        }),
    }
}

/// Track video lookup with the cached flag.
pub async fn youtube_track_search(
    deps: &ReadsDeps,
    artist: &str,
    track: &str,
) -> Result<YouTubeSearchResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(track, "track")?;
    let cached = deps.youtube.is_cached(artist, track, true);
    match deps.youtube.search_track(artist, track).await? {
        Some(video_id) => Ok(YouTubeSearchResponse {
            video_id: Some(video_id.clone()),
            embed_url: Some(format!("https://www.youtube.com/embed/{video_id}")),
            error: None,
            cached,
        }),
        None => Ok(YouTubeSearchResponse {
            video_id: None,
            embed_url: None,
            error: Some("not_found".to_owned()),
            cached,
        }),
    }
}

/// Quota state, or missing when YouTube is unconfigured (the route 404s).
pub async fn youtube_quota(deps: &ReadsDeps) -> Result<YouTubeQuotaResponse, ServiceError> {
    deps.youtube.quota().await.ok_or(ServiceError::NotFound)
}

/// Bulk cache membership. Unconfigured YouTube answers empty, never an
/// error: absence of the integration is not a failure.
pub fn youtube_cache_check(
    deps: &ReadsDeps,
    body: &TrackCacheCheckRequest,
) -> Result<TrackCacheCheckResponse, ServiceError> {
    if !deps.youtube.is_configured() {
        return Ok(TrackCacheCheckResponse { items: Vec::new() });
    }
    let mut seen = std::collections::HashSet::new();
    let mut deduped: Vec<(String, String)> = Vec::new();
    for item in body.items.iter().take(CACHE_CHECK_MAX_ITEMS) {
        let artist: String = item.artist.chars().take(NAME_MAX_LEN).collect();
        let track: String = item.track.chars().take(NAME_MAX_LEN).collect();
        let key = format!("{}|{}", artist.to_lowercase(), track.to_lowercase());
        if seen.insert(key) {
            deduped.push((artist, track));
        }
    }
    let cached = deps.youtube.cached_tracks(&deduped);
    Ok(TrackCacheCheckResponse {
        items: deduped
            .into_iter()
            .map(|(artist, track)| {
                let key = format!("{}|{}", artist.to_lowercase(), track.to_lowercase());
                TrackCacheCheckResponseItem {
                    artist,
                    track,
                    cached: cached.get(&key).copied().unwrap_or(false),
                }
            })
            .collect(),
    })
}

/// A 30-second track preview. Empty means no provider had one.
pub async fn track_preview(
    deps: &ReadsDeps,
    artist: &str,
    track: &str,
) -> Result<TrackPreviewResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(track, "track")?;
    let artist: String = artist.chars().take(NAME_MAX_LEN).collect();
    let track: String = track.chars().take(NAME_MAX_LEN).collect();
    Ok(deps.previews.track_preview(&artist, &track).await?)
}

/// Ordered 30-second album samples.
pub async fn album_preview(
    deps: &ReadsDeps,
    artist: &str,
    album: &str,
    count: Option<i64>,
) -> Result<AlbumPreviewResponse, ServiceError> {
    check_name(artist, "artist")?;
    check_name(album, "album")?;
    let artist: String = artist.chars().take(NAME_MAX_LEN).collect();
    let album: String = album.chars().take(NAME_MAX_LEN).collect();
    let count = clamp_limit(count, 4, ALBUM_PREVIEW_MAX);
    let (tracks, provider) = deps.previews.album_preview(&artist, &album, count).await?;
    Ok(AlbumPreviewResponse { tracks, provider })
}

/// Cached home shelves for the user. Same ownership rule as discover:
/// failed cache reads are internal faults.
pub async fn home(deps: &ReadsDeps, user_id: &str) -> Result<HomeResponse, ServiceError> {
    deps.content
        .home(user_id)
        .await
        .map_err(owned_store_failure)
}

/// Integration availability, refined by a live local-files check that
/// never blanks the row on failure (the cached value wins on error).
pub async fn integration_status(deps: &ReadsDeps) -> Result<IntegrationStatus, ServiceError> {
    let mut status = deps.content.integration_status().await?;
    if let Ok(has_local) = deps.content.has_local_files().await {
        status.localfiles = has_local;
    }
    Ok(status)
}

/// Genre detail with owned and popular rows.
pub async fn genre_detail(
    deps: &ReadsDeps,
    genre: &str,
    limit: Option<i64>,
    artist_offset: Option<i64>,
    album_offset: Option<i64>,
) -> Result<GenreDetailResponse, ServiceError> {
    check_name(genre, "genre")?;
    let limit = clamp_limit(limit, 50, 200);
    let artist_offset = artist_offset.unwrap_or(0).max(0);
    let album_offset = album_offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .genre_detail(genre, limit, artist_offset, album_offset)
        .await?)
}

/// One trending-artists page. This is the range-pair redesign: one route
/// with `?range=` replaces the v2 base route (four embedded ranges) plus
/// the `/{range_key}` route. No `range` means this week; the home root
/// still carries teasers for the overview use case.
pub async fn trending_artists(
    deps: &ReadsDeps,
    range: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
    source: Option<&str>,
) -> Result<TrendingArtistsPage, ServiceError> {
    let range = parse_range(range)?;
    let source = parse_source(source)?;
    let limit = clamp_limit(limit, 25, CHART_RANGE_LIMIT_MAX);
    let offset = offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .trending_artists(range, limit, offset, source)
        .await?)
}

/// One popular-albums page (same redesign as trending artists).
pub async fn popular_albums(
    deps: &ReadsDeps,
    range: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
    source: Option<&str>,
) -> Result<PopularAlbumsPage, ServiceError> {
    let range = parse_range(range)?;
    let source = parse_source(source)?;
    let limit = clamp_limit(limit, 25, CHART_RANGE_LIMIT_MAX);
    let offset = offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .popular_albums(range, limit, offset, source)
        .await?)
}

/// One your-top-albums page for the user (same redesign, per-user).
pub async fn your_top_albums(
    deps: &ReadsDeps,
    user_id: &str,
    range: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
    source: Option<&str>,
) -> Result<PopularAlbumsPage, ServiceError> {
    let range = parse_range(range)?;
    let source = parse_source(source)?;
    let limit = clamp_limit(limit, 25, CHART_RANGE_LIMIT_MAX);
    let offset = offset.unwrap_or(0).max(0);
    Ok(deps
        .charts
        .your_top_albums(user_id, range, limit, offset, source)
        .await?)
}

fn batch_detail(row: &crate::reads::discover::ports::BatchRow) -> DiscoveryBatchDetail {
    let imported = row
        .items
        .iter()
        .filter(|item| item.request_status.as_deref() == Some("imported"))
        .count() as i64;
    DiscoveryBatchDetail {
        id: row.id.clone(),
        name: row.name.clone(),
        source_section: row.source_section.clone(),
        created_at: row.created_at.clone(),
        item_count: row.items.len() as i64,
        imported_count: imported,
        pending_count: row.items.len() as i64 - imported,
        items: row
            .items
            .iter()
            .map(
                |item| crate::reads::discover::models::DiscoveryBatchItemStatus {
                    release_group_mbid: item.release_group_mbid.clone(),
                    artist_mbid: item.artist_mbid.clone(),
                    album_name: item.album_name.clone(),
                    artist_name: item.artist_name.clone(),
                    outcome: item.outcome.clone(),
                    request_status: item.request_status.clone(),
                    in_library: item.in_library,
                },
            )
            .collect(),
    }
}

/// Create a batch: here the rows
/// are stored and echoed with their initial outcomes.
pub fn create_batch(
    deps: &ReadsDeps,
    user_id: &str,
    body: &DiscoveryBatchCreate,
) -> Result<DiscoveryBatchDetail, ServiceError> {
    if body.name.trim().is_empty() {
        return Err(ServiceError::InvalidInput(
            "name must not be blank".to_owned(),
        ));
    }
    if body.items.is_empty() {
        return Err(ServiceError::InvalidInput(
            "items must hold at least one album".to_owned(),
        ));
    }
    let row = deps.batches.create(
        user_id,
        &body.name,
        &body.source_section,
        body.items
            .iter()
            .map(|item| BatchItemRow {
                release_group_mbid: item.release_group_mbid.clone(),
                artist_mbid: item.artist_mbid.clone(),
                album_name: item.album_name.clone(),
                artist_name: item.artist_name.clone(),
                outcome: "requested".to_owned(),
                request_status: Some("pending".to_owned()),
                in_library: false,
            })
            .collect(),
    )?;
    Ok(batch_detail(&row))
}

/// The user's batches, newest first.
pub fn list_batches(deps: &ReadsDeps, user_id: &str) -> DiscoveryBatchListResponse {
    DiscoveryBatchListResponse {
        batches: deps
            .batches
            .list_for_user(user_id)
            .iter()
            .map(|row| DiscoveryBatchSummary {
                id: row.id.clone(),
                name: row.name.clone(),
                source_section: row.source_section.clone(),
                created_at: row.created_at.clone(),
                item_count: row.items.len() as i64,
                imported_count: row
                    .items
                    .iter()
                    .filter(|item| item.request_status.as_deref() == Some("imported"))
                    .count() as i64,
                pending_count: row
                    .items
                    .iter()
                    .filter(|item| item.request_status.as_deref() != Some("imported"))
                    .count() as i64,
            })
            .collect(),
    }
}

/// One batch. Foreign ids read as missing.
pub fn get_batch(
    deps: &ReadsDeps,
    user_id: &str,
    batch_id: &str,
) -> Result<DiscoveryBatchDetail, ServiceError> {
    deps.batches
        .get_for_user(user_id, batch_id)
        .map(|row| batch_detail(&row))
        .ok_or(ServiceError::NotFound)
}

/// Remove one batch. Foreign ids read as missing.
pub fn remove_batch(
    deps: &ReadsDeps,
    user_id: &str,
    batch_id: &str,
    remove_albums: bool,
) -> Result<DiscoveryBatchRemoveResult, ServiceError> {
    deps.batches
        .remove(user_id, batch_id, remove_albums)
        .map(
            |(removed_albums, cancelled_requests, kept)| DiscoveryBatchRemoveResult {
                removed_albums,
                cancelled_requests,
                kept,
            },
        )
        .ok_or(ServiceError::NotFound)
}

/// The live now-playing snapshot across users.
pub fn now_playing(deps: &ReadsDeps) -> NowPlayingSnapshot {
    NowPlayingSnapshot {
        sessions: deps.now_playing.snapshot(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_range_fails_instead_of_falling_back() {
        assert!(parse_range(Some("last_eon")).is_err());
        assert_eq!(parse_range(None).unwrap(), ChartRange::ThisWeek);
        assert_eq!(parse_range(Some("all_time")).unwrap(), ChartRange::AllTime);
    }

    #[test]
    fn unknown_source_fails() {
        assert!(parse_source(Some("deezer")).is_err());
        assert_eq!(parse_source(None).unwrap(), ChartSource::Listenbrainz);
    }

    #[test]
    fn blank_names_fail() {
        assert!(check_name("  ", "artist").is_err());
        assert!(check_name("Portishead", "artist").is_ok());
    }
}
