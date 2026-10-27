//! Federated auth routes: the provider list, OIDC, Jellyfin, and the
//! unified Plex journey.
//!
//! Thin HTTP over the federated services. Shapes are clean-slate v3;
//! behavior follows the services: OIDC keeps the PKCE
//! authorize/callback/exchange steps, Jellyfin keeps credential login, Plex
//! keeps one start plus one poll per purpose (login/link/connect). The
//! provider list, OIDC, Jellyfin, Plex start, and Plex poll/login are
//! in the public group; Plex poll/link and poll/connect require a session
//! (v2 parity: the link and settings flows always ran authenticated) and
//! return no session. Only the login-shaped completions mint sessions.

use std::sync::Arc;

use crate::federated::jellyfin_http::JellyfinHttp;
use crate::federated::jellyfin_login::JellyfinLogin;
use crate::federated::oidc::{OidcLogin, STATE_TTL_SECS};
use crate::federated::plex::{PinClaim, PlexJourney, PlexPoll, PlexPurpose, PlexStartDenied};
use crate::federated::plex_http::PlexTv;
use crate::federated::{FederatedError, MediaLinks};
use crate::routes::native::PeerAddr;
use crate::session::cookies;
use crate::session::rate_limit::{LOGIN_CLASS, RateLimiter, rate_limited};
use crate::session::stated::StatedAddresses;
use crate::session::tokens::{constant_time_eq, hash_token};
use crate::sqlite::{FederatedUsers, Sessions};
use crate::users::roles::{Admin, RoleRequired, User};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

use super::error::AuthRouteError;
use super::models::{
    AuthProvidersBody, FederatedUserView, JellyfinLoginBody, OidcAuthorizeBody, OidcCallbackQuery,
    OidcExchangeBody, PlexConnectPollResult, PlexLinkPollResult, PlexLoginPollBody,
    PlexLoginPollResult, PlexPinBody, PlexStartBody, TransportDto,
};
use crate::federated::settings::SignInSettings;

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

/// Which sign-in methods are switched on (v2 `GET /auth/providers`).
#[utoipa::path(
    get,
    tag = "auth",
    path = "/api/v3/auth/providers",
    responses((status = 200, description = "Enabled sign-in methods", body = AuthProvidersBody))
)]
pub async fn providers_handler(State(settings): State<SignInSettings>) -> Json<AuthProvidersBody> {
    let providers = settings.enabled_providers();
    Json(AuthProvidersBody {
        local: providers.local,
        plex: providers.plex,
        jellyfin: providers.jellyfin,
        oidc: providers.oidc,
    })
}

/// State for the OIDC routes.
#[derive(Clone)]
pub struct OidcRouteState {
    /// OIDC login service.
    pub login: OidcLogin,
    /// The sign-in settings, read live so an admin's edit applies to the
    /// next login without a restart.
    pub config: SignInSettings,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
    /// Proxies trusted to set `X-Forwarded-Proto` for the cookie `Secure`
    /// flag; loopback by default.
    pub trusted_proxies: TrustedProxies,
    /// The saved public addresses: a sign-in from a stated `https` one gets
    /// a `Secure` cookie. None is stated by default.
    pub stated: StatedAddresses,
}

impl OidcRouteState {
    /// Wire the state from the login service and the live settings.
    pub fn new(login: OidcLogin, config: SignInSettings, base_path: &str) -> Self {
        Self {
            login,
            config,
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
            stated: StatedAddresses::default(),
        }
    }

    /// Trust the given proxies for the forwarded proto.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    /// Mark the cookie `Secure` for a sign-in from a stated `https` address.
    pub fn with_stated_addresses(mut self, stated: StatedAddresses) -> Self {
        self.stated = stated;
        self
    }
}

