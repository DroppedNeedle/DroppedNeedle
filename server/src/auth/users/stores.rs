//! Ports: the seams the users routes depend on.
//!
//! Every external behavior sits behind one of these traits so tests inject
//! fakes (see `memory.rs`). The SQLite implementations live in
//! [`crate::auth::sqlite`]; each trait documents its table mapping. Futures
//! are boxed by hand because `async fn` is not object-safe.

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

use super::super::federated::password_import::PasswordHasher;
use super::super::federated::users::ProviderBinding;
use super::models::{
    AppPasswordRecord, LastFmConnection, LocalCredential, ManagedSession, RecoveryCode,
    SessionOwner, UserRecord,
};
use super::roles::Role;

/// Boxed sendable future for object-safe async ports.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Every way a store call can fail. `Internal` carries a cause for the log;
/// only `Conflict` ever shapes a user-facing message.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    /// A uniqueness or state precondition failed (username taken, id busy).
    #[error("conflicting state")]
    Conflict,
    /// Anything unexpected. The string goes to the log only, never the wire.
    #[error("store failure: {0}")]
    Internal(String),
}

/// Outcome of a guarded role change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleChange {
    /// The role was written (or already held).
    Changed,
    /// No such user.
    NotFound,
    /// The change would leave no admin; nothing was written.
    LastAdmin,
}

/// Outcome of a guarded user delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserDeletion {
    /// The row and everything cascading from it are gone.
    Deleted,
    /// No such user.
    NotFound,
    /// The user is the last admin; nothing was deleted.
    LastAdmin,
    /// Library history that must outlive accounts names this user (the
    /// tables reference it with `ON DELETE RESTRICT`); nothing was deleted.
    /// Holds a readable name per kind of record.
    Referenced(Vec<&'static str>),
}

/// Account and credential rows.
///
/// Table mapping: `auth_users` + `auth_providers` (provider `local`,
/// `provider_uid` = username). The local credential lives in the
/// `provider_data` JSON (`{"password_hash", "scheme"}`); a missing scheme
/// reads as `bcrypt` (v2 rows predate it).
pub trait UserStore: Send + Sync {
    /// Fetch one user by id. None is absence, never failure.
    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>>;
    /// Fetch one user by lowercased username.
    fn get_by_username<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>>;
    /// Fetch one user by lowercased email.
    fn get_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>>;
    /// Fetch several users by id (admin app-password owner enrichment).
    fn get_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<UserRecord>, StoreError>>;
    /// Insert a user row. Conflict when the id, username, or email is taken.
    fn insert<'a>(&'a self, user: UserRecord) -> BoxFuture<'a, Result<(), StoreError>>;
    /// Insert a user row and its local credential in one transaction.
    /// Conflict when the id, username, or email is taken; nothing is
    /// written then.
    fn insert_with_local_credential<'a>(
        &'a self,
        user: UserRecord,
        credential: LocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    /// First-run setup: insert the user and its local credential only when
    /// no user exists yet, decided inside the same transaction. `Ok(false)`
    /// when any user (local or federated) is already there; nothing is
    /// written then.
    fn insert_first_user<'a>(
        &'a self,
        user: UserRecord,
        credential: LocalCredential,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Update display name and/or avatar URL. None fields stay untouched.
    fn update_profile<'a>(
        &'a self,
        id: &'a str,
        display_name: Option<&'a str>,
        avatar_url: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Rename a user, syncing the local `provider_uid` in the same write.
    /// Conflict when the username is taken. Returns false for unknown ids.
    fn update_username<'a>(
        &'a self,
        id: &'a str,
        username: &'a str,
        username_display: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Set or clear the email. Conflict when another account holds it.
    fn update_email<'a>(
        &'a self,
        id: &'a str,
        email: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Set the role, refusing to demote the last admin. The admin count is
    /// read inside the same write transaction as the update.
    fn set_role<'a>(
        &'a self,
        id: &'a str,
        role: Role,
    ) -> BoxFuture<'a, Result<RoleChange, StoreError>>;
    /// Stamp the last-login time.
    fn touch_login<'a>(&'a self, id: &'a str, at: i64) -> BoxFuture<'a, Result<(), StoreError>>;
    /// Delete a user, refusing the last admin and users that library
    /// history still names. Cascades to providers, tokens, codes, app
    /// passwords, and connections via the baseline foreign keys.
    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<UserDeletion, StoreError>>;
    /// One page of users (creation order, id tiebreak) plus the total count.
    fn list<'a>(
        &'a self,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<UserRecord>, u64), StoreError>>;
    /// Bound provider names for one user, e.g. `["local"]`.
    fn provider_names<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<String>, StoreError>>;
    /// The local credential, when the account has one.
    fn local_credential<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LocalCredential>, StoreError>>;
    /// Insert the local credential. Conflict when one already exists or the
    /// `(local, username)` binding is taken.
    fn insert_local_credential<'a>(
        &'a self,
        credential: LocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    /// Replace the local hash and revoke every other session of the user,
    /// in one transaction. Guarded by the expected current hash so a
    /// concurrent change fails instead of silently winning; returns false
    /// (and writes nothing) when the guard mismatches. `keep_session_id`
    /// is the session making the change, which stays live.
    fn change_local_hash<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
        keep_session_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Complete a recovery reset atomically: the guarded hash replace plus
    /// revoking every session of the user plus deleting the consumed
    /// recovery code, in one transaction. Returns false when the guard
    /// mismatches (nothing is revoked or deleted then). The SQLite adapter
    /// folds all three writes into one write tx (v2 parity).
    fn complete_recovery_reset<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Fetch one federated (non-local) binding by `(provider, provider_uid)`.
    /// None is absence, never failure.
    fn get_provider_binding<'a>(
        &'a self,
        provider: &'a str,
        provider_uid: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderBinding>, StoreError>>;
    /// Insert a pre-linked federated binding: `provider_data` stays NULL (a
    /// login identity, not a credential store; the first SSO login seals the
    /// real tokens). Conflict when the `(provider, provider_uid)` pair is
    /// taken.
    fn insert_provider_binding<'a>(
        &'a self,
        binding: ProviderBinding,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// Session management rows (the session-list backend).
