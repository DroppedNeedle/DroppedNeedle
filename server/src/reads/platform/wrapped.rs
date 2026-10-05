//! Wrapped reads: the keyed external-consumer trio.
//!
//! Carried over from v2 `wrapped.py` (trace A:633-635): user listing, per-user
//! year-in-review, and the server-wide summary. The `X-Wrapped-API-Key`
//! contract is preserved exactly (see [`check_key`]): exact header name,
//! exact value match with no trimming, 401 on missing/wrong/unconfigured,
//! and the v2 rejection message verbatim. Year-in-review data comes from the
//! [`WrappedData`] port; stage 4 ships [`FakeWrappedData`] only, since the
//! ListenBrainz-backed aggregation is stage 5.
//!
//! Unknown users answer 200 with an empty `has_data: false` payload (v2
//! `get_user_wrapped` rule kept, display name falls back to the user id) -
//! never 404. These routes take no session: the shared secret is the only
//! credential, so they mount outside the session middleware.
//!
//! Self-contained on purpose: no `crate::` imports, so this module compiles
//! both inside the wired tree and standalone in the slice tests. The error
//! envelope mirrors `crate::error` exactly.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::future::BoxFuture;
use serde::Serialize;
use utoipa::ToSchema;

/// Shared-secret header, v2 name kept byte for byte.
pub const WRAPPED_API_KEY_HEADER: &str = "x-wrapped-api-key";
/// Rejection message, v2 wording kept verbatim.
pub const WRAPPED_UNAUTHORIZED_MESSAGE: &str = "Invalid or missing wrapped API key";
/// Machine code for key rejections, matching the v3 401 code.
const UNAUTHORIZED: &str = "UNAUTHORIZED";

/// One user row in the listing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedUserSummary {
    /// User id.
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// True when a ListenBrainz account is linked.
    pub has_listenbrainz: bool,
    /// Email when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// User listing answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedUsersResponse {
    /// Year the stats cover.
    pub year: i32,
    /// Every user with its ListenBrainz link status.
    pub users: Vec<WrappedUserSummary>,
}

/// Top artist row.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedArtist {
    /// Artist name.
    pub name: String,
    /// Listen count.
    pub listen_count: i64,
    /// MusicBrainz artist id when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
}

/// Top track row.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedTrack {
    /// Track name.
    pub name: String,
    /// Artist name.
    pub artist_name: String,
    /// Listen count.
    pub listen_count: i64,
}

/// Top album row.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedAlbum {
    /// Album name.
    pub name: String,
    /// Artist name.
    pub artist_name: String,
    /// Listen count.
    pub listen_count: i64,
    /// MusicBrainz release-group id when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
}

/// Top genre row.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedGenre {
    /// Genre label.
    pub genre: String,
    /// Listen count.
    pub listen_count: i64,
}

/// Per-user year-in-review. `loved_tracks_count` and `total_listens_estimated`
/// are both approximations (v2 caveat kept): ListenBrainz caps the loved
/// endpoint at 100 rows per request, and the total sums only the returned
/// top artists rather than every play.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserWrappedResponse {
    /// User id.
    pub user_id: String,
    /// Display name.
    pub display_name: String,
    /// Year the stats cover.
    pub year: i32,
    /// False (with empty lists) when the user has no linked ListenBrainz
    /// account or no listens for the year.
    pub has_data: bool,
    /// Top artists.
    pub top_artists: Vec<WrappedArtist>,
    /// Top tracks.
    pub top_tracks: Vec<WrappedTrack>,
    /// Top albums.
    pub top_albums: Vec<WrappedAlbum>,
    /// Top genres.
    pub top_genres: Vec<WrappedGenre>,
    /// Loved-track sample size (capped at 100 by ListenBrainz), not a total.
    pub loved_tracks_count: i64,
    /// Sum over the returned top artists, not the true play count.
    pub total_listens_estimated: i64,
}

/// Server leaderboard row.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WrappedLeaderboardEntry {
    /// Display name.
    pub display_name: String,
    /// Listen count.
    pub listen_count: i64,
}

/// Server-wide year-in-review.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServerWrappedResponse {
    /// Year the stats cover.
    pub year: i32,
    /// Users included in the stats.
    pub total_users_tracked: i32,
    /// Estimated total listens.
    pub total_listens_estimated: i64,
    /// Per-user listener leaderboard.
    pub leaderboard: Vec<WrappedLeaderboardEntry>,
    /// Sitewide top artist when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_artist_sitewide: Option<WrappedArtist>,
    /// Sitewide top album when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_album_sitewide: Option<WrappedAlbum>,
}

