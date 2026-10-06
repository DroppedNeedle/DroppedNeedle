//! Federated auth routes: the provider list, OIDC, Jellyfin, and the
//! unified Plex journey.
//!
//! Thin HTTP over the federated services. Shapes are clean-slate v3;
//! behavior follows the services: OIDC keeps the PKCE
//! authorize/callback/exchange steps, Jellyfin keeps credential login, Plex
//! keeps one start plus one poll per purpose (login/link/connect). The
//! provider list, OIDC, Jellyfin, Plex start, and Plex poll/login are
//! allowlisted public; Plex poll/link and poll/connect require a session
//! (v2 parity: the link and settings flows always ran authenticated) and
//! return no session. Only the login-shaped completions mint sessions.

use std::sync::Arc;

use crate::auth::federated::FederatedError;
use crate::auth::federated::SessionIssuer;
use crate::auth::federated::jellyfin_login::{JellyfinConnectionLink, JellyfinIdp, JellyfinLogin};
use crate::auth::federated::oidc::{
    OidcConfig, OidcExchangeStore, OidcIdp, OidcLogin, OidcStateStore, STATE_TTL_SECS,
};
use crate::auth::federated::plex::{
    PinClaim, PlexConnectionLink, PlexJourney, PlexPinClient, PlexPoll, PlexPurpose,
    PlexStartDenied,
};
use crate::auth::federated::users::FederatedUserStore;
use crate::auth::routes::native::PeerAddr;
use crate::auth::session::cookies;
use crate::auth::session::middleware::{CurrentSession, TrustedProxies};
use crate::auth::session::rate_limit::{LOGIN_CLASS, RateLimiter, rate_limited_response};
use crate::auth::session::tokens::{constant_time_eq, hash_token};
use crate::auth::users::handlers::ValidJson;
use crate::ids::IdGenerator;
use axum::{
    Json, Router,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use super::error::{AuthRouteError, ValidQuery};
use super::models::{
    AuthProvidersBody, FederatedUserView, JellyfinLoginBody, OidcAuthorizeBody, OidcCallbackQuery,
    OidcExchangeBody, PlexConnectPollResult, PlexLinkPollResult, PlexLoginPollBody,
    PlexLoginPollResult, PlexPinBody, PlexStartBody, TransportDto,
};
use crate::auth::federated::settings::{enabled_providers, oidc_config};
use crate::runtime_config::ConfigStore;

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
/// out account Bearer tokens, which a PIN id alone must never unlock. This is the same
/// contract as the users [`CurrentUser`] extractor minus the role lookup
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
/// take effect without a restart.
pub trait OidcConfigSource: Clone + Send + Sync + 'static {
    /// Current OIDC connection settings.
    fn current(&self) -> impl Future<Output = OidcConfig> + Send;
}

/// OIDC settings read from the config store on every request.
#[derive(Clone)]
pub struct StoreOidcConfig(pub Arc<ConfigStore>);

impl OidcConfigSource for StoreOidcConfig {
    async fn current(&self) -> OidcConfig {
        oidc_config(&self.0)
    }
}

/// The sign-in methods the login page offers. Public: the page asks before
/// anyone is signed in.
pub fn providers_router(store: Arc<ConfigStore>) -> Router {
    Router::new()
        .route("/auth/providers", get(providers_handler))
        .with_state(store)
}

/// Which sign-in methods are switched on (v2 `GET /auth/providers`).
#[utoipa::path(
    get,
    path = "/api/v3/auth/providers",
    responses((status = 200, description = "Enabled sign-in methods", body = AuthProvidersBody))
)]
pub async fn providers_handler(State(store): State<Arc<ConfigStore>>) -> Json<AuthProvidersBody> {
    let providers = enabled_providers(&store);
    Json(AuthProvidersBody {
        local: providers.local,
        plex: providers.plex,
        jellyfin: providers.jellyfin,
        oidc: providers.oidc,
    })
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
    /// Proxies trusted to set `X-Forwarded-Proto` for the cookie `Secure`
    /// flag; loopback by default.
    pub trusted_proxies: TrustedProxies,
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
            trusted_proxies: TrustedProxies::default(),
        }
    }

    /// Trust the given proxies for the forwarded proto.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
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

