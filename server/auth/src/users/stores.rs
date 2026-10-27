//! What the users routes share: the store error and outcome types, the
//! Last.fm switch, and the seams to the world outside the process.
//!
//! The repositories themselves are concrete types in [`crate::sqlite`]
//! and [`crate::avatars`]. A trait here stays only where tests stand in for
//! an outside service ([`LastFmAuthClient`], [`UserDirectory`]). Futures
//! are boxed by hand because `async fn` is not object-safe.

use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::LastFmSettings;

/// Boxed sendable future for object-safe async ports.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Every way a store call can fail. `Internal` carries a cause for the log;
/// only `Conflict` ever shapes a user-facing message.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    /// A uniqueness or state precondition failed (username taken, id busy).
    #[error("conflicting state")]
    Conflict,
    /// The database was too busy to answer in time. Callers answer 503 with
    /// `Retry-After`; nothing is wrong with the store.
    #[error("database busy")]
    Busy,
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

/// One Last.fm API key pair, plaintext in memory only.
#[derive(Clone, PartialEq, Eq)]
pub struct LastFmKeys {
    /// API key.
    pub api_key: String,
    /// Shared secret for signed calls.
    pub shared_secret: String,
}

impl std::fmt::Debug for LastFmKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LastFmKeys(..)")
    }
}

/// The admin's Last.fm settings, read live from the `lastfm_settings`
/// section so changes apply without a restart.
#[derive(Debug, Clone)]
pub struct LastFmSwitch {
    settings: Live<LastFmSettings>,
}

impl LastFmSwitch {
    /// The switch over the live section.
    pub fn new(settings: Live<LastFmSettings>) -> Self {
        Self { settings }
    }

    /// Whether Last.fm linking and fan-out are enabled. An unreadable
    /// section reads as off: Last.fm is optional enrichment, never
    /// auth-critical, so a broken read never guesses at enabled.
    pub fn enabled(&self) -> bool {
        self.settings
            .get()
            .map(|section| section.enabled)
            .unwrap_or(false)
    }

    /// The instance key pair, when the admin saved both halves. Users
    /// without their own pair link and scrobble with it (v2 parity).
    pub fn instance_keys(&self) -> Option<LastFmKeys> {
        self.read()
            .inspect_err(|_| tracing::warn!("cannot read the Last.fm app key pair"))
            .ok()
            .and_then(|state| state.instance_keys)
    }

    /// The switch and the instance pair from one read, or
    /// [`LastFmUnreadable`] when the section cannot be read: work that must
    /// not take a broken read for "off" waits instead.
    pub fn read(&self) -> Result<LastFmState, LastFmUnreadable> {
        let section = self.settings.get().map_err(|error| {
            tracing::warn!(%error, "cannot read the Last.fm settings");
            LastFmUnreadable
        })?;
        let (api_key, shared_secret) = (section.api_key.expose(), section.shared_secret.expose());
        Ok(LastFmState {
            enabled: section.enabled,
            instance_keys: (!api_key.is_empty() && !shared_secret.is_empty()).then(|| LastFmKeys {
                api_key: api_key.to_owned(),
                shared_secret: shared_secret.to_owned(),
            }),
        })
    }
}

/// The admin's Last.fm settings as one read saw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmState {
    /// Whether Last.fm linking and fan-out are enabled.
    pub enabled: bool,
    /// The instance key pair, when the admin saved both halves.
    pub instance_keys: Option<LastFmKeys>,
}

/// The admin's Last.fm settings could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the Last.fm settings cannot be read")]
pub struct LastFmUnreadable;

/// The two Last.fm auth web calls. The live client is
/// [`super::lastfm_http::LastFmAuthHttp`]. A trait because it reaches
/// Last.fm: tests answer with a scripted fake instead of the network.
pub trait LastFmAuthClient: Send + Sync {
    /// `auth.getToken` with the user's own key pair (the call is signed).
    /// Returns the token and the browser URL that approves it.
    fn request_token<'a>(
        &'a self,
        api_key: &'a str,
        shared_secret: &'a str,
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
/// server (Jellyfin `GET /Users`, Plex Home users and friends). The live
/// directories are the federated Jellyfin and plex.tv clients; a provider
/// that is switched off or unreachable answers with a [`DirectoryError`].
/// A trait because it reaches a media server: tests answer with a
/// scripted directory instead.
pub trait UserDirectory: Send + Sync {
    /// Which provider this directory enumerates (`jellyfin` or `plex`).
    fn provider(&self) -> &'static str;
    /// List every importable account. Emails and avatars come from the
    /// provider; the import re-derives nothing client-side.
    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>>;
}
