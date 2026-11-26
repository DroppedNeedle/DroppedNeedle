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
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::page::{Page, PageRequest};
use serde::Deserialize;

use super::{
    engine::{archive, weekly},
    later,
    models::{
        AlbumPreviewMode, AlbumPreviewResponse, AlbumRef, ArtistRef, ChartPage, ChartSection,
        DigFurtherPick, DiscoverActivityRequest, DiscoverActivityResponse, DiscoverFeedbackEntry,
        DiscoverFeedbackRequest, DiscoverFeedbackResponse, DiscoverHealthResponse,
        DiscoverPickEventRequest, DiscoverPrefs, DiscoverResponse, DiscoverTasteResponse,
        DiscoverTuningRequest, DiscoverTuningsResponse, FeedbackTarget, FoundOnDiscover,
        GenreDetailResponse, HomeResponse, IntegrationStatus, LaterAlbum, LaterRequest,
        ListenSource, NewRelease, PastWeeksPage, PickAnswer, PickEvidence,
        PlaylistSuggestionsRequest, PlaylistSuggestionsResponse, RadioPlanRequest,
        RadioPlanResponse, RadioRequest, StatedArtist, StatedArtistRequest, StatedArtistsResponse,
        StatsRange, TrackPreviewResponse, TuningKind, WeekResponse, YouTubeSearchResponse,
        YourLastfmResponse,
    },
    new_releases,
    services::{self, DiscoverDeps},
};

// ---------------------------------------------------------------------------
// Query shapes
// ---------------------------------------------------------------------------

/// Chart page params shared by the three range routes.
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

/// `GET /discover/later` params.
#[derive(Debug, Deserialize)]
pub struct LaterQuery {
    /// Albums per page, 1 to 100 (default 50); more is read as 100.
    pub limit: Option<u32>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
    /// Only this album, by its release-group id: a page of one row or none.
    pub release_group_mbid: Option<String>,
}

/// `DELETE /discover/tunings/{kind}/{target}` params.
#[derive(Debug, Deserialize)]
pub struct UndoTuningQuery {
    /// Every row of that kind said for the same group, not this one alone.
    #[serde(default)]
    pub group: bool,
}