/// Path of the state cookie (`cookies::OIDC_STATE_COOKIE`), which binds a
/// pending OIDC login to the browser that started it. It holds a hash of
/// the state, so the callback only completes in the browser that asked for
/// the login (login CSRF). The path covers both the v3 callback and the v2
/// one kept for migrated providers.
fn oidc_state_cookie_path(base_path: &str) -> String {
    format!("{}/api", base_path.trim_end_matches('/'))
}

/// Start an OIDC login: returns the browser URL (PKCE, state and nonce
/// baked in) and sets a short-lived cookie tying the state to this browser.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/oidc/authorize",
    responses(
        (status = 200, description = "Browser URL for this login", body = OidcAuthorizeBody),
        (status = 503, description = "OIDC not configured or unreachable")
    )
)]
pub async fn oidc_authorize_handler(
    State(state): State<OidcRouteState>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
) -> Result<Response, ApiError> {
    let config = state.config.oidc_config();
    match state.login.build_authorize_url(&config).await {
        Ok((url, login_state)) => {
            let secure = cookies::secure_for_sign_in(
                uri.scheme_str().unwrap_or("http"),
                &headers,
                state.trusted_proxies.is_trusted(peer.0),
                &state.stated,
            );
            let mut response = Json(OidcAuthorizeBody { authorize_url: url }).into_response();
            for value in cookies::OIDC_STATE_COOKIE.set(
                &hash_token(&login_state),
                &oidc_state_cookie_path(&state.base_path),
                STATE_TTL_SECS,
                secure,
            ) {
                cookies::push_set_cookie(response.headers_mut(), &value);
            }
            insert_no_store(response.headers_mut());
            Ok(response)
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// OIDC callback: consumes the IdP code/state and redirects (relative
/// Location, same-site hand-off, v2 parity) to the SPA callback carrying the
/// one-time exchange code. Always `no-store`.
#[utoipa::path(
    get,
    tag = "auth",
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
pub async fn oidc_callback_handler(
    State(state): State<OidcRouteState>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<OidcCallbackQuery>,
) -> Result<Response, ApiError> {
    // The callback must land in the browser that started this login.
    let bound = cookies::OIDC_STATE_COOKIE
        .read(&headers)
        .is_some_and(|cookie| constant_time_eq(&cookie, &hash_token(&query.state)));
    if !bound {
        tracing::debug!("oidc callback without the matching state cookie");
        return Err(AuthRouteError::auth_failed(OIDC_FAILED).into());
    }
    let config = state.config.oidc_config();
    let user_agent = user_agent_of(&headers);
    match state
        .login
        .handle_callback(&config, &query.code, &query.state, user_agent.as_deref())
        .await
    {
        Ok(exchange_code) => {
            let location = format!("{}/auth/callback?code={exchange_code}", state.base_path);
            let value = HeaderValue::from_str(&location).map_err(|_| {
                AuthRouteError::internal(&"callback redirect is not a header value")
            })?;
            let mut response = StatusCode::FOUND.into_response();
            let response_headers = response.headers_mut();
            response_headers.insert(axum::http::header::LOCATION, value);
            for value in cookies::OIDC_STATE_COOKIE.clear(&oidc_state_cookie_path(&state.base_path))
            {
                cookies::push_set_cookie(response_headers, &value);
            }
            insert_no_store(response_headers);
            Ok(response)
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "oidc callback rejected");
            Err(AuthRouteError::auth_failed(OIDC_FAILED).into())
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// OIDC exchange: swaps the one-time callback code for a session. Cookie
/// mode (default) sets the session cookie and the body carries no token;
/// Bearer mode returns the raw token once.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/oidc/exchange",
    request_body = OidcExchangeBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer mode"),
        (status = 401, description = "Invalid or expired code")
    )
)]
pub async fn oidc_exchange_handler(
    State(state): State<OidcRouteState>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<OidcExchangeBody>,
) -> Result<Response, ApiError> {
    match state.login.exchange_code(&body.code).await {
        Ok((user, raw_token)) => {
            let body_value = serde_json::to_value(FederatedUserView::from(&user))
                .map_err(|cause| AuthRouteError::internal(&cause))?;
            Ok(federated_login_response(
                &Handoff {
                    base_path: &state.base_path,
                    secure: cookies::secure_for_sign_in(
                        uri.scheme_str().unwrap_or("http"),
                        &headers,
                        state.trusted_proxies.is_trusted(peer.0),
                        &state.stated,
                    ),
                    transport: body.transport,
                },
                body_value,
                raw_token,
            ))
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "oidc exchange rejected");
            Err(AuthRouteError::auth_failed(OIDC_BAD_CODE).into())
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// State for the Jellyfin route.
#[derive(Clone)]
pub struct JellyfinRouteState {
    /// Jellyfin login service.
    pub login: JellyfinLogin,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
    /// Proxies trusted to set `X-Forwarded-Proto` for the cookie `Secure`
    /// flag; loopback by default.
    pub trusted_proxies: TrustedProxies,
    /// The saved public addresses: a sign-in from a stated `https` one gets
    /// a `Secure` cookie. None is stated by default.
    pub stated: StatedAddresses,
    /// Request limiter; each login also takes a per-username token, like
    /// local login, so one Jellyfin account cannot be guessed at full rate.
    pub limits: Arc<RateLimiter>,
}

impl JellyfinRouteState {
    /// Wire the state from the store, client, links and sessions.
    pub fn new(
        users: FederatedUsers,
        idp: JellyfinHttp,
        links: MediaLinks,
        sessions: Sessions,
        base_path: &str,
    ) -> Self {
        Self {
            login: JellyfinLogin::new(users, idp, links, sessions),
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
            stated: StatedAddresses::default(),
            limits: Arc::new(RateLimiter::new()),
        }
    }

    /// Trust the given proxies for the forwarded proto.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    /// Mark the cookie `Secure` for a sign-in from a stated `https` address.
    pub fn with_stated_addresses(mut self, stated: StatedAddresses) -> Self {
        self.stated = stated;
        self
    }

    /// Share the app's request limiter (per-username login budget).
    pub fn with_limits(mut self, limits: Arc<RateLimiter>) -> Self {
        self.limits = limits;
        self
    }
}

/// Jellyfin login: checks credentials against the configured server, imports
/// the user, and mints a session. Transport rule matches local login.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/jellyfin/login",
    request_body = JellyfinLoginBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer mode"),
        (status = 401, description = "Invalid credentials"),
        (status = 429, description = "Too many attempts for this client or username"),
        (status = 503, description = "Jellyfin unavailable")
    )
)]
pub async fn jellyfin_login_handler(
    State(state): State<JellyfinRouteState>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<JellyfinLoginBody>,
) -> Result<Response, ApiError> {
    let username_key = format!("jellyfin-username:{}", body.username.trim().to_lowercase());
    let budget = state.limits.check(LOGIN_CLASS, &username_key);
    if !budget.allowed {
        return Err(rate_limited(LOGIN_CLASS, budget.retry_after_secs));
    }
    let user_agent = user_agent_of(&headers);
    match state
        .login
        .login(&body.username, &body.password, user_agent.as_deref())
        .await
    {
        Ok((user, raw_token)) => {
            let body_value = serde_json::to_value(FederatedUserView::from(&user))
                .map_err(|cause| AuthRouteError::internal(&cause))?;
            Ok(federated_login_response(
                &Handoff {
                    base_path: &state.base_path,
                    secure: cookies::secure_for_sign_in(
                        uri.scheme_str().unwrap_or("http"),
                        &headers,
                        state.trusted_proxies.is_trusted(peer.0),
                        &state.stated,
                    ),
                    transport: body.transport,
                },
                body_value,
                raw_token,
            ))
        }
        Err(FederatedError::Authentication(cause)) => {
            tracing::debug!(%cause, "jellyfin login rejected");
            Err(AuthRouteError::unauthorized(JELLYFIN_INVALID).into())
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// State for the Plex journey routes.
#[derive(Clone)]
pub struct PlexRouteState {
    /// Unified Plex journey.
    pub journey: PlexJourney,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
    /// Proxies trusted to set `X-Forwarded-Proto` for the cookie `Secure`
    /// flag; loopback by default.
    pub trusted_proxies: TrustedProxies,
    /// The saved public addresses: a sign-in from a stated `https` one gets
    /// a `Secure` cookie. None is stated by default.
    pub stated: StatedAddresses,
}

impl PlexRouteState {
    /// Wire the state from the store, client, links and sessions.
    pub fn new(
        users: FederatedUsers,
        client: PlexTv,
        links: MediaLinks,
        sessions: Sessions,
        base_path: &str,
    ) -> Self {
        Self {
            journey: PlexJourney::new(users, client, links, sessions),
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
            stated: StatedAddresses::default(),
        }
    }

    /// Trust the given proxies for the forwarded proto.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    /// Mark the cookie `Secure` for a sign-in from a stated `https` address.
    pub fn with_stated_addresses(mut self, stated: StatedAddresses) -> Self {
        self.stated = stated;
        self
    }
}

/// Run one start and render it. The secret is returned once and never
/// stored in clear.
async fn start_rendered(
    state: &PlexRouteState,
    purpose: PlexPurpose,
    caller: Option<&str>,
) -> Result<Json<PlexStartBody>, ApiError> {
    match state.journey.start(purpose, caller).await {
        Ok(started) => Ok(Json(PlexStartBody {
            pin_id: started.pin_id,
            authorize_url: started.authorize_url,
            pin_secret: started.pin_secret,
        })),
        Err(PlexStartDenied::NotConfigured) => Err(AuthRouteError::InvalidInput {
            message: PLEX_NOT_CONFIGURED.to_owned(),
        }
        .into()),
        Err(PlexStartDenied::Forbidden) => Err(RoleRequired::Admin.into()),
        // No caller credential is at fault here: the IdP is down, the
        // server lookup failed, or login is switched off.
        Err(PlexStartDenied::StartFailed(error)) => Err(federated_unavailable(&error).into()),
        Err(PlexStartDenied::Busy) => {
            Err(AuthRouteError::unavailable(&"too many Plex sign-ins are waiting").into())
        }
    }
}

/// Start a Plex sign-in: mints a PIN, its browser URL, and the secret
/// every poll must present. 503 while Plex login is switched off.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/plex/start",
    responses(
        (status = 200, description = "Fresh PIN, browser URL and poll secret", body = PlexStartBody),
        (status = 503, description = "Plex login is off, or Plex is unreachable")
    )
)]
pub async fn plex_start_handler(
    State(state): State<PlexRouteState>,
) -> Result<Json<PlexStartBody>, ApiError> {
    start_rendered(&state, PlexPurpose::Login, None).await
}

/// Start linking the caller's Plex account. 400 without a configured Plex
/// server.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/plex/start/link",
    responses(
        (status = 200, description = "Fresh PIN, browser URL and poll secret", body = PlexStartBody),
        (status = 400, description = "No Plex server is configured"),
        (status = 401, description = "Missing or invalid session"),
        (status = 503, description = "Plex or the configured server is unreachable")
    )
)]
pub async fn plex_start_link_handler(
    State(state): State<PlexRouteState>,
    User(caller): User,
) -> Result<Json<PlexStartBody>, ApiError> {
    start_rendered(&state, PlexPurpose::Link, Some(caller.user_id())).await
}

