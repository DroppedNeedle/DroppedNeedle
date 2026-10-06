//! Wrapped reads: the keyed external-consumer trio.
//!
//! Carried over from v2's wrapped routes: user listing, per-user
//! year-in-review, and the server-wide summary. The `X-Wrapped-API-Key`
//! contract is preserved exactly (see [`check_key`]): exact header name,
//! exact value match with no trimming, 401 on missing/wrong/unconfigured,
//! and the v2 rejection message verbatim. Year-in-review data comes from the
//! [`WrappedData`] port; production runs
//! [`ListenBrainzWrapped`](super::listenbrainz_wrapped::ListenBrainzWrapped).
//!
//! Unknown users answer 200 with an empty `has_data: false` payload (v2
//! `get_user_wrapped` rule kept, display name falls back to the user id) -
//! never 404. These routes take no session: the shared secret is the only
//! credential, so they mount outside the session middleware.
//!
//! The error envelope mirrors `crate::error` exactly.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, StatusCode, request::Parts},
    response::Response,
    routing::get,
};
use futures_util::future::BoxFuture;
use sha2::{Digest, Sha256};

use crate::auth::session::tokens::constant_time_eq;
use crate::runtime_config::{ConfigStore, secret_sections::WrappedSettings};
use serde::Serialize;
use utoipa::ToSchema;

/// Shared-secret header, v2 name kept byte for byte.
pub const WRAPPED_API_KEY_HEADER: &str = "x-wrapped-api-key";
/// Rejection message, v2 wording kept verbatim.
pub const WRAPPED_UNAUTHORIZED_MESSAGE: &str = "Invalid or missing wrapped API key";

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

/// Wrapped-data port.
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

/// Wrapped without a data source: no users and empty summaries.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWrappedData;

impl WrappedData for NoWrappedData {
    fn current_year(&self) -> i32 {
        time::OffsetDateTime::now_utc().year()
    }

    fn list_users(&self) -> BoxFuture<'_, Vec<WrappedUserSummary>> {
        Box::pin(async { Vec::new() })
    }

    fn user_wrapped(&self, _user_id: &str) -> BoxFuture<'_, Option<UserWrappedResponse>> {
        Box::pin(async { None })
    }

    fn server_wrapped(&self) -> BoxFuture<'_, ServerWrappedResponse> {
        let year = self.current_year();
        Box::pin(async move {
            ServerWrappedResponse {
                year,
                total_users_tracked: 0,
                total_listens_estimated: 0,
                leaderboard: Vec::new(),
                top_artist_sitewide: None,
                top_album_sitewide: None,
            }
        })
    }
}

/// Scripted wrapped data for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct FakeWrappedData {
    year: i32,
    users: Vec<WrappedUserSummary>,
    per_user: std::collections::HashMap<String, UserWrappedResponse>,
    server: ServerWrappedResponse,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeWrappedData {
    /// Build a fake from its parts.
    pub fn new(
        year: i32,
        users: Vec<WrappedUserSummary>,
        per_user: std::collections::HashMap<String, UserWrappedResponse>,
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
            per_user: std::collections::HashMap::new(),
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

#[cfg(any(test, feature = "test-support"))]
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

/// Where the expected shared secret comes from. Read on every request, so a
/// rotated key takes effect without a restart.
pub trait WrappedKeySource: Send + Sync {
    /// The current secret; empty means unconfigured (deny all).
    fn current(&self) -> String;
}

/// A fixed secret (tests, and callers that hold no config store).
impl WrappedKeySource for String {
    fn current(&self) -> String {
        self.clone()
    }
}

/// The live `wrapped_settings` secret from the config store. A store that
/// cannot be read denies every request (logged) rather than guessing.
pub struct ConfigWrappedKey {
    store: Arc<ConfigStore>,
}

impl ConfigWrappedKey {
    /// Read the secret from `store` on every request.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

impl WrappedKeySource for ConfigWrappedKey {
    fn current(&self) -> String {
        match self.store.get_raw::<WrappedSettings>() {
            Ok(settings) => settings.api_key.expose().to_owned(),
            Err(error) => {
                tracing::error!(%error, "cannot read the wrapped API key; denying");
                String::new()
            }
        }
    }
}

/// Handler state: the expected shared secret plus the data port. An empty
/// secret fails every request closed (v2 `not expected` rule kept).
#[derive(Clone)]
pub struct WrappedState {
    /// Expected shared secret, read per request.
    pub api_key: Arc<dyn WrappedKeySource>,
    /// Wrapped data (the fake until an aggregation is wired).
    pub data: Arc<dyn WrappedData>,
}

impl WrappedState {
    /// Wire the state from a key source and any port implementation.
    pub fn new(api_key: impl WrappedKeySource + 'static, data: Arc<dyn WrappedData>) -> Self {
        Self {
            api_key: Arc::new(api_key),
            data,
        }
    }
}

/// Wrapped routes, relative paths for nesting under `/api/v3`.
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

/// Key check, v2 `verify_wrapped_api_key` semantics: the header name is
/// matched case-insensitively (HTTP rule, same as v2 via Starlette), the
/// value is compared exactly with no trimming, an empty expected secret
/// denies everything, and a missing or non-matching header denies. The
/// comparison runs in constant time over the digests, so response timing
/// says nothing about how much of a guess was right.
pub fn check_key(headers: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let Some(presented) = headers.get(WRAPPED_API_KEY_HEADER) else {
        return false;
    };
    let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    constant_time_eq(&digest(presented.as_bytes()), &digest(expected.as_bytes()))
}

/// Key-gate rejection. Status 401 with the v2 message verbatim, and
/// no `WWW-Authenticate` header on purpose: v2 sends none, and a Bearer
/// challenge would misdescribe a shared-secret header scheme.
fn wrapped_rejection() -> Response {
    crate::error::envelope_response(
        StatusCode::UNAUTHORIZED,
        crate::error::UNAUTHORIZED,
        WRAPPED_UNAUTHORIZED_MESSAGE,
        None,
    )
}

/// Extractor enforcing the wrapped key gate before the handler runs.
pub struct WrappedKey;

impl FromRequestParts<WrappedState> for WrappedKey {
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        state: &WrappedState,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let allowed = check_key(&parts.headers, &state.api_key.current());
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
    fn rotated_key_applies_without_a_restart() {
        use crate::runtime_config::{Crypto, Secret};
        let dir = std::env::temp_dir().join(format!("dn-wrapped-key-{}", std::process::id()));
        let crypto = || Crypto::from_key_bytes(&[7u8; 32]).unwrap();
        let store = Arc::new(ConfigStore::open(&dir.join("config.json"), crypto()).unwrap());
        let source = ConfigWrappedKey::new(store.clone());
        for key in ["first-key", "rotated-key"] {
            store
                .save_secret(WrappedSettings {
                    api_key: Secret::new(key),
                })
                .unwrap();
            let presented = headers(&[("x-wrapped-api-key", key)]);
            assert!(check_key(&presented, &source.current()), "{key}");
        }
        let stale = headers(&[("x-wrapped-api-key", "first-key")]);
        assert!(!check_key(&stale, &source.current()), "old key is refused");
        let _ = std::fs::remove_dir_all(dir);
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