/// `GET /discover/new-releases` params.
#[derive(Debug, Deserialize)]
pub struct NewReleasesQuery {
    /// Releases per page, 1 to 100 (default 50); more is read as 100.
    pub limit: Option<u32>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

/// `GET /discover/week/more` params.
#[derive(Debug, Deserialize)]
pub struct WeekMoreQuery {
    /// Albums per page, 1 to 50 (default 20); more is read as 50.
    pub limit: Option<u32>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
}

/// `GET /discover/weeks` params.
#[derive(Debug, Deserialize)]
pub struct PastWeeksQuery {
    /// Weeks per page, 1 to 20 (default 8); more is read as 20.
    pub limit: Option<u32>,
    /// `next_cursor` from the previous page.
    pub cursor: Option<String>,
    /// Only picks with this answer.
    pub state: Option<PickAnswer>,
    /// Only picks whose title or artist holds this.
    pub q: Option<String>,
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
    /// Wanted sample count (1-8, default 4), in the `first` mode.
    pub count: Option<i64>,
    /// Which tracks to answer, `first` by default.
    #[serde(default)]
    pub mode: AlbumPreviewMode,
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
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<DiscoverResponse>, ApiError> {
    services::discover(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Every user's latest discover page numbers: counts only, no music
/// named.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/admin/discover/health",
    responses(
        (status = 200, description = "Discover health numbers", body = DiscoverHealthResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn admin_discover_health(
    State(deps): State<DiscoverDeps>,
    admin: Admin,
) -> Result<Json<DiscoverHealthResponse>, ApiError> {
    services::discover_health(&deps, &admin)
        .await
        .map(Json)
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
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn record_activity(
    State(deps): State<DiscoverDeps>,
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
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn discover_radio(
    State(deps): State<DiscoverDeps>,
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
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn radio_plan(
    State(deps): State<DiscoverDeps>,
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
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn playlist_suggestions(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidJson(body): ValidJson<PlaylistSuggestionsRequest>,
) -> Result<Json<PlaylistSuggestionsResponse>, ApiError> {
    services::playlist_suggestions(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Record one thing the caller tells Discover about an album or an
/// artist; a later answer about the same one replaces it. Every
/// recommending surface leaves out what it hides.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/feedback",
    request_body = DiscoverFeedbackRequest,
    responses(
        (status = 200, description = "The answer as stored", body = DiscoverFeedbackEntry),
        (status = 400, description = "Bad request body, or no id for its target"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The answer could not be saved"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn save_feedback(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoverFeedbackRequest>,
) -> Result<Json<DiscoverFeedbackEntry>, ApiError> {
    services::save_feedback(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Everything the caller told Discover that is in effect, newest first.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/feedback",
    responses(
        (status = 200, description = "The caller's answers", body = DiscoverFeedbackResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The answers could not be read"),
    )
)]
pub async fn list_feedback(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<DiscoverFeedbackResponse>, ApiError> {
    services::list_feedback(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Take back one thing the caller told Discover, so what it hid may come
/// back. Answers the same when there was no such answer.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/feedback/{target}/{mbid}",
    params(
        ("target" = FeedbackTarget, Path, description = "What the answer is about"),
        ("mbid" = String, Path, description = "The album's release-group id or the artist's MusicBrainz id"),
    ),
    responses(
        (status = 204, description = "No such answer is in effect"),
        (status = 400, description = "Not a target or not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The change could not be saved"),
    )
)]
pub async fn undo_feedback(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path((target, mbid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    services::undo_feedback(&deps, user.user_id(), &target, &mbid)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Put one album on the caller's Later pile; marking it again changes
/// nothing. Nothing is requested, and it is not offered again as a pick
/// while it is on the pile.
#[utoipa::path(
    put,
    tag = "discover",
    path = "/api/v3/discover/later/{release_group_mbid}",
    params(
        ("release_group_mbid" = String, Path, description = "The album's MusicBrainz release-group id"),
    ),
    request_body = LaterRequest,
    responses(
        (status = 204, description = "The album is on the pile"),
        (status = 400, description = "Bad request body, or not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The change could not be saved"),
    )
)]
pub async fn mark_later(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path(release_group_mbid): Path<String>,
    ValidJson(body): ValidJson<LaterRequest>,
) -> Result<StatusCode, ApiError> {
    services::mark_later(&deps, user.user_id(), &release_group_mbid, &body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Take one album off the caller's Later pile. Answers the same when it
/// was not there.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/later/{release_group_mbid}",
    params(
        ("release_group_mbid" = String, Path, description = "The album's MusicBrainz release-group id"),
    ),
    responses(
        (status = 204, description = "The album is not on the pile"),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The change could not be saved"),
    )
)]
pub async fn remove_later(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path(release_group_mbid): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::remove_later(&deps, user.user_id(), &release_group_mbid)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// The caller's Later pile, newest first, a page at a time. An album the
/// library owns is not listed.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/later",
    params(
        ("limit" = Option<u32>, Query, description = "Albums per page, 1 to 100; more is read as 100"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` from the previous page"),
        ("release_group_mbid" = Option<String>, Query, description = "Only this album: a page of one row, or none when it is not on the pile"),
    ),
    responses(
        (status = 200, description = "One page of the pile, newest first, with how many albums it lists", body = Page<LaterAlbum>),
        (status = 400, description = "Bad cursor, or not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The pile could not be read"),
    )
)]
pub async fn list_later(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<LaterQuery>,
) -> Result<Json<Page<LaterAlbum>>, ApiError> {
    let paging = PageRequest::read(
        query.limit,
        query.cursor.as_deref(),
        later::PAGE_DEFAULT,
        later::PAGE_MAX,
    )?;
    services::later_page(
        &deps,
        user.user_id(),
        paging,
        query.release_group_mbid.as_deref(),
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// The caller's new releases from their artists: every release of the
/// last 90 days by an artist they play, the library owns, they follow or
/// that is close to theirs, newest first, a page at a time, each marked
/// owned or requested. Read from kept rows; nothing is asked of a
/// provider.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/new-releases",
    params(
        ("limit" = Option<u32>, Query, description = "Releases per page, 1 to 100; more is read as 100"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` from the previous page"),
    ),
    responses(
        (status = 200, description = "One page of the list, newest first, with how many it holds", body = Page<NewRelease>),
        (status = 400, description = "Bad cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The list could not be read"),
    )
)]
pub async fn list_new_releases(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<NewReleasesQuery>,
) -> Result<Json<Page<NewRelease>>, ApiError> {
    let paging = PageRequest::read(
        query.limit,
        query.cursor.as_deref(),
        new_releases::PAGE_DEFAULT,
        new_releases::PAGE_MAX,
    )?;
    services::new_releases_page(&deps, user.user_id(), paging)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The caller's week: whether this week's set is out, being set out,
/// waiting for their history or kept from last week while a service is
/// down, and the set to show with what became of each pick. Read from the
/// caller's own rows alone.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/week",
    responses(
        (status = 200, description = "The caller's week", body = WeekResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The week could not be read"),
    )
)]
pub async fn get_week(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<WeekResponse>, ApiError> {
    services::week(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// "Dig further": the caller's pool past their newest set, in the order
/// the set was dealt from, a page at a time.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/week/more",
    params(
        ("limit" = Option<u32>, Query, description = "Albums per page, 1 to 50; more is read as 50"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` from the previous page"),
    ),
    responses(
        (status = 200, description = "One page past the set, with how many there are", body = Page<DigFurtherPick>),
        (status = 400, description = "Bad cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The pool could not be read"),
    )
)]
pub async fn week_more(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<WeekMoreQuery>,
) -> Result<Json<Page<DigFurtherPick>>, ApiError> {
    let paging = PageRequest::read(
        query.limit,
        query.cursor.as_deref(),
        weekly::MORE_PAGE_DEFAULT,
        weekly::MORE_PAGE_MAX,
    )?;
    services::week_more(&deps, user.user_id(), paging)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The caller's past weeks, newest first, a page of weeks at a time: each
/// set as it was shown, with what became of every pick and the week's
/// tally. `state` and `q` narrow the picks, and a week with none left is
/// not listed. The first page also carries the record of every set.
/// Read from the caller's own rows alone.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/weeks",
    params(
        ("limit" = Option<u32>, Query, description = "Weeks per page, 1 to 20; more is read as 20"),
        ("cursor" = Option<String>, Query, description = "`next_cursor` from the previous page"),
        ("state" = Option<PickAnswer>, Query, description = "Only picks with this answer"),
        ("q" = Option<String>, Query, description = "Only picks whose title or artist holds this"),
    ),
    responses(
        (status = 200, description = "One page of weeks, newest first", body = PastWeeksPage),
        (status = 400, description = "Bad cursor, answer or search"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The weeks could not be read"),
    )
)]
pub async fn list_past_weeks(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidQuery(query): ValidQuery<PastWeeksQuery>,
) -> Result<Json<PastWeeksPage>, ApiError> {
    let paging = PageRequest::read(
        query.limit,
        query.cursor.as_deref(),
        archive::PAGE_DEFAULT,
        archive::PAGE_MAX,
    )?;
    services::past_weeks(
        &deps,
        user.user_id(),
        paging,
        query.state,
        query.q.as_deref(),
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// The albums the caller kept from their weekly sets, each with the week
/// it was first picked, newest first. Read from the caller's own rows
/// alone.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/found",
    responses(
        (status = 200, description = "The caller's kept picks", body = FoundOnDiscover),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The picks could not be read"),
    )
)]
pub async fn found_on_discover(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<FoundOnDiscover>, ApiError> {
    services::found_on_discover(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The caller's set day.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/prefs",
    responses(
        (status = 200, description = "The caller's set day", body = DiscoverPrefs),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The set day could not be read"),
    )
)]
pub async fn get_discover_prefs(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<DiscoverPrefs>, ApiError> {
    services::discover_prefs(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Keep the caller's set day. This week's set stays as it is; the new day
/// counts from the first such day after it.
#[utoipa::path(
    put,
    tag = "discover",
    path = "/api/v3/discover/prefs",
    request_body = DiscoverPrefs,
    responses(
        (status = 200, description = "The set day as kept", body = DiscoverPrefs),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The set day could not be saved"),
    )
)]
pub async fn save_discover_prefs(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoverPrefs>,
) -> Result<Json<DiscoverPrefs>, ApiError> {
    services::save_discover_prefs(&deps, user.user_id(), body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Record what the web UI saw happen to one of the caller's picks: shown
/// (counted once a UTC day), its sample started, or its album page opened
/// from the card. An album Discover never offered the caller is left alone.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/picks/events",
    request_body = DiscoverPickEventRequest,
    responses(
        (status = 204, description = "Recorded, or nothing to record"),
        (status = 400, description = "Bad request body, or not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The event could not be saved"),
    )
)]
pub async fn record_pick_event(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoverPickEventRequest>,
) -> Result<StatusCode, ApiError> {
    services::record_pick_event(&deps, user.user_id(), &body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Everything behind one of the caller's picks: its line, the caller's
/// artists that led to it, whether it is new to them, how well known it is
/// and how it was found. A pick of one of the caller's weekly sets answers
/// as it was built for that set; any other is read from the caller's own
/// pool.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/picks/{release_group_mbid}/why",
    params(
        ("release_group_mbid" = String, Path, description = "The pick's MusicBrainz release-group id"),
    ),
    responses(
        (status = 200, description = "The pick's evidence", body = PickEvidence),
        (status = 400, description = "Not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Not a pick of the caller's with a reason that holds"),
        (status = 500, description = "The evidence could not be read"),
    )
)]
pub async fn pick_why(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path(release_group_mbid): Path<String>,
) -> Result<Json<PickEvidence>, ApiError> {
    services::pick_evidence(&deps, user.user_id(), &release_group_mbid)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Keep one way the caller tunes what Discover offers them, about one of
/// their artists or every artist of one of their taste groups: less or
/// more from it, or that it was a phase. An artist keeps one of these at a
/// time; in a group, an artist with one of its own keeps it. The other
/// kinds are refused. Their pool is built again, after their taste for a
/// phase.
#[utoipa::path(
    post,
    tag = "discover",
    path = "/api/v3/discover/tunings",
    request_body = DiscoverTuningRequest,
    responses(
        (status = 200, description = "The rows as stored: one, or one for each artist of the group", body = DiscoverTuningsResponse),
        (status = 400, description = "Bad request body, a kind not taken yet, neither or both of a target and a group, or not one of the caller's artists or groups"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The tuning could not be saved"),
    )
)]
pub async fn save_tuning(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    ValidJson(body): ValidJson<DiscoverTuningRequest>,
) -> Result<Json<DiscoverTuningsResponse>, ApiError> {
    services::save_tuning(&deps, user.user_id(), &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Every way the caller tuned what Discover offers them, newest first.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/tunings",
    responses(
        (status = 200, description = "The caller's tunings", body = DiscoverTuningsResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The tunings could not be read"),
    )
)]
pub async fn list_tunings(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<DiscoverTuningsResponse>, ApiError> {
    services::list_tunings(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The caller's taste as Discover reads it: their groups, largest share
/// first, each with its artists and what the caller said about them, what
/// it was worked out from, and whether a change they made is still being
/// followed. The artists they named are listed by their own route.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/taste",
    responses(
        (status = 200, description = "The caller's taste", body = DiscoverTasteResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The taste could not be read"),
    )
)]
pub async fn get_taste(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<DiscoverTasteResponse>, ApiError> {
    services::taste(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// The artists the caller named as ones they love, in the order they named
/// them, and the most they may name.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/stated-artists",
    responses(
        (status = 200, description = "The caller's named artists", body = StatedArtistsResponse),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The list could not be read"),
    )
)]
pub async fn list_stated_artists(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<StatedArtistsResponse>, ApiError> {
    services::list_stated(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Name one artist the caller loves, for Discover to grow from. It is not
/// a follow: nothing is watched or requested. Naming it again changes
/// nothing. Their taste is built again.
#[utoipa::path(
    put,
    tag = "discover",
    path = "/api/v3/discover/stated-artists/{artist_mbid}",
    params(
        ("artist_mbid" = String, Path, description = "The artist's MusicBrainz id"),
    ),
    request_body = StatedArtistRequest,
    responses(
        (status = 200, description = "The artist as stored", body = StatedArtist),
        (status = 400, description = "Bad request body, not a MusicBrainz artist id, a special-purpose id, or the list is full"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The change could not be saved"),
    )
)]
pub async fn name_stated_artist(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<StatedArtistRequest>,
) -> Result<Json<StatedArtist>, ApiError> {
    services::name_stated(&deps, user.user_id(), &artist_mbid, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Take one artist off the caller's list. Their taste is built again.
/// Answers the same when it was not there.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/stated-artists/{artist_mbid}",
    params(
        ("artist_mbid" = String, Path, description = "The artist's MusicBrainz id"),
    ),
    responses(
        (status = 204, description = "The artist is not on the list"),
        (status = 400, description = "Not a MusicBrainz artist id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The change could not be saved"),
    )
)]
pub async fn remove_stated_artist(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path(artist_mbid): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::remove_stated(&deps, user.user_id(), &artist_mbid)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Take back one way the caller tuned Discover, or with `group` the whole
/// statement it was part of. Their pool is built again, after their taste
/// for a phase. Answers the same when there was no such tuning.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/tunings/{kind}/{target}",
    params(
        ("kind" = TuningKind, Path, description = "What the tuning is"),
        ("target" = String, Path, description = "What it is about: an artist's MusicBrainz id"),
        ("group" = Option<bool>, Query, description = "Every row of that kind said for the same taste group as this one"),
    ),
    responses(
        (status = 204, description = "No such tuning is in effect"),
        (status = 400, description = "Not a kind taken yet, or not a MusicBrainz id"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "The change could not be saved"),
    )
)]
pub async fn undo_tuning(
    State(deps): State<DiscoverDeps>,
    User(user): User,
    Path((kind, target)): Path<(String, String)>,
    ValidQuery(query): ValidQuery<UndoTuningQuery>,
) -> Result<StatusCode, ApiError> {
    services::undo_tuning(&deps, user.user_id(), &kind, &target, query.group)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Forget the caller's history: how their plays ended, the albums
/// Discover offered them and what came of each, and their saved Discover
/// pages, which are built again. Plays, play counts, the library, requests
/// and what they told Discover stay. A failure leaves the steps before it
/// done; asking again finishes the rest.
#[utoipa::path(
    delete,
    tag = "discover",
    path = "/api/v3/discover/history",
    responses(
        (status = 204, description = "Forgotten"),
        (status = 401, description = "Not authenticated"),
        (status = 500, description = "Not all of it could be removed; asking again finishes it"),
    )
)]
pub async fn forget_history(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<StatusCode, ApiError> {
    services::forget_history(&deps, user.user_id())
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn youtube_track_search(
    State(deps): State<DiscoverDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<YouTubeTrackQuery>,
) -> Result<Json<YouTubeSearchResponse>, ApiError> {
    services::youtube_track_search(&deps, &query.artist, &query.track)
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
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn track_preview(
    State(deps): State<DiscoverDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<TrackPreviewQuery>,
) -> Result<Json<TrackPreviewResponse>, ApiError> {
    services::track_preview(&deps, &query.artist, &query.track)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// 30-second samples of an album: its first tracks in album order, or in the
/// audition mode its opener, best-known track and a deep cut.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/discover/album-preview",
    params(
        ("artist" = String, Query, description = "Artist name"),
        ("album" = String, Query, description = "Album name"),
        ("count" = Option<i64>, Query, description = "Wanted samples in the first mode, 1-8"),
        ("mode" = Option<AlbumPreviewMode>, Query, description = "first (default) or audition"),
    ),
    responses(
        (status = 200, description = "Album samples", body = AlbumPreviewResponse),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn album_preview(
    State(deps): State<DiscoverDeps>,
    _user: User,
    ValidQuery(query): ValidQuery<AlbumPreviewQuery>,
) -> Result<Json<AlbumPreviewResponse>, ApiError> {
    services::album_preview(&deps, &query.artist, &query.album, query.count, query.mode)
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
    State(deps): State<DiscoverDeps>,
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
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn integration_status(
    State(deps): State<DiscoverDeps>,
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
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn genre_detail(
    State(deps): State<DiscoverDeps>,
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

/// One trending-artists page. Range rides the `range` query param.
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
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn trending_artists(
    State(deps): State<DiscoverDeps>,
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

/// One popular-albums page, with the range param.
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
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn popular_albums(
    State(deps): State<DiscoverDeps>,
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

/// One your-top-albums page for the caller.
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
        (status = 409, description = "Not configured (NOT_CONFIGURED)"),
        (status = 503, description = "A source is down (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn your_top_albums(
    State(deps): State<DiscoverDeps>,
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

/// The caller's own Last.fm lists for their charts page: recent
/// scrobbles and this week's top albums and artists. Empty for a person
/// without Last.fm linked.
#[utoipa::path(
    get,
    tag = "discover",
    path = "/api/v3/home/your-top/lastfm",
    responses(
        (status = 200, description = "The caller's Last.fm lists", body = YourLastfmResponse),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "Every list's read failed (PROVIDER_UNAVAILABLE names an outside one, with retry_after when known)"),
    )
)]
pub async fn your_lastfm(
    State(deps): State<DiscoverDeps>,
    User(user): User,
) -> Result<Json<YourLastfmResponse>, ApiError> {
    services::your_lastfm(&deps, user.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// Discover and home routes at their full `/api/v3` paths, for any
/// signed-in user. `GET /now-playing` is served by `playback`.
pub fn routes() -> Routes<DiscoverDeps> {
    Routes::new()
        .user(routes!(get_discover))
        .user(routes!(record_activity))
        .user(routes!(discover_radio))
        .user(routes!(radio_plan))
        .user(routes!(playlist_suggestions))
        .user(routes!(save_feedback))
        .user(routes!(list_feedback))
        .user(routes!(undo_feedback))
        .user(routes!(record_pick_event))
        .user(routes!(pick_why))
        .user(routes!(save_tuning))
        .user(routes!(list_tunings))
        .user(routes!(undo_tuning))
        .user(routes!(get_taste))
        .user(routes!(mark_later))
        .user(routes!(remove_later))
        .user(routes!(list_later))
        .user(routes!(list_stated_artists))
        .user(routes!(name_stated_artist))
        .user(routes!(remove_stated_artist))
        .user(routes!(list_new_releases))
        .user(routes!(get_week))
        .user(routes!(week_more))
        .user(routes!(list_past_weeks))
        .user(routes!(found_on_discover))
        .user(routes!(get_discover_prefs))
        .user(routes!(save_discover_prefs))
        .user(routes!(forget_history))
        .user(routes!(youtube_track_search))
        .user(routes!(track_preview))
        .user(routes!(album_preview))
        .user(routes!(get_home))
        .user(routes!(integration_status))
        .user(routes!(genre_detail))
        .user(routes!(trending_artists))
        .user(routes!(popular_albums))
        .user(routes!(your_top_albums))
        .user(routes!(your_lastfm))
        .admin(routes!(admin_discover_health))
}
