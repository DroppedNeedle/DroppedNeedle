//! Federated auth routes: OIDC, Jellyfin, and the unified Plex journey.
//!
//! Thin HTTP over the federated slice services. Shapes are clean-slate v3;
//! behavior follows the services plus stage0-auth: OIDC keeps the PKCE
//! authorize/callback/exchange steps, Jellyfin keeps credential login, Plex
//! keeps one start plus one poll per purpose (login/link/connect). OIDC,
//! Jellyfin, Plex start, and Plex poll/login are allowlisted public; Plex
//! poll/link and poll/connect require a session (v2 parity: the link and
//! settings flows always ran authenticated) and return no session. Only the
//! login-shaped completions mint sessions.

use std::sync::Arc;

use crate::auth::federated::FederatedError;
use crate::auth::federated::SessionIssuer;
use crate::auth::federated::jellyfin_login::{JellyfinConnectionLink, JellyfinIdp, JellyfinLogin};
use crate::auth::federated::oidc::{
    OidcConfig, OidcExchangeStore, OidcIdp, OidcLogin, OidcStateStore,
};
use crate::auth::federated::plex::{
    PlexConnectionLink, PlexJourney, PlexPinClient, PlexPoll, PlexPurpose, PlexStartDenied,
};
use crate::auth::federated::users::FederatedUserStore;
use crate::auth::session::cookies;
use crate::auth::session::extract;
use crate::auth::session::login::{LoginSuccess, TransportParam};
use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::handlers::ValidJson;
use crate::ids::IdGenerator;
use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use super::error::{AuthRouteError, ValidQuery};
use super::models::{
    FederatedUserView, JellyfinLoginBody, OidcAuthorizeBody, OidcCallbackQuery, OidcExchangeBody,
    PlexConnectPollResult, PlexLinkPollResult, PlexLoginPollBody, PlexLoginPollResult, PlexPinBody,
    PlexProfileView, PlexStartBody, TransportDto,
};

/// Jellyfin rejection, v2 message verbatim.
pub const JELLYFIN_INVALID: &str = "Invalid credentials";
/// OIDC callback rejection, v2 message verbatim.
pub const OIDC_FAILED: &str = "OIDC authentication failed";
/// OIDC exchange rejection, v2 message verbatim.
pub const OIDC_BAD_CODE: &str = "Invalid or expired code";
/// Plex poll rejection, v2 message verbatim.
pub const PLEX_DENIED: &str = "Access denied";
/// Plex link/connect start without a configured server, v2 message verbatim.
pub const PLEX_NOT_CONFIGURED: &str = "Plex is not configured by the administrator";

/// Session guard for the Plex link/connect polls: the middleware stashes
/// [`CurrentSession`] on every authenticated request, and these polls hand
/// out account Bearer [REDACTED] a PIN id alone must never unlock. This is the same
/// contract as the sibling [`CurrentUser`] extractor minus the role lookup
/// (the polls need no role, and this router's state is not `UsersDeps`, so
/// `CurrentUser` cannot apply here); the allowlist is the outer gate and
/// this is the inner one.
///
/// [`CurrentUser`]: crate::auth::users::roles::CurrentUser
pub struct AuthenticatedSession(pub CurrentSession);

impl<S: Send + Sync> FromRequestParts<S> for AuthenticatedSession {
    type Rejection = AuthRouteError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentSession>()
            .cloned()
            .map(AuthenticatedSession)
            .ok_or_else(|| AuthRouteError::unauthorized("Authentication required"))
    }
}

/// Live OIDC settings. A port (not a value in state) so admin config edits
/// take effect without a restart; the settings slice owns the production
/// implementation.
pub trait OidcConfigSource: Clone + Send + Sync + 'static {
    /// Current OIDC connection settings.
    fn current(&self) -> impl Future<Output = OidcConfig> + Send;
}

/// Fixed OIDC settings for tests and single-config deployments.
#[derive(Debug, Clone)]
pub struct StaticOidcConfig(pub OidcConfig);

impl OidcConfigSource for StaticOidcConfig {
    async fn current(&self) -> OidcConfig {
        self.0.clone()
    }
}

/// State for the OIDC routes.
#[derive(Clone)]
pub struct OidcRouteState<S, I, T, E, N, F> {
    /// OIDC login service.
    pub login: OidcLogin<S, I, T, E, N>,
    /// Live settings source.
    pub config: F,
    /// Fresh ids for 5xx log correlation.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
}