/// Cookie binding a pending OIDC login to the browser that started it. It
/// holds a hash of the state, so the callback only completes in the
/// browser that asked for the login (login CSRF).
pub const OIDC_STATE_COOKIE: &str = "droppedneedle_oidc_state";

/// One `Set-Cookie` value for the state cookie. The path covers both the
/// v3 callback and the v2 one kept for migrated providers.
fn oidc_state_cookie(base_path: &str, value: &str, max_age: u64, secure: bool) -> String {
    let mut cookie = format!(
        "{OIDC_STATE_COOKIE}={value}; Path={}/api; Max-Age={max_age}; HttpOnly; SameSite=Lax",
        base_path.trim_end_matches('/'),
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// One cookie's value from the request.
fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
}

/// Start an OIDC login: returns the browser URL (PKCE, state and nonce
/// baked in) and sets a short-lived cookie tying the state to this browser.
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
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
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
    match state.login.build_authorize_url(&config).await {
        Ok((url, login_state)) => {
            let secure = cookies::is_secure_trusted(
                uri.scheme_str().unwrap_or("http"),
                &headers,
                state.trusted_proxies.is_trusted(peer.0),
            );
            let mut response = Json(OidcAuthorizeBody { authorize_url: url }).into_response();
            cookies::push_set_cookie(
                response.headers_mut(),
                &oidc_state_cookie(
                    &state.base_path,
                    &hash_token(&login_state),
                    STATE_TTL_SECS,
                    secure,
                ),
            );
            insert_no_store(response.headers_mut());
            Ok(response)
        }
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
    // The callback must land in the browser that started this login.
    let bound = read_cookie(&headers, OIDC_STATE_COOKIE)
        .is_some_and(|cookie| constant_time_eq(&cookie, &hash_token(&query.state)));
    if !bound {
        tracing::debug!("oidc callback without the matching state cookie");
        return Err(AuthRouteError::auth_failed(OIDC_FAILED));
    }
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
            cookies::push_set_cookie(
                response_headers,
                &oidc_state_cookie(&state.base_path, "", 0, false),
            );
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
/// Bearer mode returns the raw token once.
#[utoipa::path(
    post,
    path = "/api/v3/auth/oidc/exchange",
    request_body = OidcExchangeBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer mode"),
        (status = 401, description = "Invalid or expired code")
    )
)]
pub async fn oidc_exchange_handler<S, I, T, E, N, F>(
    State(state): State<OidcRouteState<S, I, T, E, N, F>>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
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
                    secure: cookies::is_secure_trusted(
                        uri.scheme_str().unwrap_or("http"),
                        &headers,
                        state.trusted_proxies.is_trusted(peer.0),
                    ),
                    transport: body.transport,
                },
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
    /// Proxies trusted to set `X-Forwarded-Proto` for the cookie `Secure`
    /// flag; loopback by default.
    pub trusted_proxies: TrustedProxies,
    /// Request limiter; each login also takes a per-username token, like
    /// local login, so one Jellyfin account cannot be guessed at full rate.
    pub limits: Arc<RateLimiter>,
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
            trusted_proxies: TrustedProxies::default(),
            limits: Arc::new(RateLimiter::new()),
        }
    }

    /// Trust the given proxies for the forwarded proto.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    /// Share the app's request limiter (per-username login budget).
    pub fn with_limits(mut self, limits: Arc<RateLimiter>) -> Self {
        self.limits = limits;
        self
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
        (status = 200, description = "Authenticated user; token only in Bearer mode"),
        (status = 401, description = "Invalid credentials"),
        (status = 429, description = "Too many attempts for this client or username"),
        (status = 503, description = "Jellyfin unavailable")
    )
)]
pub async fn jellyfin_login_handler<S, I, L, N>(
    State(state): State<JellyfinRouteState<S, I, L, N>>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<JellyfinLoginBody>,
) -> Result<Response, AuthRouteError>
where
    S: FederatedUserStore,
    I: JellyfinIdp,
    L: JellyfinConnectionLink,
    N: SessionIssuer,
{
    let username_key = format!("jellyfin-username:{}", body.username.trim().to_lowercase());
    let budget = state.limits.check(LOGIN_CLASS, &username_key);
    if !budget.allowed {
        return Ok(rate_limited_response(LOGIN_CLASS, budget.retry_after_secs));
    }
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
                    secure: cookies::is_secure_trusted(
                        uri.scheme_str().unwrap_or("http"),
                        &headers,
                        state.trusted_proxies.is_trusted(peer.0),
                    ),
                    transport: body.transport,
                },
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
    /// Proxies trusted to set `X-Forwarded-Proto` for the cookie `Secure`
    /// flag; loopback by default.
    pub trusted_proxies: TrustedProxies,
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
            trusted_proxies: TrustedProxies::default(),
        }
    }

    /// Trust the given proxies for the forwarded proto.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }
}

