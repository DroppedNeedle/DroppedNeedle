//! Federated login: OIDC, Jellyfin, and the unified Plex journey, plus v2
//! password-hash import with Argon2id rehash.
//!
//! OIDC keeps PKCE states and the
//! short-lived exchange code; Jellyfin keeps the per-user connection
//! auto-link; Plex unifies v2's three PIN flows (login, link, settings
//! OAuth) into one journey. v2 bcrypt rows verify once and rehash
//! to Argon2id on success; sessions never survive import.
//!
//! Ported from v2's OIDC, Jellyfin, Plex and local auth services, the auth
//! store's username derivation (`_derive_username`, `_slugify`), and the
//! auth, connection and Plex auth routes.
//!
//! Every network edge sits behind a trait (`OidcIdp`, `JellyfinIdp`,
//! `PlexPinClient`, `UserDirectory`) whose live implementation lives in a
//! `*_http` module and talks through the shared outbound client. Wire
//! shapes live in the matching `*_models` module. The services stay free
//! of HTTP, and the integration tests run them against local mock servers.
//!
//! Contracts with the rest of auth:
//!
//! - The production [`SessionIssuer`] wraps `tokens::mint_token`,
//!   `tokens::hash_token`, `SessionStore::insert` (kind `Standard`), and a
//!   `last_login_at` update.
//! - The production [`password_import::PasswordHasher`] is
//!   [`crate::auth::passwords`], over the `bcrypt` and `argon2` crates.
//! - The user/provider tables adapter implements
//!   [`users::FederatedUserStore`] and must seal `token_json` at rest with
//!   the deployment key before writing `provider_data`.

#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod jellyfin_http;
pub mod jellyfin_login;
pub mod jellyfin_models;
pub mod jwt;
pub mod oidc;
pub mod oidc_http;
pub mod oidc_models;
pub mod password_import;
pub mod plex;
pub mod plex_http;
pub mod plex_models;
pub mod settings;
pub mod users;

use thiserror::Error;

/// Failure of a federated login or import step. HTTP mapping lives in the
/// handler layer: `Authentication` is 401 on login routes (403 on the Plex
/// poll, v2 parity), `NotConfigured` and `ProviderUnavailable` are 503,
/// `StoreUnavailable` and `RngUnavailable` are 500 with a fixed body.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FederatedError {
    /// Bad credential, bad/expired state or exchange code, rejected
    /// membership, or any other caller fault. Messages mirror v2 verbatim.
    #[error("authentication failed: {0}")]
    Authentication(String),
    /// The remote IdP failed or answered unexpectedly (v2
    /// `ExternalServiceError`).
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
    /// The provider is disabled or half-configured (v2
    /// `ConfigurationError`).
    #[error("not configured: {0}")]
    NotConfigured(String),
    /// Local persistence failed.
    #[error("store unavailable: {0}")]
    StoreUnavailable(String),
    /// A username insert raced a concurrent create; the caller re-derives.
    #[error("username taken")]
    UsernameTaken,
    /// The OS random source failed; the request fails, never falls back.
    #[error("random source unavailable")]
    RngUnavailable,
}

/// Issues a native opaque session after a federated login succeeds.
/// The production impl mints via
/// `session::tokens`, stores a `Standard` row through
/// `session::SessionStore`, and updates `last_login_at`.
pub trait SessionIssuer: Clone + Send + Sync + 'static {
    /// Mint a session for `user_id` and return the raw token once.
    fn issue_session(
        &self,
        user_id: &str,
        user_agent: Option<&str>,
    ) -> impl Future<Output = Result<String, FederatedError>> + Send;
}

/// Append `value` as one JSON string literal (quotes included).
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    json_escape_into(&mut out, value);
    out.push('"');
    out
}

/// Escape `value` for embedding in a JSON string (no surrounding quotes).
pub fn json_escape_into(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}
