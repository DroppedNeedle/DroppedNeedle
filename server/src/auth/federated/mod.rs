//! Federated login slice: OIDC, Jellyfin, and the unified Plex journey,
//! plus v2 password-hash import with Argon2id rehash.
//!
//! Stage 3 scope, stage0-auth.md D5/D6. OIDC keeps PKCE states and the
//! short-lived exchange code; Jellyfin keeps the per-user connection
//! auto-link; Plex unifies v2's three PIN flows (login, link, settings
//! OAuth) into one journey (R4). v2 bcrypt rows verify once and rehash
//! to Argon2id on success; sessions never survive import.
//!
//! v2 provenance (read-only): `backend/services/oidc_user_auth_service.py`,
//! `backend/services/jellyfin_user_auth_service.py`,
//! `backend/services/plex_user_auth_service.py`, `backend/services/auth_service.py`
//! (`_make_local_data`, `_verify_password`, `_dummy_verify`),
//! `backend/infrastructure/persistence/auth_store.py` (`_derive_username`,
//! `_slugify`), `backend/api/v1/routes/auth.py`, `me_connections.py`,
//! `plex_auth.py`.
//!
//! No live IdP calls anywhere in this slice: every network edge sits behind
//! a trait (`OidcIdp`, `JellyfinIdp`, `PlexPinClient`) and tests use the
//! scripted fakes in [`fakes`]. Production adapters live with the settings
//! and connection slices; the exact mappings they must implement are
//! documented on each trait.
//!
//! Assumed sibling APIs (owned outside this slice, flagged for wiring):
//!
//! - `session::SessionStore` + `session::tokens` exist on disk; the
//!   production [`SessionIssuer`] wraps `tokens::mint_token`,
//!   `tokens::hash_token`, `SessionStore::insert` (kind `Standard`), and a
//!   `last_login_at` update.
//! - `session::login::PasswordVerifier` (not yet on disk) will own the real
//!   bcrypt/argon2 primitives; the production [`password_import::PasswordHasher`]
//!   delegates to it or to the crates directly.
//! - The user/provider tables adapter implements
//!   [`users::FederatedUserStore`] and MUST seal `token_json` at rest with
//!   the deployment key before writing `provider_data`.
//!
//! New deps at wiring: `bcrypt`, `argon2` (password primitives only).

#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod jellyfin_login;
pub mod oidc;
pub mod password_import;
pub mod plex;
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
/// Assumed sibling API: the production impl mints via
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
