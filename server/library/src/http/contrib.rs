//! Library contribution routes: carry a local album to MusicBrainz.
//!
//! Thirteen session-gated routes under `/api/v3/library` drive the draft,
//! the Discogs source, the duplicate check, the seeded release editor and
//! verification (v2 `library_contributions.py`). The release editor sends
//! the curator's browser back to a public callback, served at the v3 path
//! and at the v1 path a seed opened before the upgrade still points at.
//!
//! Every failure answers with the shared envelope plus an action:
//! `{"error": {"code", "message", "details": {"action"}}}`, so the page can
//! say what went wrong and what to do about it.

use axum::{
    Json, Router,
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Deserialize;
use serde_json::json;
use utoipa::{IntoParams, ToSchema};

use crate::contrib::error::ContribError;
use crate::contrib::models::{
    ContributionRecord, DiscogsReleaseCandidate, LEGACY_CALLBACK_PATH, MusicBrainzSeed,
    ReleaseDraft,
};
use crate::contrib::service::ContributionService;
use crate::wiring::LibraryServices;
use droppedneedle_auth::{Curator, User};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// DTOs (v2 `api/v1/schemas/library_contributions.py`)
// ---------------------------------------------------------------------------

/// Save an edited draft.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ContributionDraftBody {
    /// Row revision the caller last read.
    pub expected_row_revision: i64,
    /// The whole draft.
    pub draft: ReleaseDraft,
}

/// A revision guard alone (rebuild, cancel, remove Discogs, seed, verify).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ContributionRevisionBody {
    /// Row revision the caller last read.
    pub expected_row_revision: i64,
}

/// Search Discogs; an empty query searches the album's artist and title.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct DiscogsSearchBody {
    /// Title, artist, barcode, URL or ID.
    #[serde(default)]
    pub query: Option<String>,
}

/// Discogs search results.
#[derive(Debug, Clone, serde::Serialize, ToSchema)]
pub struct DiscogsSearchResponse {
    /// Up to eight candidate releases.
    pub results: Vec<DiscogsReleaseCandidate>,
}

/// Use one Discogs release as the contribution's source.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DiscogsSelectBody {
    /// Row revision the caller last read.
    pub expected_row_revision: i64,
    /// Discogs release ID or release URL.
    pub release_id_or_url: String,
}

/// Run the MusicBrainz duplicate check.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DuplicateCheckBody {
    /// Row revision the caller last read.
    pub expected_row_revision: i64,
    /// The curator confirmed the similar releases are other editions.
    #[serde(default)]
    pub different_edition_confirmed: bool,
}

/// Link the album to a release already on MusicBrainz.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AttachExistingBody {
    /// Row revision the caller last read.
    pub expected_row_revision: i64,
    /// A release from the duplicate-check result.
    pub release_mbid: String,
}

/// Record the release MusicBrainz created, by hand.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MusicBrainzResultBody {
    /// Row revision the caller last read.
    pub expected_row_revision: i64,
    /// Release MBID or release URL.
    pub release_id_or_url: String,
    /// Replace a different result already recorded.
    #[serde(default)]
    pub replace_existing_result: bool,
}

/// What the MusicBrainz release editor sends back.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MusicBrainzCallbackQuery {
    /// The one-time token the seed carried.
    pub token: Option<String>,
    /// The release MusicBrainz created.
    pub release_mbid: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