/// Plex journey routes. Relative paths; the orchestrator nests them under
/// `/api/v3`. Only the login start and poll/login are allowlisted public;
/// the link and connect steps carry the session extractor, so they 401 even
/// if mounted without the middleware.
pub fn plex_router<S, C, L, N>(state: PlexRouteState<S, C, L, N>) -> Router
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    Router::new()
        .route("/auth/plex/start", post(plex_start_handler))
        .route("/auth/plex/start/link", post(plex_start_link_handler))
        .route("/auth/plex/start/connect", post(plex_start_connect_handler))
        .route("/auth/plex/poll/login", post(plex_poll_login_handler))
        .route("/auth/plex/poll/link", post(plex_poll_link_handler))
        .route("/auth/plex/poll/connect", post(plex_poll_connect_handler))
        .with_state(state)
}

/// Run one start and render it. The secret is returned once and never
/// stored in clear.
async fn start_rendered<S, C, L, N>(
    state: &PlexRouteState<S, C, L, N>,
    purpose: PlexPurpose,
    caller: Option<&str>,
) -> Result<Json<PlexStartBody>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    match state.journey.start(purpose, caller).await {
        Ok(started) => Ok(Json(PlexStartBody {
            pin_id: started.pin_id,
            authorize_url: started.authorize_url,
            pin_secret: started.pin_secret,
        })),
        Err(PlexStartDenied::NotConfigured) => Err(AuthRouteError::InvalidInput {
            message: PLEX_NOT_CONFIGURED.to_owned(),
        }),
        Err(PlexStartDenied::Forbidden) => Err(AuthRouteError::Forbidden {
            message: "Admin access required".to_owned(),
        }),
        // No caller credential is at fault here: the IdP is down, the
        // server lookup failed, or login is switched off.
        Err(PlexStartDenied::StartFailed(error)) => {
            Err(federated_unavailable(&error, state.ids.as_ref()))
        }
    }
}

/// Start a Plex sign-in: mints a PIN, its browser URL, and the secret
/// every poll must present. 503 while Plex login is switched off.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/start",
    responses(
        (status = 200, description = "Fresh PIN, browser URL and poll secret", body = PlexStartBody),
        (status = 503, description = "Plex login is off, or Plex is unreachable")
    )
)]
pub async fn plex_start_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
) -> Result<Json<PlexStartBody>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    start_rendered(&state, PlexPurpose::Login, None).await
}

/// Start linking the caller's Plex account. 400 without a configured Plex
/// server.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/start/link",
    responses(
        (status = 200, description = "Fresh PIN, browser URL and poll secret", body = PlexStartBody),
        (status = 400, description = "No Plex server is configured"),
        (status = 401, description = "Missing or invalid session"),
        (status = 503, description = "Plex or the configured server is unreachable")
    )
)]
pub async fn plex_start_link_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    AuthenticatedSession(session): AuthenticatedSession,
) -> Result<Json<PlexStartBody>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    start_rendered(&state, PlexPurpose::Link, Some(&session.user_id)).await
}