/// Start the settings sign-in that hands an admin their Plex token for
/// the server settings. Admin only.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/plex/start/connect",
    responses(
        (status = 200, description = "Fresh PIN, browser URL and poll secret", body = PlexStartBody),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Admin role required"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_start_connect_handler(
    State(state): State<PlexRouteState>,
    admin: Admin,
) -> Result<Json<PlexStartBody>, ApiError> {
    start_rendered(&state, PlexPurpose::Connect, Some(admin.user_id())).await
}

/// Plex login completion: polls the PIN, and once authorized imports the
/// user and mints a session. Pending polls carry only the flag.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/plex/poll/login",
    request_body = PlexLoginPollBody,
    responses(
        (status = 200, description = "Pending flag, or the user once authorized", body = PlexLoginPollResult),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_login_handler(
    State(state): State<PlexRouteState>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<PlexLoginPollBody>,
) -> Result<Response, ApiError> {
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
            .map_err(|cause| AuthRouteError::internal(&cause))?;
            let handoff = Handoff {
                base_path: &state.base_path,
                secure: cookies::secure_for_sign_in(
                    uri.scheme_str().unwrap_or("http"),
                    &headers,
                    state.trusted_proxies.is_trusted(peer.0),
                    &state.stated,
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
            }
            .into())
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// Plex link completion: polls the PIN and, once authorized, stores the
/// verified account as the caller's Plex media link (v2 parity). Requires
/// a session; the answer carries the Plex user name, never its tokens.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/plex/poll/link",
    request_body = PlexPinBody,
    responses(
        (status = 200, description = "Pending flag, or the linked Plex user name", body = PlexLinkPollResult),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_link_handler(
    State(state): State<PlexRouteState>,
    User(caller): User,
    ValidJson(body): ValidJson<PlexPinBody>,
) -> Result<Json<PlexLinkPollResult>, ApiError> {
    let claim = PinClaim {
        pin_id: body.pin_id,
        pin_secret: &body.pin_secret,
        user_id: Some(caller.user_id()),
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
            }
            .into())
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// Plex settings completion: polls the PIN and returns the raw auth token
/// untouched. Requires a session (the token is an account credential).
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/plex/poll/connect",
    request_body = PlexPinBody,
    responses(
        (status = 200, description = "Pending flag, or the raw auth token", body = PlexConnectPollResult),
        (status = 401, description = "Missing or invalid session"),
        (status = 403, description = "Access denied"),
        (status = 503, description = "Plex unreachable")
    )
)]
pub async fn plex_poll_connect_handler(
    State(state): State<PlexRouteState>,
    User(caller): User,
    ValidJson(body): ValidJson<PlexPinBody>,
) -> Result<Json<PlexConnectPollResult>, ApiError> {
    let claim = PinClaim {
        pin_id: body.pin_id,
        pin_secret: &body.pin_secret,
        user_id: Some(caller.user_id()),
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
            }
            .into())
        }
        Err(error) => Err(federated_unavailable(&error).into()),
    }
}

/// Non-caller-fault federated failures: misconfigured or unreachable IdPs
/// are 503, store/RNG faults are 500. Bodies stay fixed; the cause goes to
/// the log.
fn federated_unavailable(error: &FederatedError) -> AuthRouteError {
    match error {
        FederatedError::NotConfigured(_) | FederatedError::ProviderUnavailable(_) => {
            AuthRouteError::unavailable(error)
        }
        FederatedError::Authentication(_) => AuthRouteError::unavailable(error),
        FederatedError::StoreUnavailable(_)
        | FederatedError::UsernameTaken
        | FederatedError::RngUnavailable => AuthRouteError::internal(error),
    }
}

/// How one login-shaped response hands the session over.
struct Handoff<'a> {
    base_path: &'a str,
    /// Mark the cookie `Secure`, as `cookies::secure_for_sign_in` decided.
    secure: cookies::SecureMark,
    transport: TransportDto,
}

/// One login-shaped federated response. The session row already exists (the
/// session issue wrote it); this only hands the token over under the login
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
    if let Ok(value) = crate::session::login::NO_STORE.parse() {
        headers.insert(axum::http::header::CACHE_CONTROL, value);
    }
}