impl From<ContribError> for ApiError {
    fn from(error: ContribError) -> Self {
        let error = match error {
            ContribError::Busy(cause) => {
                return ApiError::unavailable(Unavailable::ServerBusy, cause);
            }
            ContribError::ProviderUnavailable(provider)
            | ContribError::DiscogsUnavailable(provider) => return provider.into(),
            other => other,
        };
        let status = match &error {
            ContribError::ContributionNotFound
            | ContribError::AlbumNotFound
            | ContribError::Missing(_) => StatusCode::NOT_FOUND,
            ContribError::Validation(_) => StatusCode::BAD_REQUEST,
            ContribError::State(_)
            | ContribError::Stale(_)
            | ContribError::ProviderExpired(_)
            | ContribError::DuplicateCheckRequired(_)
            | ContribError::ExactDuplicate(_)
            | ContribError::ResultMismatch(_) => StatusCode::CONFLICT,
            ContribError::ProviderUnavailable(_)
            | ContribError::DiscogsUnavailable(_)
            | ContribError::Busy(_) => StatusCode::SERVICE_UNAVAILABLE,
            ContribError::ProviderUnmappable | ContribError::DiscogsUnusable => {
                StatusCode::BAD_GATEWAY
            }
            ContribError::Data(_) | ContribError::Storage(_) | ContribError::TokenUnavailable => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        if status.is_server_error() {
            return ApiError::fault(status, format!("{}: {error:?}", error.code()));
        }
        ApiError::new(status, error.code(), error.to_string())
            .with_details(json!({ "action": error.action() }))
    }
}

type Answer<T> = Result<Json<T>, ApiError>;

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// The contribution routes. The callback is public: the one-time token
/// identifies the contribution, as in v2's public route.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .curator(routes!(create_contribution))
        .user(routes!(get_contribution))
        .curator(routes!(update_draft))
        .curator(routes!(rebuild))
        .curator(routes!(cancel))
        .curator(routes!(search_discogs))
        .curator(routes!(select_discogs))
        .curator(routes!(remove_discogs))
        .curator(routes!(check_duplicates))
        .curator(routes!(attach_existing))
        .curator(routes!(create_seed))
        .curator(routes!(record_result))
        .curator(routes!(retry_verification))
        .public(routes!(musicbrainz_callback))
}

impl LibraryServices {
    /// The MusicBrainz callback at its v1 path, for a seed opened before the
    /// upgrade. Mounted outside the session gate, like the v3 one.
    pub fn legacy_contrib_callback_router(&self) -> Router {
        Router::new()
            .route(LEGACY_CALLBACK_PATH, get(musicbrainz_callback))
            .with_state(self.contrib.clone())
    }
}

/// Start (or reopen) the contribution for one local album.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/albums/{id}/contributions",
    params(("id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "The album's active contribution", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Album not found or has no indexed tracks"),
        (status = 409, description = "The album already has an exact release, or changed"),
    )
)]
pub async fn create_contribution(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(album_id): Path<String>,
) -> Answer<ContributionRecord> {
    Ok(Json(contrib.create(&album_id, caller.user_id()).await?))
}

/// Read one contribution.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}",
    params(("id" = String, Path, description = "Contribution id")),
    responses(
        (status = 200, description = "Contribution", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Contribution not found"),
    )
)]
pub async fn get_contribution(
    State(contrib): State<Arc<ContributionService>>,
    _caller: User,
    Path(id): Path<String>,
) -> Answer<ContributionRecord> {
    Ok(Json(contrib.get(&id).await?))
}

/// Save the edited draft.
#[utoipa::path(
    put,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/draft",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = ContributionDraftBody,
    responses(
        (status = 200, description = "Updated contribution", body = ContributionRecord),
        (status = 400, description = "Invalid draft value"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Changed since read, or no longer editable"),
    )
)]
pub async fn update_draft(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ContributionDraftBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .update(
                &id,
                body.expected_row_revision,
                &body.draft,
                caller.user_id(),
            )
            .await?,
    ))
}

/// Rebuild a stale contribution from the album's current files.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/rebuild",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = ContributionRevisionBody,
    responses(
        (status = 200, description = "The new draft", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution or album not found"),
        (status = 409, description = "Not stale, or changed since read"),
    )
)]
pub async fn rebuild(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ContributionRevisionBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .rebuild(&id, body.expected_row_revision, caller.user_id())
            .await?,
    ))
}

/// Cancel a contribution.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/cancel",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = ContributionRevisionBody,
    responses(
        (status = 200, description = "Cancelled contribution", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Already closed, or changed since read"),
    )
)]
pub async fn cancel(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ContributionRevisionBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .cancel(&id, body.expected_row_revision, caller.user_id())
            .await?,
    ))
}

