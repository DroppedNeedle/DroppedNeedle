//! Native auth routes: login, logout, first-run setup, setup status.
//!
//! Thin HTTP over the auth services: [`LoginService`](crate::auth::session::login::LoginService)
//! owns login, the users services own user creation, and these handlers own
//! status mapping, cookies, and envelopes only.

use crate::auth::session::cookies;
use crate::auth::session::extract;
use crate::auth::session::login::{
    INVALID_CREDENTIALS, LoginContext, LoginError, LoginRequest, LoginService, LoginSuccess,
    TransportParam,
};
use crate::auth::session::middleware::TrustedProxies;
use crate::auth::session::store::{SessionKind, SessionRecord, SessionStore, now_unix};
use crate::auth::session::tokens;
use crate::auth::users::UsersDeps;
use crate::auth::users::handlers::ValidJson;
use crate::auth::users::models::UserResponse;
use crate::auth::users::roles::Role;
use crate::auth::users::{clock_now, services};
use crate::ids::IdGenerator;
use axum::{
    Json, Router,
    extract::{ConnectInfo, FromRequestParts, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use super::error::AuthRouteError;
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
pub struct NativeAuthState<S, V, C> {
    /// Token store (setup session mint).
    pub sessions: S,
    /// Local-login service.
    pub login: LoginService<S, V, C>,
    /// User stores, clock, ids, hasher (setup, probe, logout, enrichment).
    pub users: UsersDeps,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
    /// Proxies trusted to set `X-Forwarded-*`; loopback by default (v2
    /// parity). Gates `Secure` marking from `X-Forwarded-Proto`.
    pub trusted_proxies: TrustedProxies,
    /// Serializes first-run setup (see `setup_handler`).
    pub setup_guard: Arc<tokio::sync::Mutex<()>>,
}

impl<S, V, C> NativeAuthState<S, V, C>
where
    S: SessionStore,
    V: crate::auth::session::login::PasswordVerifier,
    C: crate::auth::session::login::CredentialLookup,
{
    /// Wire the state from its ports. The login service is built here from
    /// the same handles so wiring passes each dependency once.
    pub fn new(
        sessions: S,
        verifier: V,
        credentials: C,
        users: UsersDeps,
        base_path: &str,
    ) -> Self {
        Self {
            sessions: sessions.clone(),
            login: LoginService::new(sessions, verifier, credentials),
            users,
            base_path: base_path.to_owned(),
            trusted_proxies: TrustedProxies::default(),
            setup_guard: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Trust the given proxies for forwarded proto. Fed from deployment
    /// config at wiring.
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }
}

/// Native auth routes. Paths are relative; the orchestrator nests this
/// router under `/api/v3` inside the session middleware (all four paths are
/// allowlisted public).
pub fn native_auth_router<S, V, C>(state: NativeAuthState<S, V, C>) -> Router
where
    S: SessionStore,
    V: crate::auth::session::login::PasswordVerifier,
    C: crate::auth::session::login::CredentialLookup,
{
    Router::new()
        .route("/auth/login", post(login_handler))
        .route("/auth/logout", post(logout_handler))
        .route("/auth/setup", post(setup_handler))
        .route("/auth/setup/status", get(setup_status_handler))
        .with_state(state)
}

/// Local login. Cookie mode (default) sets the session cookie and the body
/// carries no token; Bearer mode returns the raw token once and sets no
/// cookie. Failures are one uniform 401 with a dummy-hash verify behind it.
#[utoipa::path(
    post,
    path = "/api/v3/auth/login",
    request_body = LoginBody,
    responses(
        (status = 200, description = "Authenticated user; token only in Bearer mode", body = inline(AuthSessionBody)),
        (status = 401, description = "Invalid username or password")
    )
)]
pub async fn login_handler<S, V, C>(
    State(state): State<NativeAuthState<S, V, C>>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<LoginBody>,
) -> Result<Response, AuthRouteError>
where
    S: SessionStore,
    V: crate::auth::session::login::PasswordVerifier,
    C: crate::auth::session::login::CredentialLookup,
{
    let secure = cookies::is_secure_trusted(
        uri.scheme_str().unwrap_or("http"),
        &headers,
        state.trusted_proxies.is_trusted(peer.0),
    );
    let ctx = LoginContext {
        base_path: state.base_path.clone(),
        secure,
        user_agent: user_agent_of(&headers),
        now_unix: now_unix(),
    };
    let request = LoginRequest {
        username: body.username,
        password: body.password,
        transport: body.transport.as_param(),
    };
    match state.login.login(request, ctx).await {
        Ok(success) => {
            let user = login_user(&state.users, &success.user_id).await?;
            let rendered = render_login(
                &state.base_path,
                secure,
                serde_json::json!({ "user": user }),
                &success,
            );
            Ok(rendered)
        }
        Err(LoginError::InvalidCredentials) => {
            Err(AuthRouteError::unauthorized(INVALID_CREDENTIALS))
        }
        Err(LoginError::Unavailable) => Err(AuthRouteError::internal(
            &LoginError::Unavailable,
            state.users.ids.as_ref(),
        )),
    }
}

/// Logout. Public so a stale client can always clear its cookie: revokes the
/// presented token when it resolves, always clears the cookie, always 204.
#[utoipa::path(
    post,
    path = "/api/v3/auth/logout",
    responses((status = 204, description = "Logged out; cookie cleared"))
)]
pub async fn logout_handler<S, V, C>(
    State(state): State<NativeAuthState<S, V, C>>,
    headers: HeaderMap,
) -> Result<Response, AuthRouteError>
where
    S: SessionStore,
    V: crate::auth::session::login::PasswordVerifier,
    C: crate::auth::session::login::CredentialLookup,
{
    if let Some((raw_token, _)) = extract::extract(&headers) {
        let owner = state
            .users
            .sessions
            .owner_by_hash(&tokens::hash_token(&raw_token))
            .await
            .map_err(|error| {
                AuthRouteError::from(store_failure(&error, state.users.ids.as_ref()))
            })?;
        if let Some(owner) = owner {
            state
                .users
                .sessions
                .revoke_scoped(&owner.user_id, &owner.session_id)
                .await
                .map_err(|error| {
                    AuthRouteError::from(store_failure(&error, state.users.ids.as_ref()))
                })?;
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    cookies::push_set_cookie(
        response.headers_mut(),
        &cookies::clear_cookie_value(&state.base_path),
    );
    Ok(response)
}

/// First-admin setup. Creates the admin and logs them in (201 + session,
/// v2 parity) iff no users exist; otherwise 409. Concurrent setups serialize
/// on the state guard: the loser probes after the winner commits and lands
/// on 409. (The users table has no empty-table constraint, so without the
/// guard two distinct usernames would both succeed.)
#[utoipa::path(
    post,
    path = "/api/v3/auth/setup",
    request_body = SetupBody,
    responses(
        (status = 201, description = "First admin created and logged in", body = inline(AuthSessionBody)),
        (status = 400, description = "Invalid username, password, or email"),
        (status = 409, description = "Setup has already been completed")
    )
)]
pub async fn setup_handler<S, V, C>(
    State(state): State<NativeAuthState<S, V, C>>,
    uri: Uri,
    headers: HeaderMap,
    peer: PeerAddr,
    ValidJson(body): ValidJson<SetupBody>,
) -> Result<Response, AuthRouteError>
where
    S: SessionStore,
    V: crate::auth::session::login::PasswordVerifier,
    C: crate::auth::session::login::CredentialLookup,
{
    let _setup = state.setup_guard.lock().await;
    if users_exist(&state.users).await? {
        return Err(AuthRouteError::Conflict {
            message: SETUP_ALREADY_COMPLETED.to_owned(),
        });
    }
    let user = services::admin_create_user(
        &state.users,
        &body.username,
        &body.password,
        body.display_name.as_deref(),
        body.email.as_deref(),
        Role::Admin,
    )
    .await
    .map_err(AuthRouteError::from)?;
    drop(_setup);
    let now = clock_now(&state.users);
    let raw_token =
        tokens::mint_token().map_err(|error| AuthRouteError::internal(&error, id_gen(&state)))?;
    state
        .sessions
        .insert(SessionRecord {
            id: state.users.ids.new_id(),
            user_id: user.id.clone(),
            token_hash: tokens::hash_token(&raw_token),
            kind: SessionKind::Standard,
            label: None,
            issued_at: now,
            expires_at: tokens::expires_at(now),
            last_seen_at: now,
            revoked: false,
            user_agent: user_agent_of(&headers),
        })
        .await
        .map_err(|error| AuthRouteError::internal(&error, id_gen(&state)))?;
    state
        .users
        .users
        .touch_login(&user.id, now)
        .await
        .map_err(|error| AuthRouteError::from(store_failure(&error, state.users.ids.as_ref())))?;
    let success = LoginSuccess {
        user_id: user.id.clone(),
        display_name: user.display_name.clone(),
        transport: transport_of(body.transport.as_param()),
        raw_token,
        set_cookie: None,
    };
    let secure = cookies::is_secure_trusted(
        uri.scheme_str().unwrap_or("http"),
        &headers,
        state.trusted_proxies.is_trusted(peer.0),
    );
    let mut rendered = render_login(
        &state.base_path,
        secure,
        serde_json::json!({ "user": user }),
        &success,
    );
    *rendered.status_mut() = StatusCode::CREATED;
    Ok(rendered)
}