/// Start the settings sign-in that hands an admin their Plex token for
/// the server settings. Admin only.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/start/connect",
    responses(
        (status = 200, description = "Fresh PIN, browser URL and poll secret", body = PlexStartBody),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Admin access required"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_start_connect_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    AuthenticatedSession(session): AuthenticatedSession,
) -> Result<Json<PlexStartBody>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    start_rendered(&state, PlexPurpose::Connect, Some(&session.user_id)).await
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
    peer: PeerAddr,
    ValidJson(body): ValidJson<PlexLoginPollBody>,
) -> Result<Response, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    let user_agent = user_agent_of(&headers);
    let claim = PinClaim {
        pin_id: body.pin_id,
        pin_secret: &body.pin_secret,
        user_id: None,
    };
    match state
        .journey
        .poll_login(&claim, user_agent.as_deref())
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
                secure: cookies::is_secure_trusted(
                    uri.scheme_str().unwrap_or("http"),
                    &headers,
                    state.trusted_proxies.is_trusted(peer.0),
                ),
                transport: body.transport,
            };
            let rendered = federated_login_response(&handoff, body_value, raw_token);
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

/// Plex link completion: polls the PIN and, once authorized, stores the
/// verified account as the caller's Plex media link (v2 parity). Requires
/// a session; the answer carries the Plex user name, never its tokens.
#[utoipa::path(
    post,
    path = "/api/v3/auth/plex/poll/link",
    request_body = PlexPinBody,
    responses(
        (status = 200, description = "Pending flag, or the linked Plex user name", body = PlexLinkPollResult),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_link_handler<S, C, L, N>(
    State(state): State<PlexRouteState<S, C, L, N>>,
    AuthenticatedSession(session): AuthenticatedSession,
    ValidJson(body): ValidJson<PlexPinBody>,
) -> Result<Json<PlexLinkPollResult>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    let claim = PinClaim {
        pin_id: body.pin_id,
        pin_secret: &body.pin_secret,
        user_id: Some(&session.user_id),
    };
    match state.journey.poll_link(&claim).await {
        Ok(PlexPoll::Pending) => Ok(Json(PlexLinkPollResult {
            completed: false,
            username: None,
        })),
        Ok(PlexPoll::Complete(profile)) => Ok(Json(PlexLinkPollResult {
            completed: true,
            username: Some(profile.display_name),
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
    AuthenticatedSession(session): AuthenticatedSession,
    ValidJson(body): ValidJson<PlexPinBody>,
) -> Result<Json<PlexConnectPollResult>, AuthRouteError>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    let claim = PinClaim {
        pin_id: body.pin_id,
        pin_secret: &body.pin_secret,
        user_id: Some(&session.user_id),
    };
    match state.journey.poll_connect(&claim).await {
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
    /// Mark the cookie `Secure`: direct TLS, or HTTPS at a trusted proxy.
    secure: bool,
    transport: TransportDto,
}

/// One login-shaped federated response. The session row already exists (the
/// `SessionIssuer` wrote it); this only hands the token over under the login
/// transport rule.
fn federated_login_response(
    handoff: &Handoff<'_>,
    user_json: serde_json::Value,
    raw_token: String,
) -> Response {
    super::login_handoff(
        handoff.base_path,
        handoff.secure,
        user_json,
        handoff.transport.as_param().into(),
        &raw_token,
    )
}

/// Calling `User-Agent`, when the client sent one.
fn user_agent_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Stamp `no-store` when the value parses (it always does; the guard keeps
/// the helper total like the session login renderer).
fn insert_no_store(headers: &mut HeaderMap) {
    if let Ok(value) = crate::auth::session::login::NO_STORE.parse() {
        headers.insert(axum::http::header::CACHE_CONTROL, value);
    }
}
