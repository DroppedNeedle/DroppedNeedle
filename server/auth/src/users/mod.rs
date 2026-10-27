//! Users, roles, devices, recovery, app passwords, per-user Last.fm.
//!
//! The users routes, mounted under `/api/v3`. Clean-slate shapes;
//! no v2 wire compatibility is kept. Role semantics are unchanged: `user`
//! requests await approval while `trusted`/`admin` auto-approve and are
//! quota-exempt; curator means admin-or-trusted.
//!
//! ## Wiring (as built)
//!
//! [`routes`] mounts through [`AuthSetup`][wiring-setup] (`wiring.rs`); the
//! user and admin groups sit inside the session middleware (it puts the
//! request's [`Principal`] in the extensions, which the role extractors read).
//! `POST /api/v3/auth/password-recovery/reset` is in the public group; the
//! origin check guards cookie mutations. The repositories are the
//! concrete types in [`crate::sqlite`] and [`crate::avatars`]; hashing is
//! [`Passwords`][prod-hasher] (bcrypt verify, Argon2id hash, rehash of a
//! weaker hash on login). The `auth_providers` scheme tag lives in
//! the `provider_data` JSON (`{"password_hash", "scheme"}`); no migration
//! added a scheme column.
//!
//! [wiring-setup]: super::wiring::AuthSetup
//! [prod-hasher]: super::passwords::Passwords

pub mod error;
pub mod handlers;
pub mod hibp;
pub mod import;
pub mod lastfm_http;
pub mod lastfm_models;
pub mod lastfm_pending;
pub mod models;
pub mod roles;
pub mod services;
pub mod stores;

use std::sync::Arc;

use axum::extract::FromRef;
use droppedneedle_kernel::mount::{Routes, routes};

pub use error::UsersError;
pub use roles::{Admin, Curator, Principal, Role, RoleRequired, SessionKind, User};

use crate::avatars::Avatars;
use crate::sqlite::{AppPasswords, LastFmLinks, RecoveryCodes, Sessions, Users};
use droppedneedle_kernel::{clock::Clock, ids::IdGenerator, runtime_config::crypto::Crypto};
use hibp::HibpScreen;
use stores::{LastFmAuthClient, LastFmSwitch, UserDirectory};

/// Every dependency the users routes need, injected by constructor.
#[derive(Clone)]
pub struct UsersDeps {
    /// Account and credential rows.
    pub users: Users,
    /// Session management rows (the session-list backend over `auth_tokens`).
    pub sessions: Sessions,
    /// App-password rows.
    pub app_passwords: AppPasswords,
    /// Per-user Last.fm links.
    pub lastfm: LastFmLinks,
    /// Recovery code rows.
    pub recovery: RecoveryCodes,
    /// Avatar image bytes.
    pub avatars: Avatars,
    /// Password hashing and verification.
    pub passwords: super::passwords::Passwords,
    /// Breach-corpus screening, switched by the live `security` section.
    pub screen: HibpScreen,
    /// Clock for issue/expiry stamps.
    pub clock: Arc<dyn Clock>,
    /// Fresh ids for rows.
    pub ids: Arc<dyn IdGenerator>,
    /// At-rest encryption for app secrets and Last.fm keys.
    pub crypto: Arc<Crypto>,
    /// Last.fm auth web calls.
    pub lastfm_client: Arc<dyn LastFmAuthClient>,
    /// Last.fm switch and instance key pair (live read).
    pub lastfm_switch: LastFmSwitch,
    /// Last.fm sign-in tokens waiting for approval, per requesting user.
    pub lastfm_pending: Arc<lastfm_pending::LastFmPending>,
    /// Jellyfin accounts for the admin user import.
    pub jellyfin_directory: Arc<dyn UserDirectory>,
    /// Plex accounts for the admin user import.
    pub plex_directory: Arc<dyn UserDirectory>,
}

/// Now, unix seconds, from the injected clock.
pub fn clock_now(deps: &UsersDeps) -> i64 {
    deps.clock.now_unix()
}

/// Every users route, in the group its handler's extractor implies.
/// `POST /auth/password-recovery/reset` is public: the recovery code is
/// its credential.
pub fn routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    UsersDeps: FromRef<S>,
{
    use handlers::*;

    Routes::new()
        .user(routes!(get_profile, patch_profile))
        .user(routes!(put_username))
        .user(routes!(put_email))
        .user(routes!(post_password))
        .user(routes!(post_local_password))
        .user(routes!(post_avatar))
        .user(routes!(get_avatar))
        .user(routes!(list_sessions))
        .user(routes!(revoke_session))
        .user(routes!(mint_device_session))
        .user(routes!(logout_all))
        .user(routes!(list_app_passwords, create_app_password))
        .user(routes!(revoke_app_password))
        .user(routes!(
            lastfm_status,
            lastfm_set_credentials,
            lastfm_unlink
        ))
        .user(routes!(lastfm_token))
        .user(routes!(lastfm_session))
        .admin(routes!(admin_list_users, admin_create_user))
        .admin(routes!(admin_get_user, admin_delete_user))
        .admin(routes!(admin_set_role))
        .admin(routes!(admin_revoke_sessions))
        .admin(routes!(admin_mint_recovery_code))
        .admin(routes!(admin_list_app_passwords))
        .admin(routes!(admin_revoke_app_password))
        .admin(routes!(admin_import_list_jellyfin))
        .admin(routes!(admin_import_list_plex))
        .admin(routes!(admin_import_users))
        .public(routes!(reset_password))
}