///
/// The session module owns the middleware/login store
/// (`session::store::SessionStore`: insert + valid lookup) and assigns the
/// list/revoke surface to the users routes. This port is that surface, plus the
/// atomic companion replace and a lookup used only to pin native/compat
/// credential separation. The wiring-step SQLite adapter implements both
/// traits over the one `auth_tokens` table; the memory fake in `memory.rs`
/// proves this shape is implementable.
///
/// Table mapping: `auth_tokens`. Companion rows carry
/// `user_agent = "DroppedNeedle companion · {label}"` and
/// `session_kind = 'companion'`; listing strips the prefix back to the label.
pub trait SessionManager: Send + Sync {
    /// Live sessions for one user (unrevoked, unexpired): newest first, id tiebreak.
    /// Rows carry no token hashes.
    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ManagedSession>, StoreError>>;
    /// Revoke one session, but only when it belongs to the user. Returns
    /// false for unknown or foreign ids (no cross-user oracle).
    fn revoke_scoped<'a>(
        &'a self,
        user_id: &'a str,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Revoke every session of one user. Returns the revoked count.
    fn revoke_all_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>>;
    /// Insert a companion token and revoke the prior live same-label
    /// companion token for the user, atomically, in one transaction.
    fn replace_companion<'a>(
        &'a self,
        id: &'a str,
        user_id: &'a str,
        token_hash: &'a str,
        label: &'a str,
        issued_at: i64,
        expires_at: i64,
    ) -> BoxFuture<'a, Result<ManagedSession, StoreError>>;
    /// Resolve a live token hash to its owner. None for unknown, revoked,
    /// or expired hashes. Exists so tests can pin that app-password
    /// secrets never resolve as native sessions.
    fn owner_by_hash<'a>(
        &'a self,
        token_hash: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionOwner>, StoreError>>;
}

