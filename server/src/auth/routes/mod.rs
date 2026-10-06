//! Native auth HTTP routes: login, logout, setup, and federated login.
//!
//! Thin Axum routers over the auth services (services own logic; this layer
//! owns status mapping, cookies, and envelopes only). Paths are relative;
//! the orchestrator nests each router under `/api/v3` inside the session
//! middleware and registers the `utoipa::path` handlers in `ApiDoc`.
//!
//! ## Routers and constructors
//!
//! ```ignore
//! use crate::auth::routes::native::{NativeAuthState, native_auth_router};
//! use crate::auth::routes::federated::{
//!     JellyfinRouteState, OidcRouteState, PlexRouteState, jellyfin_router, oidc_router,
//!     plex_router,
//! };
//!
//! let app = Router::new()
//!     .nest("/api/v3", native_auth_router(NativeAuthState::new(
//!         sessions, verifier, credentials, users_deps, base_path,
//!     )))
//!     .nest("/auth...", /* same pattern for oidc/jellyfin/plex */)
//!     .layer(middleware::from_fn_with_state(
//!         SessionAuth::new(sessions, base_path),
//!         require_session,
//!     ));
//! ```
//!
//! Constructor signatures:
//!
//! - `NativeAuthState::new(sessions: S, verifier: V, credentials: C,
//!   users: UsersDeps, base_path: &str)` where `S: SessionStore`,
//!   `V: PasswordVerifier`, `C: CredentialLookup`. The `LoginService` is
//!   built inside from the same handles.
//! - `OidcRouteState::new(login: OidcLogin<S, I, T, E, N>, config: F,
//!   ids: Arc<dyn IdGenerator>, base_path: &str)` where
//!   `S: FederatedUserStore`, `I: OidcIdp`, `T: OidcStateStore`,
//!   `E: OidcExchangeStore`, `N: SessionIssuer`, `F: OidcConfigSource`.
//!   Build the service first via `OidcLogin::new(users, idp, states,
//!   exchanges, sessions)`.
//! - `JellyfinRouteState::new(users: S, idp: I, links: L, sessions: N,
//!   ids: Arc<dyn IdGenerator>, base_path: &str)` where
//!   `S: FederatedUserStore`, `I: JellyfinIdp`, `L: JellyfinConnectionLink`,
//!   `N: SessionIssuer`.
//! - `PlexRouteState::new(users: S, client: C, links: L, sessions: N,
//!   ids: Arc<dyn IdGenerator>, base_path: &str)` where
//!   `S: FederatedUserStore`, `C: PlexPinClient`, `L: PlexConnectionLink`,
//!   `N: SessionIssuer`.
//!
//! ## Paths (public vs session-gated)
//!
//! Most paths below are allowlisted public, but the Plex link/connect
//! polls are session-gated (they hand out account sessions for a PIN id).
//! Ground truth is `crate::auth::session::allowlist`; this list must match it:
//!
//! ```text
//! POST /api/v3/auth/login                       (public)
//! POST /api/v3/auth/logout                      (public)
//! POST /api/v3/auth/setup                       (public)
//! GET  /api/v3/auth/setup/status                (public)
//! GET  /api/v3/auth/providers                   (public)
//! POST /api/v3/auth/oidc/authorize              (public)
//! GET  /api/v3/auth/oidc/callback               (public)
//! POST /api/v3/auth/oidc/exchange               (public)
//! POST /api/v3/auth/jellyfin/login              (public)
//! POST /api/v3/auth/plex/start                  (public)
//! POST /api/v3/auth/plex/poll/login             (public)
//! POST /api/v3/auth/plex/poll/link              (session-gated)
//! POST /api/v3/auth/plex/poll/connect           (session-gated)
//! ```
//!
//! ## utoipa paths to register in `ApiDoc`
//!
//! ```ignore
//! native::login_handler,
//! native::logout_handler,
//! native::setup_handler,
//! native::setup_status_handler,
//! federated::providers_handler,
//! federated::oidc_authorize_handler,
//! federated::oidc_callback_handler,
//! federated::oidc_exchange_handler,
//! federated::jellyfin_login_handler,
//! federated::plex_start_handler,
//! federated::plex_poll_login_handler,
//! federated::plex_poll_link_handler,
//! federated::plex_poll_connect_handler,
//! ```

pub mod error;
pub mod federated;
pub mod models;
pub mod native;

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::auth::session::{cookies, extract::Transport, login::NO_STORE};

/// Hand a fresh session to the client. `user_json` is the route's own body
/// object; Bearer mode adds the raw token to it once, cookie mode sets the
/// session cookie and keeps the token out of the body. Always `no-store`.
pub fn login_handoff(
    base_path: &str,
    secure: bool,
    mut user_json: serde_json::Value,
    transport: Transport,
    raw_token: &str,
) -> Response {
    if transport == Transport::Bearer
        && let Some(object) = user_json.as_object_mut()
    {
        object.insert(
            "token".to_owned(),
            serde_json::Value::String(raw_token.to_owned()),
        );
    }
    let mut response = (StatusCode::OK, Json(user_json)).into_response();
    let headers = response.headers_mut();
    if transport == Transport::Cookie {
        cookies::push_set_cookie(
            headers,
            &cookies::set_cookie_value(raw_token, base_path, secure),
        );
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    response
}