/// Wrapped-data port. Stage 5 aggregates ListenBrainz stats; stage 4 runs
/// against [`FakeWrappedData`].
pub trait WrappedData: Send + Sync + 'static {
    /// Year the stats cover.
    fn current_year(&self) -> i32;
    /// Every user with its ListenBrainz link status.
    fn list_users(&self) -> BoxFuture<'_, Vec<WrappedUserSummary>>;
    /// Per-user stats, or `None` when the user is unknown or has no data.
    fn user_wrapped(&self, user_id: &str) -> BoxFuture<'_, Option<UserWrappedResponse>>;
    /// Server-wide stats.
    fn server_wrapped(&self) -> BoxFuture<'_, ServerWrappedResponse>;
}

/// Fake wrapped data for stage 4.
#[derive(Debug, Clone)]
pub struct FakeWrappedData {
    year: i32,
    users: Vec<WrappedUserSummary>,
    per_user: HashMap<String, UserWrappedResponse>,
    server: ServerWrappedResponse,
}

impl FakeWrappedData {
    /// Build a fake from its parts.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(
        year: i32,
        users: Vec<WrappedUserSummary>,
        per_user: HashMap<String, UserWrappedResponse>,
        server: ServerWrappedResponse,
    ) -> Self {
        Self {
            year,
            users,
            per_user,
            server,
        }
    }

    /// Empty fake: no users, zeroed server stats.
    pub fn empty(year: i32) -> Self {
        Self {
            year,
            users: Vec::new(),
            per_user: HashMap::new(),
            server: ServerWrappedResponse {
                year,
                total_users_tracked: 0,
                total_listens_estimated: 0,
                leaderboard: Vec::new(),
                top_artist_sitewide: None,
                top_album_sitewide: None,
            },
        }
    }
}

impl WrappedData for FakeWrappedData {
    fn current_year(&self) -> i32 {
        self.year
    }

    fn list_users(&self) -> BoxFuture<'_, Vec<WrappedUserSummary>> {
        let users = self.users.clone();
        Box::pin(async move { users })
    }

    fn user_wrapped(&self, user_id: &str) -> BoxFuture<'_, Option<UserWrappedResponse>> {
        let found = self.per_user.get(user_id).cloned();
        Box::pin(async move { found })
    }

    fn server_wrapped(&self) -> BoxFuture<'_, ServerWrappedResponse> {
        let server = self.server.clone();
        Box::pin(async move { server })
    }
}

/// Handler state: the expected shared secret plus the data port. The secret
/// arrives decrypted from settings at wiring; an empty secret fails every
/// request closed (v2 `not expected` rule kept).
#[derive(Clone)]
pub struct WrappedState {
    /// Expected shared secret; empty means unconfigured (deny all).
    pub api_key: String,
    /// Wrapped data (fake in stage 4, ListenBrainz aggregation in stage 5).
    pub data: Arc<dyn WrappedData>,
}

impl WrappedState {
    /// Wire the state from the expected secret and any port implementation.
    pub fn new(api_key: String, data: Arc<dyn WrappedData>) -> Self {
        Self { api_key, data }
    }
}

/// Routes for this slice, relative paths for nesting under `/api/v3`.
/// Mount outside the session middleware: the shared secret is the only
/// credential and must never ride the session allowlist (allowlisted means
/// public, which these routes are not).
pub fn routes(state: WrappedState) -> Router {
    Router::new()
        .route("/wrapped/users", get(get_wrapped_users))
        .route("/wrapped/user/{user_id}", get(get_wrapped_user))
        .route("/wrapped/server", get(get_wrapped_server))
        .with_state(state)
}

/// Key check, v2 `verify_wrapped_api_key` semantics kept byte for byte:
/// the header name is matched case-insensitively (HTTP rule, same as v2 via
/// Starlette), the value is compared exactly with no trimming, an empty
/// expected secret denies everything, and a missing or non-matching header
/// denies. Only an exact match passes.
pub fn check_key(headers: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    headers
        .get(WRAPPED_API_KEY_HEADER)
        .is_some_and(|value| value.as_bytes() == expected.as_bytes())
}

/// Shared error envelope, byte-identical in shape to `crate::error`.
#[derive(Debug, Clone, Serialize, ToSchema)]
struct WrappedErrorBody {
    code: String,
    message: String,
    details: Option<serde_json::Value>,
}

/// Shared error envelope, byte-identical in shape to `crate::error`.
#[derive(Debug, Clone, Serialize, ToSchema)]
struct WrappedErrorEnvelope {
    error: WrappedErrorBody,
}

