//! Production auth adapters: the wiring surface the app owns.
//!
//! This module is the single import point for the stage-3 close-out: the
//! Argon2id password hasher and the SQLite adapters over the 0001 baseline
//! tables. Slices define ports; this module re-exports the production types
//! behind them plus one bundle constructor, so app wiring touches one path.
//!
//! ## What the wiring calls
//!
//! ```ignore
//! use std::sync::Arc;
//! use droppedneedle::auth::prod::ProdAuth;
//!
//! let auth = ProdAuth::new(
//!     runtime.pool(),
//!     runtime.lane(),
//!     crypto,
//!     ids,
//!     clock,
//!     avatar_dir,
//! );
//! // Native login: LoginService::new(auth.sessions.clone(), auth.hasher.clone(), auth.credentials.clone())
//! // UsersDeps: users/auth.sessions/auth.app_passwords/auth.recovery/auth.lastfm/auth.avatars,
//! //   passwords: Arc::new(auth.hasher.clone())
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
//! - Bcrypt logins rehash through one shared queue: the verifier pushes the
//!   upgrade, the session insert persists it in the same tx (see
//!   [`RehashQueue`](super::passwords::RehashQueue)).
//! - Federated `token_json` is sealed with the deployment key on every
//!   write; no read path decrypts it.
//! - App-password verification is decrypt-free (`secret_sha256` +
//!   constant-time compare + touch); `secret_encrypted` round-trips so
//!   exports can re-encrypt it under a fresh key.
//!
//! ## Boot states
//!
//! [`ProdAuth::new`] is the live bundle. Skeleton boot (pre-database) holds
//! no bundle at all: [`AuthDb::unwired`] plus [`FileAvatarStore::unwired`]
//! fail closed per adapter, and the wiring keeps them out of serving paths
//! until the runtime opens.

use std::path::Path;
use std::sync::Arc;

use sqlx::SqlitePool;

use super::users::stores::Clock;
use crate::db::WriteLane;
use crate::ids::IdGenerator;
use crate::runtime_config::crypto::Crypto;

pub use super::passwords::{
    Argon2idHasher, OWASP_M_COST_KIB, OWASP_P_COST, OWASP_T_COST, PendingRehash, RehashQueue,
};
pub use super::sqlite::{
    AuthDb, FileAvatarStore, SqliteAppPasswordStore, SqliteCredentialLookup, SqliteFederatedStore,
    SqliteLastFmStore, SqliteOidcStateStore, SqliteRecoveryStore, SqliteSessionIssuer,
    SqliteSessionManager, SqliteSessionStore, SqliteUserStore, session_expires_at,
};
pub use super::times::{parse_iso, to_iso};

/// Every production auth adapter, built from one database handle.
///
/// Clones share the pool, lane, crypto, ids, and clock; hand clones to each
/// router, service, and background loop that needs them.
#[derive(Clone, Debug)]
pub struct ProdAuth {
    /// Shared SQLite handle behind every adapter below.
    pub db: AuthDb,
    /// Argon2id native / bcrypt legacy / dummy unknown.
    pub hasher: Argon2idHasher,
    /// Logins + middleware over `auth_tokens`.
    pub sessions: SqliteSessionStore,
    /// R6 list/revoke surface over `auth_tokens`.
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
    /// Build the live bundle. `pool` serves reads, `lane` serializes writes,
    /// `crypto` seals federated tokens, `ids` mints row ids, `clock` drives
    /// management-side expiry, and `avatar_dir` roots avatar files.
    pub fn new(
        pool: &SqlitePool,
        lane: &WriteLane,
        crypto: Arc<Crypto>,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
        avatar_dir: &Path,
    ) -> Self {
        let db = AuthDb::new(pool, lane);
        let hasher = Argon2idHasher::new();
        // Opportunistic rehash: the login verifier queues bcrypt upgrades
        // and the session insert drains them. One shared queue or the login
        // path silently stops rehashing.
        let mut sessions = SqliteSessionStore::new(&db);
        sessions.set_rehash_queue(hasher.rehash_queue().clone());
        Self {
            sessions,
            session_manager: SqliteSessionManager::new(&db, clock),
            users: SqliteUserStore::new(&db),
            federated: SqliteFederatedStore::new(&db, crypto, ids.clone()),
            issuer: SqliteSessionIssuer::new(&db, ids),
            credentials: SqliteCredentialLookup::new(&db),
            app_passwords: SqliteAppPasswordStore::new(&db),
            recovery: SqliteRecoveryStore::new(&db),
            lastfm: SqliteLastFmStore::new(&db),
            oidc_states: SqliteOidcStateStore::new(&db),
            avatars: FileAvatarStore::new(avatar_dir),
            hasher,
            db,
        }
    }
}
