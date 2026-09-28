//! Users, roles, devices, recovery, app passwords, per-user Last.fm.
//!
//! The stage-3 users slice, mounted under `/api/v3`. Clean-slate shapes;
//! no v2 wire compatibility is kept. Role semantics are unchanged: `user`
//! requests await approval while `trusted`/`admin` auto-approve and are
//! quota-exempt; curator means admin-or-trusted.
//!
//! ## Wiring (as built)
//!
//! The routers below mount through [`AuthSetup`][wiring-setup] (`wiring.rs`),
//! nested under `/api/v3` inside the sibling session middleware (it stashes
//! `session::middleware::CurrentSession`, which the role extractors read).
//! `POST /api/v3/auth/password-recovery/reset` is allowlisted public; the
//! origin check guards cookie mutations. Stores are the `sqlite.rs`
//! adapters over the 0001 baseline tables; hashing is the production
//! [`Argon2idHasher`][prod-hasher] (bcrypt verify, Argon2id hash,
//! opportunistic rehash on login). The `auth_providers` scheme tag lives in
//! the `provider_data` JSON (`{"password_hash", "scheme"}`); no migration
//! added a scheme column. The test hasher in `memory.rs` must never be
//! constructed outside tests.
//!
//! [wiring-setup]: super::wiring::AuthSetup
//! [prod-hasher]: super::prod::Argon2idHasher

pub mod error;
pub mod handlers;
pub mod hibp;
pub mod import;
pub mod memory;
pub mod models;
pub mod roles;
pub mod services;
pub mod stores;

use std::sync::Arc;

use axum::{
    Router,
    routing::{delete, get, post, put},
};

pub use error::{UsersError, WWW_AUTHENTICATE_BEARER};
pub use roles::{AuthContext, CurrentAdmin, CurrentCurator, CurrentUser, Role, SessionKind};

use crate::{ids::IdGenerator, runtime_config::crypto::Crypto};
use stores::{
    AppPasswordStore, AvatarStore, Clock, FalliblePasswordHasher, LastFmAuthClient, LastFmStore,
    LastFmSwitch, PasswordScreen, RecoveryStore, SecurityPolicy, SessionManager, UserStore,
};

/// Every dependency this slice needs, injected by constructor.
#[derive(Clone)]
pub struct UsersDeps {
    /// Account and credential rows.
    pub users: Arc<dyn UserStore>,
    /// Session management rows (the R6 backend over the sibling table).
    pub sessions: Arc<dyn SessionManager>,
    /// App-password rows.
    pub app_passwords: Arc<dyn AppPasswordStore>,
    /// Per-user Last.fm links.
    pub lastfm: Arc<dyn LastFmStore>,
    /// Recovery code rows.
    pub recovery: Arc<dyn RecoveryStore>,
    /// Avatar image bytes.
    pub avatars: Arc<dyn AvatarStore>,
    /// Password hashing and verification (fallible Argon2id entry point).
    pub passwords: Arc<dyn FalliblePasswordHasher>,
    /// Breach-corpus screening.
    pub screen: Arc<dyn PasswordScreen>,
    /// Clock for issue/expiry stamps.
    pub clock: Arc<dyn Clock>,
    /// Fresh ids for rows and error ids.
    pub ids: Arc<dyn IdGenerator>,
    /// At-rest encryption for app secrets and Last.fm keys.
    pub crypto: Arc<Crypto>,
    /// Last.fm auth web calls.
    pub lastfm_client: Arc<dyn LastFmAuthClient>,
    /// Last.fm master switch (live read).
    pub lastfm_switch: Arc<dyn LastFmSwitch>,
    /// HIBP knobs (live read).
    pub security: Arc<dyn SecurityPolicy>,
}

/// Now, unix seconds, from the injected clock.
pub fn clock_now(deps: &UsersDeps) -> i64 {
    deps.clock.now_unix()
}

/// Authenticated user routes. The sibling middleware authenticates before
/// these run; the extractors add role gating only.
pub fn users_router(deps: UsersDeps) -> Router {
    Router::new()
        .route(
            "/me",
            get(handlers::get_profile).patch(handlers::patch_profile),
        )
        .route("/me/username", put(handlers::put_username))
        .route("/me/email", put(handlers::put_email))
        .route("/me/password", post(handlers::post_password))
        .route("/me/local-password", post(handlers::post_local_password))
        .route("/me/avatar", post(handlers::post_avatar))
        .route("/users/{id}/avatar", get(handlers::get_avatar))
        .route("/auth/sessions", get(handlers::list_sessions))
        .route("/auth/sessions/{id}", delete(handlers::revoke_session))
        .route("/auth/device-sessions", post(handlers::mint_device_session))
        .route("/auth/logout-all", post(handlers::logout_all))
        .route(
            "/me/app-passwords",
            get(handlers::list_app_passwords).post(handlers::create_app_password),
        )
        .route(
            "/me/app-passwords/{id}",
            delete(handlers::revoke_app_password),
        )
        .route(
            "/me/connections/lastfm",
            get(handlers::lastfm_status)
                .put(handlers::lastfm_set_credentials)
                .delete(handlers::lastfm_unlink),
        )
        .route("/me/connections/lastfm/token", post(handlers::lastfm_token))
        .route(
            "/me/connections/lastfm/session",
            post(handlers::lastfm_session),
        )
        .with_state(deps)
}

/// Admin routes. Every handler takes `CurrentAdmin`.
pub fn admin_router(deps: UsersDeps) -> Router {
    Router::new()
        .route(
            "/admin/users",
            get(handlers::admin_list_users).post(handlers::admin_create_user),
        )
        .route(
            "/admin/users/{id}",
            get(handlers::admin_get_user).delete(handlers::admin_delete_user),
        )
        .route("/admin/users/{id}/role", put(handlers::admin_set_role))
        .route(
            "/admin/users/{id}/sessions",
            delete(handlers::admin_revoke_sessions),
        )
        .route(
            "/admin/users/{id}/recovery-code",
            post(handlers::admin_mint_recovery_code),
        )
        .route(
            "/admin/app-passwords",
            get(handlers::admin_list_app_passwords),
        )
        .route(
            "/admin/app-passwords/{id}",
            delete(handlers::admin_revoke_app_password),
        )
        .route(
            "/admin/import/jellyfin",
            get(handlers::admin_import_list_jellyfin),
        )
        .route("/admin/import/plex", get(handlers::admin_import_list_plex))
        .route("/admin/import", post(handlers::admin_import_users))
        .with_state(deps)
}

/// Public routes. The sibling middleware must allowlist these; they carry
/// their own credential (the recovery code).
pub fn public_router(deps: UsersDeps) -> Router {
    Router::new()
        .route(
            "/auth/password-recovery/reset",
            post(handlers::reset_password),
        )
        .with_state(deps)
}
