//! Production auth adapters: the wiring surface the app owns.
//!
//! This module is the single import point for the production auth types:
//! the password hasher and the SQLite adapters over the 0001
//! baseline tables. The auth modules define ports; this module re-exports the production types
//! behind them plus one bundle constructor, so app wiring touches one path.
//!
//! ## What the wiring calls
//!
//! ```ignore
//! use std::sync::Arc;
//! use droppedneedle_auth::prod::ProdAuth;
//!
//! let auth = ProdAuth::new(&platform);
//! // Native login: LoginService::new(auth.sessions.clone(), auth.passwords.clone(), auth.credentials.clone())
//! // UsersDeps: users/auth.sessions/auth.app_passwords/auth.recovery/auth.lastfm/auth.avatars,
//! //   passwords: auth.passwords.clone()
//! // Federated: auth.federated.clone() + auth.issuer.clone()
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
//! - `last_seen_at` rewrites at most once per 5 minutes per token.
//! - A login against a hash weaker than the pin writes the rehash in the
//!   same transaction as its session (see
//!   [`SqliteSessionStore::insert_login`]).
//! - Federated `token_json` is sealed with the deployment key on every
//!   write; no read path decrypts it.
//! - App-password verification is decrypt-free (`secret_sha256` +
//!   constant-time compare + touch); `secret_encrypted` round-trips so
//!   exports can re-encrypt it under a fresh key.
//!

pub use super::avatars::FileAvatarStore;
pub use super::passwords::{
    OWASP_M_COST_KIB, OWASP_P_COST, OWASP_T_COST, Passwords, StoredPassword, Verdict,
};
pub use super::sqlite::{
    AuthDb, SqliteAppPasswordStore, SqliteCredentialLookup, SqliteFederatedStore,
    SqliteLastFmStore, SqliteOidcStateStore, SqliteRecoveryStore, SqliteSessionIssuer,
    SqliteSessionManager, SqliteSessionStore, SqliteUserStore, session_expires_at,
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
    /// Logins + middleware over `auth_tokens`.
    pub sessions: SqliteSessionStore,
    /// Session list/revoke surface over `auth_tokens`.
    pub session_manager: SqliteSessionManager,
    /// Accounts + local credentials over `auth_users` / `auth_providers`.
    pub users: SqliteUserStore,
    /// Federated import over `auth_users` / `auth_providers`.
    pub federated: SqliteFederatedStore,
    /// Post-login session mint + `last_login_at`.
    pub issuer: SqliteSessionIssuer,
    /// Native login lookup (`scheme$hash` tagged rows).
    pub credentials: SqliteCredentialLookup,
    /// Compat secrets over `connect_app_passwords`.
    pub app_passwords: SqliteAppPasswordStore,
    /// Single-active-code rows over `auth_password_recovery_codes`.
    pub recovery: SqliteRecoveryStore,
    /// Per-user links over `user_connections`.
    pub lastfm: SqliteLastFmStore,
    /// PKCE states over `auth_oidc_states`.
    pub oidc_states: SqliteOidcStateStore,
    /// Avatar bytes under `<avatar_dir>/avatars/`.
    pub avatars: FileAvatarStore,
}

impl ProdAuth {
    /// Build the live bundle over the platform's database: its data key
    /// seals federated tokens, its clock drives management-side expiry, and
    /// avatar files live under its cache folder.
    pub fn new(platform: &droppedneedle_kernel::platform::Platform) -> Self {
        let db = AuthDb::new(&platform.db, platform.clock.clone());
        let (crypto, ids, clock) = (
            platform.crypto.clone(),
            platform.ids.clone(),
            platform.clock.clone(),
        );
        Self {
            passwords: Passwords::new(),
            sessions: SqliteSessionStore::new(&db),
            session_manager: SqliteSessionManager::new(&db, clock),
            users: SqliteUserStore::new(&db),
            federated: SqliteFederatedStore::new(&db, crypto, ids.clone()),
            issuer: SqliteSessionIssuer::new(&db, ids),
            credentials: SqliteCredentialLookup::new(&db),
            app_passwords: SqliteAppPasswordStore::new(&db),
            recovery: SqliteRecoveryStore::new(&db),
            lastfm: SqliteLastFmStore::new(&db),
            oidc_states: SqliteOidcStateStore::new(&db),
            avatars: FileAvatarStore::new(&platform.config.cache_dir),
            db,
        }
    }
}