/// App-password rows.
///
/// Table mapping: `connect_app_passwords`. Verification is decrypt-free:
/// `secret_sha256` carries over verbatim on import while `secret_encrypted`
/// is re-encrypted under the v3 key.
pub trait AppPasswordStore: Send + Sync {
    /// Insert a row unless the owner already holds `max_active` live rows;
    /// the count and the insert share one transaction. Returns false when
    /// the cap refused it. Conflict when the id or secret hash is taken.
    fn insert_capped<'a>(
        &'a self,
        row: AppPasswordRecord,
        max_active: u64,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Fetch one row by id, regardless of revocation.
    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>>;
    /// Fetch the live row for a secret hash. None when unknown or revoked.
    fn get_active_by_sha256<'a>(
        &'a self,
        secret_sha256: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>>;
    /// Live rows for one user: oldest first, id tiebreak.
    fn list_active_by_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>>;
    /// Every live row across users: owner, then age, then id (admin oversight).
    fn list_all_active<'a>(&'a self) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>>;
    /// Soft-revoke. Returns false for unknown or already-revoked ids.
    fn revoke<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>>;
    /// Stamp last use after a verification.
    fn touch<'a>(
        &'a self,
        secret_sha256: &'a str,
        last_used_at: i64,
        last_client: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// Per-user Last.fm links (v3 has no admin-global pair).
///
/// Table mapping: `user_connections` with `service = 'lastfm'`;
/// `connection_data` is `v3:` ciphertext of the JSON
/// `{api_key, shared_secret, username, session_key}` where absent fields are
/// null. Reads through this trait return the record with ciphertext fields
/// intact; only the service layer decrypts, in memory.
pub trait LastFmStore: Send + Sync {
    /// Fetch one user's link. None is absence, never failure.
    fn get<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LastFmConnection>, StoreError>>;
    /// Insert or replace one user's link.
    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        link: LastFmConnection,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    /// Delete one user's link. Returns false when none existed.
    fn delete<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>>;
}

/// Password recovery codes.
///
/// Table mapping: `auth_password_recovery_codes` (single active code per
/// user; minting replaces the prior row).
pub trait RecoveryStore: Send + Sync {
    /// Store (replacing any prior) one user's code.
    fn store<'a>(&'a self, code: RecoveryCode) -> BoxFuture<'a, Result<(), StoreError>>;
    /// Find the live row for a code hash. None when unknown or expired.
    fn find_live_by_hash<'a>(
        &'a self,
        code_hash: &'a str,
        now: i64,
    ) -> BoxFuture<'a, Result<Option<RecoveryCode>, StoreError>>;
    /// Delete one user's code. Used after a reset consumes it.
    fn delete_for_user<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// Loaded avatar bytes plus their content type.
pub type LoadedAvatar = (Vec<u8>, String);

/// Avatar image bytes.
///
/// Storage mapping: files under `<cache_dir>/avatars/{user_id}.{ext}`;
/// only one extension variant exists per user at a time.
pub trait AvatarStore: Send + Sync {
    /// Save bytes, replacing any prior avatar regardless of extension.
    /// Returns the stored extension (`jpg`, `png`, `webp`, `gif`).
    fn save<'a>(
        &'a self,
        user_id: &'a str,
        content_type: &'a str,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<String, StoreError>>;
    /// Load bytes plus content type. None when the user has no avatar.
    fn load<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedAvatar>, StoreError>>;
}

/// Password hashing comes from the federated module:
/// [`PasswordHasher`](super::super::federated::password_import::PasswordHasher)
/// (bcrypt verify, Argon2id hash, dummy verify) with scheme dispatch on
/// [`HashScheme`](super::super::federated::password_import::HashScheme).
/// The users routes hold it as `Arc<dyn FalliblePasswordHasher>` in
/// [`UsersDeps`](super::UsersDeps) and always writes the `argon2id` scheme
/// tag on new hashes. Tests use the SHA-256 test hasher in `memory.rs`,
/// never valid in production.
///
/// Every way Argon2id hashing can fail. The message goes to the log only;
/// the request fails with a fixed 500.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HashError {
    /// The OS random source failed; no salt, no hash, no fallback.
    #[error("random source unavailable")]
    RngUnavailable,
    /// Salt encoding or the Argon2id run itself failed.
    #[error("password hash failed")]
    HashFailed,
}