impl<S, I, T, E, N, F> OidcRouteState<S, I, T, E, N, F>
where
    S: FederatedUserStore,
    I: OidcIdp,
    T: OidcStateStore,
    E: OidcExchangeStore,
    N: SessionIssuer,
    F: OidcConfigSource,
{
    /// Wire the state from the login service plus its HTTP ports.
    pub fn new(
        login: OidcLogin<S, I, T, E, N>,
        config: F,
        ids: Arc<dyn IdGenerator>,
        base_path: &str,
    ) -> Self {
        Self {
            login,
            config,
            ids,
            base_path: base_path.to_owned(),
        }
    }
}

/// OIDC routes. Paths are relative; the orchestrator nests this router under
/// `/api/v3` (the whole `/auth/oidc` prefix is allowlisted public).
pub fn oidc_router<S, I, T, E, N, F>(state: OidcRouteState<S, I, T, E, N, F>) -> Router
where
    S: FederatedUserStore,
    I: OidcIdp,
    T: OidcStateStore,
    E: OidcExchangeStore,
    N: SessionIssuer,
    F: OidcConfigSource,
{
    Router::new()
        .route("/auth/oidc/authorize", post(oidc_authorize_handler))
        .route("/auth/oidc/callback", get(oidc_callback_handler))
        .route("/auth/oidc/exchange", post(oidc_exchange_handler))
        .with_state(state)
}