/// Key-gate rejection. Status 401 with the v2 message verbatim, and
/// deliberately no `WWW-Authenticate` header: v2 sends none, and a Bearer
/// challenge would misdescribe a shared-secret header scheme.
fn wrapped_rejection() -> Response {
    let body = WrappedErrorEnvelope {
        error: WrappedErrorBody {
            code: UNAUTHORIZED.to_owned(),
            message: WRAPPED_UNAUTHORIZED_MESSAGE.to_owned(),
            details: None,
        },
    };
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

/// Extractor enforcing the wrapped key gate before the handler runs.
pub struct WrappedKey;

impl FromRequestParts<WrappedState> for WrappedKey {
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        state: &WrappedState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let allowed = check_key(&parts.headers, &state.api_key);
        async move {
            if allowed {
                Ok(Self)
            } else {
                Err(wrapped_rejection())
            }
        }
    }
}

/// Empty per-user payload, v2 `get_user_wrapped` rule kept: unknown or
/// unlinked users get `has_data: false` with empty lists, and the display
/// name falls back to the user id.
fn empty_user_wrapped(user_id: &str, year: i32) -> UserWrappedResponse {
    UserWrappedResponse {
        user_id: user_id.to_owned(),
        display_name: user_id.to_owned(),
        year,
        has_data: false,
        top_artists: Vec::new(),
        top_tracks: Vec::new(),
        top_albums: Vec::new(),
        top_genres: Vec::new(),
        loved_tracks_count: 0,
        total_listens_estimated: 0,
    }
}

/// Every user with its ListenBrainz link status. Callers match users by
/// email and request `/user/{user_id}` only for `has_listenbrainz` ones.
#[utoipa::path(
    get,
    path = "/api/v3/wrapped/users",
    responses(
        (status = 200, description = "User listing", body = WrappedUsersResponse),
        (status = 401, description = "Invalid or missing wrapped API key"),
    )
)]
pub async fn get_wrapped_users(
    State(state): State<WrappedState>,
    _: WrappedKey,
) -> Json<WrappedUsersResponse> {
    let users = state.data.list_users().await;
    Json(WrappedUsersResponse {
        year: state.data.current_year(),
        users,
    })
}

/// Per-user year-in-review summary.
#[utoipa::path(
    get,
    path = "/api/v3/wrapped/user/{user_id}",
    params(("user_id" = String, Path, description = "User id")),
    responses(
        (status = 200, description = "Per-user summary, possibly empty", body = UserWrappedResponse),
        (status = 401, description = "Invalid or missing wrapped API key"),
    )
)]
pub async fn get_wrapped_user(
    State(state): State<WrappedState>,
    _: WrappedKey,
    Path(user_id): Path<String>,
) -> Json<UserWrappedResponse> {
    let payload = state.data.user_wrapped(&user_id).await;
    match payload {
        Some(payload) => Json(payload),
        None => Json(empty_user_wrapped(&user_id, state.data.current_year())),
    }
}

/// Server-wide year-in-review summary.
#[utoipa::path(
    get,
    path = "/api/v3/wrapped/server",
    responses(
        (status = 200, description = "Server summary", body = ServerWrappedResponse),
        (status = 401, description = "Invalid or missing wrapped API key"),
    )
)]
pub async fn get_wrapped_server(
    State(state): State<WrappedState>,
    _: WrappedKey,
) -> Json<ServerWrappedResponse> {
    Json(state.data.server_wrapped().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn key_check_passes_only_on_exact_match() {
        let exact = headers(&[("x-wrapped-api-key", "secret")]);
        assert!(check_key(&exact, "secret"));
        // Header names are case-insensitive per HTTP (v2 behaves the same).
        let upper = headers(&[("X-WRAPPED-API-KEY", "secret")]);
        assert!(check_key(&upper, "secret"));
    }

    #[test]
    fn key_check_rejects_everything_else() {
        assert!(!check_key(&HeaderMap::new(), "secret"));
        let wrong = headers(&[("x-wrapped-api-key", "other")]);
        assert!(!check_key(&wrong, "secret"));
        // No trimming: padded values fail (v2 `!=` on the raw string).
        for padded in [" secret", "secret ", " secret "] {
            let headers = headers(&[("x-wrapped-api-key", padded)]);
            assert!(!check_key(&headers, "secret"), "value {padded:?}");
        }
        // Unconfigured secret denies everything, even a presented key.
        let presented = headers(&[("x-wrapped-api-key", "secret")]);
        assert!(!check_key(&presented, ""));
        assert!(!check_key(&HeaderMap::new(), ""));
    }

    #[test]
    fn empty_payload_falls_back_to_user_id() {
        let empty = empty_user_wrapped("u-1", 2026);
        assert!(!empty.has_data);
        assert_eq!(empty.display_name, "u-1");
        assert!(empty.top_artists.is_empty());
        assert_eq!(empty.total_listens_estimated, 0);
    }
}
