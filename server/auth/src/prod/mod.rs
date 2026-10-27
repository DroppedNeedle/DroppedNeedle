//! Production auth adapters: the wiring surface the app owns.
//!
//! This module is the single import point for the production auth types:
//! the password hasher, the repositories over the auth tables and the
//! avatar files, re-exported with one bundle constructor so app wiring
//! touches one path.
//!
//! ## What the wiring calls
//!
//! ```ignore
//! use std::sync::Arc;
//! use droppedneedle_auth::prod::ProdAuth;
//!
//! let auth = ProdAuth::new(&platform);
//! // Native login: LoginService::new(auth.sessions.clone(), auth.passwords.clone(), auth.users.clone())
//! // UsersDeps: users/auth.sessions/auth.app_passwords/auth.recovery/auth.lastfm/auth.avatars,
//! //   passwords: auth.passwords.clone()
//! // Federated: auth.federated.clone() + auth.sessions.clone()
//! // OIDC states: auth.oidc_states.clone()
//! ```
//!
//! ## Pinned choices
//!
//! - Argon2id `m=19456, t=2, p=1` (OWASP minimum, verified 2026-09-28
//!   against the live cheat sheet and the `argon2` 0.5 defaults).
//! - Scheme dispatch on the explicit tag only (`bcrypt` | `argon2id`);
//!   a missing tag on a local row reads as `bcrypt` (v2 rows predate it).
//! - Local credential JSON is `{"password_hash": ..., "scheme": ...}`
//!   in `auth_providers.provider_data` with `provider = 'local'`; no
//!   migration adds a scheme column.
//! - `last_seen_at` rewrites at most once per 5 minutes per token, in a
//!   background write the request does not wait for.
//! - A login against a hash weaker than the pin writes the rehash in the
//!   same transaction as its session (see
//!   [`Sessions::insert_login`]).
//! - Federated `token_json` is sealed with the deployment key on every
//!   write; no read path decrypts it.
//! - App-password verification is decrypt-free (`secret_sha256` +
//!   constant-time compare + touch); `secret_encrypted` round-trips so
//!   exports can re-encrypt it under a fresh key.
//!

pub use super::avatars::Avatars;
pub use super::passwords::{
    OWASP_M_COST_KIB, OWASP_P_COST, OWASP_T_COST, Passwords, StoredPassword, Verdict,
};
pub use super::sqlite::{
    AppPasswords, AuthDb, FederatedUsers, LastFmLinks, OidcStates, RecoveryCodes, Sessions, Users,
    session_expires_at,
};
pub use super::times::{parse_iso, to_iso};

/// Every production auth adapter, built from one database handle.
///
/// Clones share the database, crypto, ids, and clock; hand clones to each
/// router, service, and background loop that needs them.
#[derive(Clone, Debug)]
pub struct ProdAuth {
    /// Shared SQLite handle behind every adapter below.
    pub db: AuthDb,
    /// Argon2id native / bcrypt legacy / dummy unknown.
    pub passwords: Passwords,
    /// Every session over `auth_tokens`: logins, the gate, the session
    /// list and federated sign-ins.
    pub sessions: Sessions,
    /// Accounts, local credentials and the sign-in lookup over
    /// `auth_users` / `auth_providers`.
    pub users: Users,
    /// Federated import over `auth_users` / `auth_providers`.
    pub federated: FederatedUsers,
    /// Compat secrets over `connect_app_passwords`.
    pub app_passwords: AppPasswords,
    /// Single-active-code rows over `auth_password_recovery_codes`.
    pub recovery: RecoveryCodes,
    /// Per-user links over `user_connections`.
    pub lastfm: LastFmLinks,
    /// PKCE states over `auth_oidc_states`.
    pub oidc_states: OidcStates,
    /// Avatar bytes under `<avatar_dir>/avatars/`.
    pub avatars: Avatars,
}

impl ProdAuth {
    /// Build the live bundle over the platform's database: its data key
    /// seals federated tokens, its clock drives management-side expiry, and
    /// avatar files live under its cache folder.
    pub fn new(platform: &droppedneedle_kernel::platform::Platform) -> Self {
        let db = AuthDb::new(&platform.db, platform.clock.clone());
        let (crypto, ids) = (platform.crypto.clone(), platform.ids.clone());
        Self {
            passwords: Passwords::new(),
            sessions: Sessions::new(&db, ids.clone()),
            users: Users::new(&db),
            federated: FederatedUsers::new(&db, crypto, ids),
            app_passwords: AppPasswords::new(&db),
            recovery: RecoveryCodes::new(&db),
            lastfm: LastFmLinks::new(&db),
            oidc_states: OidcStates::new(&db),
            avatars: Avatars::new(&platform.config.cache_dir),
            db,
        }
    }
}