/// Start an OIDC login: returns the browser URL (PKCE + state baked in).
#[utoipa::path(
    post,
    path = "/api/v3/auth/oidc/authorize",
    responses(
        (status = 200, description = "Browser URL for this login", body = OidcAuthorizeBody),
        (status = 503, description = "OIDC not configured or unreachable")
    )
)]
pub async fn oidc_authorize_handler<S, I, T, E, N, F>(
    State(state): State<OidcRouteState<S, I, T, E, N, F>>,
) -> Result<Json<OidcAuthorizeBody>, AuthRouteError>
where
    S: FederatedUserStore,
    I: OidcIdp,
    T: OidcStateStore,
    E: OidcExchangeStore,
    N: SessionIssuer,
    F: OidcConfigSource,
{
    let config = state.config.current().await;
    match state.login.build_authorize_url(&config).await {
        Ok(url) => Ok(Json(OidcAuthorizeBody { authorize_url: url })),
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// OIDC callback: consumes the IdP code/state and redirects (relative
/// Location, same-site hand-off, v2 parity) to the SPA callback carrying the
/// one-time exchange code. Always `no-store`.
#[utoipa::path(
    get,
    path = "/api/v3/auth/oidc/callback",
    params(
        ("code" = String, Query, description = "Authorization code from the IdP"),
        ("state" = String, Query, description = "State echoed from the authorize step")
    ),
    responses(
        (status = 302, description = "Redirect to the SPA callback with the exchange code"),
        (status = 401, description = "OIDC authentication failed"),
        (status = 503, description = "OIDC provider unavailable")
    )
)]
pub async fn oidc_callback_handler<S, I, T, E, N, F>(
    State(state): State<OidcRouteState<S, I, T, E, N, F>>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<OidcCallbackQuery>,
) -> Result<Response, AuthRouteError>
where
    S: FederatedUserStore,
    I: OidcIdp,
    T: OidcStateStore,
    E: OidcExchangeStore,
    N: SessionIssuer,
    F: OidcConfigSource,
{
    let config = state.config.current().await;
    let user_agent = user_agent_of(&headers);
    match state
        .login
        .handle_callback(&config, &query.code, &query.state, user_agent.as_deref())
        .await
    {
        Ok(exchange_code) => {
            let location = format!("{}/auth/callback?code={exchange_code}", state.base_path);
            let value = HeaderValue::from_str(&location).map_err(|_| {
                AuthRouteError::internal(
                    &"callback redirect is not a header value",
                    state.ids.as_ref(),
                )
            })?;
            let mut response = StatusCode::FOUND.into_response();
            let response_headers = response.headers_mut();
            response_headers.insert(axum::http::header::LOCATION, value);
            insert_no_store(response_headers);
            Ok(response)
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "oidc callback rejected");
            Err(AuthRouteError::auth_failed(OIDC_FAILED))
        }
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// OIDC exchange: swaps the one-time callback code for a session. Cookie
/// mode (default) sets the session cookie and the body carries no token;
/// Bearer [REDACTED] the raw token once.
#[utoipa::path(
    post,
    path = "/api/v3/auth/oidc/exchange",
    request_body = OidcExchangeBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer [REDACTED]"),
        (status = 401, description = "Invalid or expired code")
    )
)]
pub async fn oidc_exchange_handler<S, I, T, E, N, F>(
    State(state): State<OidcRouteState<S, I, T, E, N, F>>,
    uri: Uri,
    headers: HeaderMap,
    ValidJson(body): ValidJson<OidcExchangeBody>,
) -> Result<Response, AuthRouteError>
where
    S: FederatedUserStore,
    I: OidcIdp,
    T: OidcStateStore,
    E: OidcExchangeStore,
    N: SessionIssuer,
    F: OidcConfigSource,
{
    match state.login.exchange_code(&body.code).await {
        Ok((user, raw_token)) => {
            let body_value = serde_json::to_value(FederatedUserView::from(&user))
                .map_err(|cause| AuthRouteError::internal(&cause, state.ids.as_ref()))?;
            Ok(federated_login_response(
                &Handoff {
                    base_path: &state.base_path,
                    scheme: uri.scheme_str().unwrap_or("http"),
                    headers: &headers,
                    transport: body.transport,
                },
                &user.id,
                &user.display_name,
                body_value,
                raw_token,
            ))
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "oidc exchange rejected");
            Err(AuthRouteError::auth_failed(OIDC_BAD_CODE))
        }
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// State for the Jellyfin route.
#[derive(Clone)]
pub struct JellyfinRouteState<S, I, L, N> {
    /// Jellyfin login service.
    pub login: JellyfinLogin<S, I, L, N>,
    /// Fresh ids for 5xx log correlation.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
}

impl<S, I, L, N> JellyfinRouteState<S, I, L, N>
where
    S: FederatedUserStore,
    I: JellyfinIdp,
    L: JellyfinConnectionLink,
    N: SessionIssuer,
{
    /// Wire the state from its ports.
    pub fn new(
        users: S,
        idp: I,
        links: L,
        sessions: N,
        ids: Arc<dyn IdGenerator>,
        base_path: &str,
    ) -> Self {
        Self {
            login: JellyfinLogin::new(users, idp, links, sessions),
            ids,
            base_path: base_path.to_owned(),
        }
    }
}

/// Jellyfin route. Relative path; the orchestrator nests it under `/api/v3`
/// (the exact path is allowlisted public).
pub fn jellyfin_router<S, I, L, N>(state: JellyfinRouteState<S, I, L, N>) -> Router
where
    S: FederatedUserStore,
    I: JellyfinIdp,
    L: JellyfinConnectionLink,
    N: SessionIssuer,
{
    Router::new()
        .route("/auth/jellyfin/login", post(jellyfin_login_handler))
        .with_state(state)
}

/// Jellyfin login: checks credentials against the configured server, imports
/// the user, and mints a session. Transport rule matches local login.
#[utoipa::path(
    post,
    path = "/api/v3/auth/jellyfin/login",
    request_body = JellyfinLoginBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer [REDACTED]"),
        (status = 401, description = "Invalid credentials"),
        (status = 503, description = "Jellyfin unavailable")
    )
)]
pub async fn jellyfin_login_handler<S, I, L, N>(
    State(state): State<JellyfinRouteState<S, I, L, N>>,
    uri: Uri,
    headers: HeaderMap,
    ValidJson(body): ValidJson<JellyfinLoginBody>,
) -> Result<Response, AuthRouteError>
where
    S: FederatedUserStore,
    I: JellyfinIdp,
    L: JellyfinConnectionLink,
    N: SessionIssuer,
{
    let user_agent = user_agent_of(&headers);
    match state
        .login
        .login(&body.username, &body.password, user_agent.as_deref())
        .await
    {
        Ok((user, raw_token)) => {
            let body_value = serde_json::to_value(FederatedUserView::from(&user))
                .map_err(|cause| AuthRouteError::internal(&cause, state.ids.as_ref()))?;
            Ok(federated_login_response(
                &Handoff {
                    base_path: &state.base_path,
                    scheme: uri.scheme_str().unwrap_or("http"),
                    headers: &headers,
                    transport: body.transport,
                },
                &user.id,
                &user.display_name,
                body_value,
                raw_token,
            ))
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "jellyfin login rejected");
            Err(AuthRouteError::unauthorized(JELLYFIN_INVALID))
        }
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// State for the Plex journey routes.
#[derive(Clone)]
pub struct PlexRouteState<S, C, L, N> {
    /// Unified Plex journey.
    pub journey: PlexJourney<S, C, L, N>,
    /// Fresh ids for 5xx log correlation.
    pub ids: Arc<dyn IdGenerator>,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
}

impl<S, C, L, N> PlexRouteState<S, C, L, N>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    /// Wire the state from its ports.
    pub fn new(
        users: S,
        client: C,
        links: L,
        sessions: N,
        ids: Arc<dyn IdGenerator>,
        base_path: &str,
    ) -> Self {
        Self {
            journey: PlexJourney::new(users, client, links, sessions),
            ids,
            base_path: base_path.to_owned(),
        }
    }
}

