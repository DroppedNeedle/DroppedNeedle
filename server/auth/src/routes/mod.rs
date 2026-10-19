//! Native auth HTTP routes: login, logout, setup, and federated login.
//!
//! Thin handlers over the auth services (services own logic; this layer
//! owns status mapping, cookies, and envelopes only). Each handler takes
//! the production route state; [`crate::wiring::routes`] mounts them.
//!
//! ## Paths (public vs session-gated)
//!
//! Most paths below are in the public group, but the Plex link/connect
//! polls are session-gated (they hand out account sessions for a PIN id).
//! The group each route is mounted in decides; this list follows it:
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
//! POST /api/v3/auth/plex/start/link             (session-gated)
//! POST /api/v3/auth/plex/start/connect          (session-gated, admin)
//! POST /api/v3/auth/plex/poll/login             (public)
//! POST /api/v3/auth/plex/poll/link              (session-gated)
//! POST /api/v3/auth/plex/poll/connect           (session-gated)
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

use crate::session::{cookies, extract::Transport, login::NO_STORE};

/// Hand a fresh session to the client. `user_json` is the route's own body
/// object; Bearer mode adds the raw token to it once, cookie mode sets the
/// session cookie and keeps the token out of the body. Always `no-store`.
pub fn login_handoff(
    base_path: &str,
    secure: cookies::SecureMark,
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
        for value in cookies::set_session_cookie(raw_token, base_path, secure) {
            cookies::push_set_cookie(headers, &value);
        }
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    response
}