/// Password hashing with a fallible Argon2id entry point. The federated
/// [`PasswordHasher`] port stays infallible (its contract), so production
/// code here hashes only through [`try_hash_argon2id`](Self::try_hash_argon2id)
/// and fails the request on error. A failed hash must never persist: there
/// is no sentinel value that reads as "no hash".
pub trait FalliblePasswordHasher: PasswordHasher {
    /// Hash a password with Argon2id, or fail. No fallback value.
    fn try_hash_argon2id(&self, password: &str) -> Result<String, HashError>;
}

/// Breach-corpus screening (Have-I-Been-Pwned), fail-open on errors.
pub trait PasswordScreen: Send + Sync {
    /// True when the password appears in the corpus. Transport and file
    /// errors fail open (false), exactly like v2; only a hit returns true.
    fn screen<'a>(&'a self, password: &'a str, policy: &'a HibpPolicy) -> BoxFuture<'a, bool>;
}

/// The HIBP knobs the users routes read. The wiring bridges these to the
/// `security` config section on every password write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HibpPolicy {
    /// Whether screening runs at all.
    pub check: bool,
    /// Local HIBP file; empty means use the range API.
    pub local_path: String,
}

/// Source of the HIBP knobs. A trait so runtime config edits take effect
/// without a restart.
pub trait SecurityPolicy: Send + Sync {
    /// Current HIBP knobs.
    fn hibp(&self) -> HibpPolicy;
}

/// The Last.fm master switch. A trait so admin toggles take effect without
/// a restart. Wiring bridges to `lastfm_settings.enabled`.
pub trait LastFmSwitch: Send + Sync {
    /// Whether Last.fm fan-out is enabled.
    fn enabled(&self) -> bool;
}

/// The two Last.fm auth web calls, behind a seam so tests never touch the
/// network. No live implementation exists yet.
pub trait LastFmAuthClient: Send + Sync {
    /// `auth.getToken` with the user's own API key.
    fn request_token<'a>(
        &'a self,
        api_key: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>>;
    /// `auth.getSession` with the user's key pair and an approved token.
    fn exchange_session<'a>(
        &'a self,
        api_key: &'a str,
        shared_secret: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>>;
}

/// Last.fm auth failures. Variants carry no secret material.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum LastFmError {
    /// The token was never approved in the browser.
    #[error("token not authorized")]
    TokenNotAuthorized,
    /// Credentials or parameters rejected.
    #[error("last.fm configuration rejected")]
    Configuration,
    /// Transport or server failure upstream.
    #[error("last.fm unreachable")]
    Transport,
}

/// One account enumerated from a media-server user directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    /// Provider-side id. Must equal exactly what the live login produces
    /// (Jellyfin user id, Plex account uuid): it is the import join key.
    pub provider_uid: String,
    /// Display name on the provider.
    pub display_name: String,
    /// Account image URL, when the provider exposes one.
    pub avatar_url: Option<String>,
    /// Account email, when the provider exposes one.
    pub email: Option<String>,
}

/// User-directory failures. Both map to the federated 503 posture (fixed
/// body plus error id), matching the login flows.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DirectoryError {
    /// No live client yet, or the provider is disabled.
    #[error("directory not configured")]
    NotConfigured,
    /// Transport or server failure upstream.
    #[error("directory unreachable")]
    Transport,
}

/// Network edge for admin user import: enumerate the accounts on one media
/// server (Jellyfin `GET /Users`, Plex users). Production has no live
/// client yet, so the import serves a 503 through its disabled
/// implementations until the provider clients land (same posture as the
/// login IdPs). Tests use the scripted fakes in `memory.rs`.
pub trait UserDirectory: Send + Sync {
    /// Which provider this directory enumerates (`jellyfin` or `plex`).
    fn provider(&self) -> &'static str;
    /// List every importable account. Emails and avatars come from the
    /// provider; the import re-derives nothing client-side.
    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>>;
}

/// Clock. Unix seconds keep every store free of datetime parsing.
pub trait Clock: Send + Sync {
    /// Now, unix epoch seconds.
    fn now_unix(&self) -> i64;
}

/// System clock for production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}