/// Setup-required probe for the SPA first-run gate.
#[utoipa::path(
    get,
    path = "/api/v3/auth/setup/status",
    responses((status = 200, description = "Whether setup must run", body = SetupStatusBody))
)]
pub async fn setup_status_handler<S, V, C>(
    State(state): State<NativeAuthState<S, V, C>>,
) -> Result<Json<SetupStatusBody>, AuthRouteError>
where
    S: SessionStore,
    V: crate::auth::session::login::PasswordVerifier,
    C: crate::auth::session::login::CredentialLookup,
{
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
        .map_err(|error| AuthRouteError::from(store_failure(&error, users.ids.as_ref())))
}

/// Full login user for the response body. The credential lookup already
/// proved the account exists; a vanished row is a 500, never a partial body.
async fn login_user(users: &UsersDeps, user_id: &str) -> Result<UserResponse, AuthRouteError> {
    let user = users
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| AuthRouteError::from(store_failure(&error, users.ids.as_ref())))?
        .ok_or_else(|| {
            AuthRouteError::internal(&"login credential has no user row", users.ids.as_ref())
        })?;
    let providers = users
        .users
        .provider_names(user_id)
        .await
        .map_err(|error| AuthRouteError::from(store_failure(&error, users.ids.as_ref())))?;
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