/// Plex journey routes. Relative paths; the orchestrator nests them under
/// `/api/v3`. Only start and poll/login are allowlisted public; poll/link
/// and poll/connect additionally carry the session extractor, so they 401
/// even if mounted without the middleware.
pub fn plex_router<S, C, L, N>(state: PlexRouteState<S, C, L, N>) -> Router
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    Router::new()
        .route("/auth/plex/start", post(plex_start_handler))
        .route("/auth/plex/poll/login", post(plex_poll_login_handler))
        .route("/auth/plex/poll/link", post(plex_poll_link_handler))
        .route("/auth/plex/poll/connect", post(plex_poll_connect_handler))
        .with_state(state)
}

/// Plex start query: which flow this PIN serves. Absent means login.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlexStartQuery {
    /// `login` (default), `link`, or `connect`.
    pub purpose: Option<String>,
}

/// Start any Plex flow: mints a PIN and its browser URL. Link and connect
/// starts 400 without a configured Plex server; login starts never gate.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/start",
    params(
        ("purpose" = Option<String>, Query, description = "Which flow this PIN serves: login (default), link, or connect")
    ),
    responses(
        (status = 200, description = "Fresh PIN and browser URL", body = PlexStartBody),
        (status = 400, description = "Link/connect start without a configured Plex server"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_start_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    ValidQuery(query): ValidQuery<PlexStartQuery>,
) -> Result<Json<PlexStartBody>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    let purpose = match query.purpose.as_deref() {
        None | Some("login") => PlexPurpose::Login,
        Some("link") => PlexPurpose::Link,
        Some("connect") => PlexPurpose::Connect,
        Some(other) => {
            return Err(AuthRouteError::InvalidInput {
                message: format!("Unknown Plex start purpose: {other}"),
            });
        }
    };
    match state.journey.start_for_purpose(purpose).await {
        Ok((pin_id, authorize_url)) => Ok(Json(PlexStartBody {
            pin_id,
            authorize_url,
        })),
        Err(PlexStartDenied::NotConfigured) => Err(AuthRouteError::InvalidInput {
            message: PLEX_NOT_CONFIGURED.to_owned(),
        }),
        // No caller credential exists at this step, so any failure is the
        // IdP being down (v2 let it escape to a 500; v3 says 503).
        Err(PlexStartDenied::StartFailed(error)) => {
            Err(federated_unavailable(&error, state.ids.as_ref()))
        }
    }
}

