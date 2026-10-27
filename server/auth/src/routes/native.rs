//! Native auth routes: login, logout, first-run setup, setup status.
//!
//! Thin HTTP over the auth services: [`LoginService`](crate::session::login::LoginService)
//! owns login, the users services own user creation, and these handlers own
//! status mapping, cookies, and envelopes only.

use crate::session::cookies;
use crate::session::extract;
use crate::session::login::{
    INVALID_CREDENTIALS, LoginContext, LoginError, LoginRequest, LoginService,
};
use crate::session::rate_limit::{LOGIN_USERNAME_CLASS, RateLimiter, rate_limited};
use crate::session::stated::StatedAddresses;
use crate::session::store::NewSession;
use crate::session::tokens;
use crate::sqlite::Sessions;
use crate::users::UsersDeps;
use crate::users::models::UserResponse;
use crate::users::{clock_now, services};
use axum::{
    Json,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;

use super::error::AuthRouteError;
use super::login_handoff;
use super::models::{LoginBody, SetupBody, SetupStatusBody};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use utoipa::ToSchema;

/// Repeat-setup refusal, v2 message verbatim (v2 answers 409, not 403/404).
pub const SETUP_ALREADY_COMPLETED: &str = "Setup has already been completed";

/// Login/setup success body: the authenticated user, plus the raw session
/// token only in Bearer mode (cookie mode carries it in `Set-Cookie`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuthSessionBody {
    /// Authenticated user.
    pub user: UserResponse,
    /// Raw session token; present only in Bearer mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// State for the native auth routes. The session store mints setup sessions,
/// the login service owns local login, and the users deps own user creation,
/// the setup probe, logout revocation, and login enrichment.
#[derive(Clone)]
pub struct NativeAuthState {
    /// Token store (setup session mint).
    pub sessions: Sessions,
    /// Local-login service.
    pub login: LoginService,
    /// User stores, clock, ids, passwords (setup, probe, logout, enrichment).
    pub users: UsersDeps,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
    /// Proxies trusted to set `X-Forwarded-*`; loopback by default (v2
    /// parity). Gates `Secure` marking from `X-Forwarded-Proto`.
    pub trusted_proxies: TrustedProxies,
    /// The saved public addresses: a sign-in from a stated `https` one gets
    /// a `Secure` cookie. None is stated by default.
    pub stated: StatedAddresses,
    /// Request limiter; login also takes a per-username token from it.
    pub limits: Arc<RateLimiter>,
}

impl NativeAuthState {
    /// Wire the state from its parts. The login service is built here from
    /// the same handles so wiring passes each dependency once.
    pub fn new(
        sessions: Sessions,
        passwords: crate::passwords::Passwords,
        users: UsersDeps,
        base_path: &str,
    ) -> Self {
        Self {
            sessions: sessions.clone(),
            login: LoginService::new(sessions, passwords, users.users.clone()),
            users,
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
            stated: StatedAddresses::default(),
            limits: Arc::new(RateLimiter::new()),
        }
    }

    /// Share the app's request limiter (per-username login budget).
    pub fn with_limits(mut self, limits: Arc<RateLimiter>) -> Self {
        self.limits = limits;
        self
    }

    /// Trust the given proxies for forwarded proto. Fed from deployment
    /// config at wiring.
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

/// Local login. Cookie mode (default) sets the session cookie and the body
/// carries no token; Bearer mode returns the raw token once and sets no
/// cookie. Failures are one uniform 401 with a dummy-hash verify behind it.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/login",
    request_body = LoginBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer mode", body = inline(AuthSessionBody)),
        (status = 401, description = "Invalid username or password"),
        (status = 429, description = "Too many attempts for this client or username")
    )
)]
pub async fn login_handler(
    State(state): State<NativeAuthState>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<LoginBody>,
) -> Result<Response, ApiError> {
    let secure = cookies::secure_for_sign_in(
        uri.scheme_str().unwrap_or("http"),
        &headers,
        state.trusted_proxies.is_trusted(peer.0),
        &state.stated,
    );
    let budget = state.limits.check_login(&body.username);
    if !budget.allowed {
        return Err(rate_limited(LOGIN_USERNAME_CLASS, budget.retry_after_secs));
    }
    let ctx = LoginContext {
        user_agent: user_agent_of(&headers),
        now_unix: clock_now(&state.users),
    };
    let request = LoginRequest {
        username: body.username,
        password: body.password,
        transport: body.transport.as_param(),
    };
    match state.login.login(request, ctx).await {
        Ok(success) => {
            let user = login_user(&state.users, &success.user_id).await?;
            Ok(login_handoff(
                &state.base_path,
                secure,
                serde_json::json!({ "user": user }),
                success.transport,
                &success.raw_token,
            ))
        }
        Err(LoginError::InvalidCredentials) => {
            Err(AuthRouteError::unauthorized(INVALID_CREDENTIALS).into())
        }
        Err(LoginError::Unavailable) => {
            Err(AuthRouteError::internal(&LoginError::Unavailable).into())
        }
        Err(LoginError::Busy) => Err(AuthRouteError::busy(&LoginError::Busy).into()),
    }
}