/// Render a login success through the sibling renderer: cookie in cookie
/// mode, `token` field only in Bearer mode, `no-store` always. The cookie value
/// is derived here because setup and login share this renderer while only
/// login mints through the service. `secure` is resolved by the caller from
/// the direct scheme plus the trusted-proxy verdict.
fn render_login(
    base_path: &str,
    secure: bool,
    user_json: serde_json::Value,
    success: &LoginSuccess,
) -> Response {
    let cookie = match success.transport {
        extract::Transport::Cookie => Some(cookies::set_cookie_value(
            &success.raw_token,
            base_path,
            secure,
        )),
        extract::Transport::Bearer => None,
    };
    let with_cookie = LoginSuccess {
        user_id: success.user_id.clone(),
        display_name: success.display_name.clone(),
        transport: success.transport,
        raw_token: success.raw_token.clone(),
        set_cookie: cookie,
    };
    crate::auth::session::login::login_response(user_json, &with_cookie)
}

/// Calling `User-Agent`, when the client sent one.
fn user_agent_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Mechanism transport from the request param.
fn transport_of(param: TransportParam) -> extract::Transport {
    match param {
        TransportParam::Cookie => extract::Transport::Cookie,
        TransportParam::Bearer => extract::Transport::Bearer,
    }
}

/// Optional peer address: `Some` when the server runs with connect info,
/// `None` in-process or without it (untrusted either way). Infallible so
/// login and setup stay reachable however the server is wired.
pub struct PeerAddr(Option<SocketAddr>);

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

/// Borrow the id generator out of native state.
fn id_gen<S, V, C>(state: &NativeAuthState<S, V, C>) -> &dyn IdGenerator {
    state.users.ids.as_ref()
}

/// Store failures: conflicts stay conflicts, internals are logged with an
/// id. The users error converts into the route error at the call site.
fn store_failure(
    error: &crate::auth::users::stores::StoreError,
    ids: &dyn crate::ids::IdGenerator,
) -> crate::auth::users::error::UsersError {
    match error {
        crate::auth::users::stores::StoreError::Conflict => {
            crate::auth::users::error::UsersError::Conflict {
                message: "Conflicting state".to_owned(),
            }
        }
        crate::auth::users::stores::StoreError::Internal(cause) => {
            crate::auth::users::error::UsersError::internal(cause, ids)
        }
    }
}