/// Search Discogs releases for this contribution.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/discogs/search",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = DiscogsSearchBody,
    responses(
        (status = 200, description = "Candidate releases", body = DiscogsSearchResponse),
        (status = 400, description = "Query too short or too long"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "The contribution is closed"),
        (status = 503, description = "Discogs unavailable"),
    )
)]
pub async fn search_discogs(
    State(contrib): State<Arc<ContributionService>>,
    _caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<DiscogsSearchBody>,
) -> Answer<DiscogsSearchResponse> {
    let results = contrib.search_discogs(&id, body.query.as_deref()).await?;
    Ok(Json(DiscogsSearchResponse { results }))
}

/// Use a Discogs release as the source.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/discogs/select",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = DiscogsSelectBody,
    responses(
        (status = 200, description = "Updated contribution", body = ContributionRecord),
        (status = 400, description = "Not a Discogs release ID or URL"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution or Discogs release not found"),
        (status = 409, description = "Changed since read, or no longer editable"),
        (status = 503, description = "Discogs unavailable"),
    )
)]
pub async fn select_discogs(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<DiscogsSelectBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .select_discogs(
                &id,
                &body.release_id_or_url,
                body.expected_row_revision,
                caller.user_id(),
            )
            .await?,
    ))
}

/// Drop the Discogs source and its values.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/discogs/remove",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = ContributionRevisionBody,
    responses(
        (status = 200, description = "Updated contribution", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Changed since read, or no longer editable"),
    )
)]
pub async fn remove_discogs(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ContributionRevisionBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .remove_discogs(&id, body.expected_row_revision, caller.user_id())
            .await?,
    ))
}

/// Check MusicBrainz for releases this would duplicate.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/musicbrainz/duplicates",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = DuplicateCheckBody,
    responses(
        (status = 200, description = "Contribution with the duplicate result", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Draft incomplete, source expired, or changed since read"),
        (status = 503, description = "MusicBrainz unavailable"),
    )
)]
pub async fn check_duplicates(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<DuplicateCheckBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .check_duplicates(
                &id,
                body.expected_row_revision,
                caller.user_id(),
                body.different_edition_confirmed,
            )
            .await?,
    ))
}

/// Link the album to a release already on MusicBrainz.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/musicbrainz/attach",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = AttachExistingBody,
    responses(
        (status = 200, description = "Linked contribution", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution or release not found"),
        (status = 409, description = "Not in the duplicate result, or does not match safely"),
        (status = 503, description = "MusicBrainz unavailable"),
    )
)]
pub async fn attach_existing(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<AttachExistingBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .attach_existing(
                &id,
                &body.release_mbid,
                body.expected_row_revision,
                caller.user_id(),
            )
            .await?,
    ))
}

/// Build the form that opens the seeded MusicBrainz release editor. The
/// browser POSTs it; MusicBrainz sends the curator back to the callback.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/musicbrainz/seed",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = ContributionRevisionBody,
    responses(
        (status = 200, description = "Release editor form", body = MusicBrainzSeed),
        (status = 400, description = "The public server address is not usable"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Duplicate check missing or found an exact release"),
        (status = 503, description = "MusicBrainz unavailable"),
    )
)]
pub async fn create_seed(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    OriginalUri(original): OriginalUri,
    headers: HeaderMap,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ContributionRevisionBody>,
) -> Answer<MusicBrainzSeed> {
    let base_url = format!(
        "{}{}",
        public_origin(&headers),
        base_path(original.path(), "/api/v3/")
    );
    Ok(Json(
        contrib
            .create_seed(&id, body.expected_row_revision, caller.user_id(), &base_url)
            .await?,
    ))
}

/// Record the release MusicBrainz created, by hand.
#[utoipa::path(
    put,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/musicbrainz/result",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = MusicBrainzResultBody,
    responses(
        (status = 200, description = "Contribution, now verifying", body = ContributionRecord),
        (status = 400, description = "Not a release MBID or URL"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Not waiting for a result, or a different one exists"),
    )
)]
pub async fn record_result(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<MusicBrainzResultBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .record_manual_result(
                &id,
                &body.release_id_or_url,
                body.expected_row_revision,
                caller.user_id(),
                body.replace_existing_result,
            )
            .await?,
    ))
}