/// Plex login completion: polls the PIN, and once authorized imports the
/// user and mints a session. Pending polls carry only the flag.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/poll/login",
    request_body = PlexLoginPollBody,
    responses(
        (status = 200, description = "Pending flag, or the user once authorized", body = PlexLoginPollResult),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_login_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    uri: Uri,
    headers: HeaderMap,
    ValidJson(body): ValidJson<PlexLoginPollBody>,
) -> Result<Response, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    let user_agent = user_agent_of(&headers);
    match state
        .journey
        .poll_login(body.pin_id, user_agent.as_deref())
        .await
    {
        Ok(PlexPoll::Pending) => Ok(Json(PlexLoginPollResult {
            completed: false,
            user: None,
            token: None,
        })
        .into_response()),
        Ok(PlexPoll::Complete((user, raw_token))) => {
            let body_value = serde_json::to_value(PlexLoginPollResult {
                completed: true,
                user: Some(FederatedUserView::from(&user)),
                token: None,
            })
            .map_err(|cause| AuthRouteError::internal(&cause, state.ids.as_ref()))?;
            let handoff = Handoff {
                base_path: &state.base_path,
                scheme: uri.scheme_str().unwrap_or("http"),
                headers: &headers,
                transport: body.transport,
            };
            let rendered = federated_login_response(
                &handoff,
                &user.id,
                &user.display_name,
                body_value,
                raw_token,
            );
            Ok(rendered)
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "plex login poll rejected");
            Err(AuthRouteError::Forbidden {
                message: PLEX_DENIED.to_owned(),
            })
        }
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// Plex link completion: polls the PIN and returns the verified profile.
/// Requires a session (the profile carries account tokens); no login side
/// effects, the caller attaches the profile to its own account.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/poll/link",
    request_body = PlexPinBody,
    responses(
        (status = 200, description = "Pending flag, or the verified profile", body = PlexLinkPollResult),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_link_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    AuthenticatedSession(_session): AuthenticatedSession,
    ValidJson(body): ValidJson<PlexPinBody>,
) -> Result<Json<PlexLinkPollResult>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    match state.journey.poll_link(body.pin_id).await {
        Ok(PlexPoll::Pending) => Ok(Json(PlexLinkPollResult {
            completed: false,
            profile: None,
        })),
        Ok(PlexPoll::Complete(profile)) => Ok(Json(PlexLinkPollResult {
            completed: true,
            profile: Some(PlexProfileView::from(&profile)),
        })),
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "plex link poll rejected");
            Err(AuthRouteError::Forbidden {
                message: PLEX_DENIED.to_owned(),
            })
        }
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// Plex settings completion: polls the PIN and returns the raw auth token
/// untouched. Requires a session (the token is an account credential).
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/poll/connect",
    request_body = PlexPinBody,
    responses(
        (status = 200, description = "Pending flag, or the raw auth token", body = PlexConnectPollResult),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_connect_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    AuthenticatedSession(_session): AuthenticatedSession,
    ValidJson(body): ValidJson<PlexPinBody>,
) -> Result<Json<PlexConnectPollResult>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    match state.journey.poll_connect(body.pin_id).await {
        Ok(PlexPoll::Pending) => Ok(Json(PlexConnectPollResult {
            completed: false,
            auth_token: None,
        })),
        Ok(PlexPoll::Complete(auth_token)) => Ok(Json(PlexConnectPollResult {
            completed: true,
            auth_token: Some(auth_token),
        })),
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "plex connect poll rejected");
            Err(AuthRouteError::Forbidden {
                message: PLEX_DENIED.to_owned(),
            })
        }
        Err(error) => Err(federated_unavailable(&error, state.ids.as_ref())),
    }
}

/// Non-caller-fault federated failures: misconfigured or unreachable IdPs
/// are 503, store/RNG faults are 500. Bodies stay fixed; the cause goes to
/// the log with the error id.
fn federated_unavailable(error: &FederatedError, ids: &dyn IdGenerator) -> AuthRouteError {
    match error {
        FederatedError::NotConfigured(_) | FederatedError::ProviderUnavailable(_) => {
            AuthRouteError::unavailable(error, ids)
        }
        FederatedError::Authentication(_) => AuthRouteError::unavailable(error, ids),
        FederatedError::StoreUnavailable(_)
        | FederatedError::UsernameTaken
        | FederatedError::RngUnavailable => AuthRouteError::internal(error, ids),
    }
}

/// How one login-shaped response hands the session over.
struct Handoff<'a> {
    base_path: &'a str,
    scheme: &'a str,
    headers: &'a HeaderMap,
    transport: TransportDto,
}

/// One login-shaped federated response: the user JSON plus the session under
/// the D1 transport rule, `no-store` always. The session row already exists
/// (the `SessionIssuer` wrote it); this only hands the token over. In Bearer [REDACTED]
/// the sibling renderer adds the `token` field to the object body.
fn federated_login_response(
    handoff: &Handoff<'_>,
    user_id: &str,
    display_name: &str,
    user_json: serde_json::Value,
    raw_token: String,
) -> Response {
    let param = handoff.transport.as_param();
    let session_transport = match param {
        TransportParam::Cookie => extract::Transport::Cookie,
        TransportParam::Bearer => extract::Transport::Bearer,
    };
    let set_cookie = match param {
        TransportParam::Cookie => Some(cookies::set_cookie_value(
            &raw_token,
            handoff.base_path,
            cookies::is_secure(handoff.scheme, handoff.headers),
        )),
        TransportParam::Bearer => None,
    };
    let success = LoginSuccess {
        user_id: user_id.to_owned(),
        display_name: display_name.to_owned(),
        transport: session_transport,
        raw_token,
        set_cookie,
    };
    crate::auth::session::login::login_response(user_json, &success)
}

/// Calling `User-Agent`, when the client sent one.
fn user_agent_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Stamp `no-store` when the value parses (it always does; the guard keeps
/// the helper total like the sibling renderer).
fn insert_no_store(headers: &mut HeaderMap) {
    if let Ok(value) = crate::auth::session::login::NO_STORE.parse() {
        headers.insert(axum::http::header::CACHE_CONTROL, value);
    }
}