/// Logout. Public so a stale client can always clear its cookie: revokes the
/// presented sessions that resolve, always clears the cookie under both its
/// names, always 204. A Bearer token is the only one presented when sent;
/// otherwise the session under each cookie name the request carries ends,
/// so a sign-out at the https address leaves no plain cookie signed in.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/logout",
    responses((status = 204, description = "Logged out; cookie cleared"))
)]
pub async fn logout_handler(
    State(state): State<NativeAuthState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let presented = match extract::extract(&headers) {
        Some((raw_token, extract::Transport::Bearer)) => vec![raw_token],
        Some((_, extract::Transport::Cookie)) => cookies::SESSION_COOKIE.read_each(&headers),
        None => Vec::new(),
    };
    for raw_token in presented {
        let owner = state
            .users
            .sessions
            .owner_by_hash(&tokens::hash_token(&raw_token))
            .await
            .map_err(|error| AuthRouteError::from(store_failure(&error)))?;
        if let Some(owner) = owner {
            state
                .users
                .sessions
                .revoke_scoped(&owner.user_id, &owner.session_id)
                .await
                .map_err(|error| AuthRouteError::from(store_failure(&error)))?;
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    for value in cookies::clear_session_cookie(&state.base_path) {
        cookies::push_set_cookie(response.headers_mut(), &value);
    }
    Ok(response)
}

/// First-admin setup. Creates the admin and logs them in (201 + session,
/// v2 parity) iff no users exist; otherwise 409. The emptiness check runs
/// inside the insert transaction, so concurrent setups, or a setup racing
/// a first federated login (whose role is also decided in its insert),
/// leave exactly one admin. The early probe only skips the password hash.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/setup",
    request_body = SetupBody,
    responses(
        (status = 201, description = "First admin created and logged in", body = inline(AuthSessionBody)),
        (status = 400, description = "Invalid username, password, or email"),
        (status = 409, description = "Setup has already been completed")
    )
)]
pub async fn setup_handler(
    State(state): State<NativeAuthState>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<SetupBody>,
) -> Result<Response, ApiError> {
    if users_exist(&state.users).await? {
        return Err(AuthRouteError::Conflict {
            message: SETUP_ALREADY_COMPLETED.to_owned(),
        }
        .into());
    }
    let user = services::create_first_admin(
        &state.users,
        &body.username,
        &body.password,
        body.display_name.as_deref(),
        body.email.as_deref(),
        SETUP_ALREADY_COMPLETED,
    )
    .await
    .map_err(AuthRouteError::from)?;
    let now = clock_now(&state.users);
    let raw_token = tokens::mint_token().map_err(|error| AuthRouteError::internal(&error))?;
    state
        .sessions
        .insert(NewSession {
            user_id: user.id.clone(),
            token_hash: tokens::hash_token(&raw_token),
            issued_at: now,
            user_agent: user_agent_of(&headers),
        })
        .await
        .map_err(|error| AuthRouteError::internal(&error))?;
    state
        .users
        .users
        .touch_login(&user.id, now)
        .await
        .map_err(|error| AuthRouteError::from(store_failure(&error)))?;
    let secure = cookies::secure_for_sign_in(
        uri.scheme_str().unwrap_or("http"),
        &headers,
        state.trusted_proxies.is_trusted(peer.0),
        &state.stated,
    );
    let mut rendered = login_handoff(
        &state.base_path,
        secure,
        serde_json::json!({ "user": user }),
        body.transport.as_param().into(),
        &raw_token,
    );
    *rendered.status_mut() = StatusCode::CREATED;
    Ok(rendered)
}

/// Setup-required probe for the SPA first-run gate.
#[utoipa::path(
    get,
    tag = "auth",
    path = "/api/v3/auth/setup/status",
    responses((status = 200, description = "Whether setup must run", body = SetupStatusBody))
)]
pub async fn setup_status_handler(
    State(state): State<NativeAuthState>,
) -> Result<Json<SetupStatusBody>, ApiError> {
    Ok(Json(SetupStatusBody {
        setup_required: !users_exist(&state.users).await?,
    }))
}

/// True when at least one user exists. The count comes from a one-row page.
async fn users_exist(users: &UsersDeps) -> Result<bool, AuthRouteError> {
    users
        .users
        .list(1, 0)
        .await
        .map(|(_, total)| total > 0)
        .map_err(|error| AuthRouteError::from(store_failure(&error)))
}

/// Full login user for the response body. The credential lookup already
/// proved the account exists; a vanished row is a 500, never a partial body.
async fn login_user(users: &UsersDeps, user_id: &str) -> Result<UserResponse, AuthRouteError> {
    let user = users
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| AuthRouteError::from(store_failure(&error)))?
        .ok_or_else(|| AuthRouteError::internal(&"login credential has no user row"))?;
    let providers = users
        .users
        .provider_names(user_id)
        .await
        .map_err(|error| AuthRouteError::from(store_failure(&error)))?;
    Ok(UserResponse {
        id: user.id,
        username: user.username,
        username_display: user.username_display,
        display_name: user.display_name,
        email: user.email,
        avatar_url: user.avatar_url,
        role: user.role,
        providers,
        created_at: user.created_at,
        last_login_at: user.last_login_at,
    })
}

/// Calling `User-Agent`, when the client sent one.
fn user_agent_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Optional peer address: `Some` when the server runs with connect info,
/// `None` in-process or without it (untrusted either way). Infallible so
/// login and setup stay reachable however the server is wired.
pub struct PeerAddr(pub Option<SocketAddr>);

impl<S> FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0),
        ))
    }
}

/// Store failures: conflicts stay conflicts, busy is a 503, internals carry
/// their cause. The users error converts into the route error at the call
/// site.
fn store_failure(error: &crate::users::stores::StoreError) -> crate::users::error::UsersError {
    match error {
        crate::users::stores::StoreError::Conflict => crate::users::error::UsersError::Conflict {
            message: "Conflicting state".to_owned(),
        },
        crate::users::stores::StoreError::Busy => crate::users::error::UsersError::Busy,
        crate::users::stores::StoreError::Internal(cause) => {
            crate::users::error::UsersError::internal(cause)
        }
    }
}