/// Queue the recorded release for verification again.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/contributions/{id}/musicbrainz/verify",
    params(("id" = String, Path, description = "Contribution id")),
    request_body = ContributionRevisionBody,
    responses(
        (status = 200, description = "Contribution, now verifying", body = ContributionRecord),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "Contribution not found"),
        (status = 409, description = "Nothing to verify, or changed since read"),
    )
)]
pub async fn retry_verification(
    State(contrib): State<Arc<ContributionService>>,
    caller: Curator,
    Path(id): Path<String>,
    ValidJson(body): ValidJson<ContributionRevisionBody>,
) -> Answer<ContributionRecord> {
    Ok(Json(
        contrib
            .retry_verification(&id, body.expected_row_revision, caller.user_id())
            .await?,
    ))
}

/// Where the MusicBrainz release editor sends the curator back. Public:
/// the one-time token identifies the contribution. Every outcome is a 303
/// to the web app: the contribution page on success, the library page
/// with `musicbrainz=callback-error` otherwise (v2's contract).
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/contributions/musicbrainz/callback",
    params(MusicBrainzCallbackQuery),
    responses((status = 303, description = "Redirect to the contribution or library page"))
)]
pub async fn musicbrainz_callback(
    State(contrib): State<Arc<ContributionService>>,
    OriginalUri(original): OriginalUri,
    ValidQuery(query): ValidQuery<MusicBrainzCallbackQuery>,
) -> Response {
    let path = original.path();
    let api = if path.contains("/api/v1/") {
        "/api/v1/"
    } else {
        "/api/v3/"
    };
    let base = base_path(path, api);
    let target = match contrib
        .consume_callback(query.token.as_deref(), query.release_mbid.as_deref())
        .await
    {
        Ok(contribution_id) => {
            format!("{base}/library/contributions/{contribution_id}?musicbrainz=returned")
        }
        Err(error) => {
            tracing::info!(code = error.code(), %error, "MusicBrainz callback refused");
            format!("{base}/library?musicbrainz=callback-error")
        }
    };
    let mut response = Redirect::to(&target).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

/// The deployment base path: everything before the API prefix in the path
/// the browser asked for (`""` at the domain root, `/music` under a base).
fn base_path<'a>(original_path: &'a str, api_prefix: &str) -> &'a str {
    original_path
        .find(api_prefix)
        .map_or("", |index| &original_path[..index])
}

/// The origin the curator's browser uses for this server: the `Origin`
/// header a same-site POST carries, else the forwarded or direct host.
/// MusicBrainz sends the browser back here, so it must be the address
/// the browser knows, not one the server guesses.
fn public_origin(headers: &HeaderMap) -> String {
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(',').next().unwrap_or("").trim().to_owned())
            .filter(|value| !value.is_empty() && value != "null")
    };
    if let Some(origin) = text("origin")
        && (origin.starts_with("https://") || origin.starts_with("http://"))
    {
        return origin.trim_end_matches('/').to_owned();
    }
    let scheme = text("x-forwarded-proto")
        .filter(|proto| proto.eq_ignore_ascii_case("https"))
        .map_or("http", |_| "https");
    let host = text("x-forwarded-host")
        .or_else(|| text("host"))
        .unwrap_or_else(|| "localhost".to_owned());
    format!("{scheme}://{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_path_and_origin_follow_the_browser() {
        assert_eq!(base_path("/api/v3/library/contributions/x", "/api/v3/"), "");
        assert_eq!(
            base_path(
                "/music/api/v1/library/contributions/musicbrainz/callback",
                "/api/v1/"
            ),
            "/music"
        );
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("lan.example:8080"));
        assert_eq!(public_origin(&headers), "http://lan.example:8080");
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("music.example"),
        );
        assert_eq!(public_origin(&headers), "https://music.example");
        headers.insert(
            "origin",
            HeaderValue::from_static("https://music.example.org"),
        );
        assert_eq!(public_origin(&headers), "https://music.example.org");
    }
}
