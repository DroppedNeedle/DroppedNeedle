//! Remote server connections: the admin's servers plus per-user links.
//!
//! The account rules are v2's: the server URL and enabled flag belong to
//! the admin (Settings > Jellyfin, > Navidrome, > Plex) and are read from
//! the config store on every resolution. A user may link their own account
//! on that server; the link holds only their credential and lives in
//! `user_connections` (one row per user and service, the JSON document
//! sealed whole, v2 field names kept). Resolution prefers the caller's own
//! link (`linked`) and falls back to the admin's credential (`shared`), so
//! a server the admin configured is usable by every user. Playback
//! attribution uses the caller's own link only and fails closed.
//!
//! Cache scoping keys off the user id plus a hash of the connection
//! material, so one user's stale password never poisons another's entries.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime_config::ConfigStore;
use crate::runtime_config::crypto::{Crypto, CryptoError};
use crate::runtime_config::secret_sections::{
    JellyfinConnection, NavidromeConnection, PlexConnection,
};
use crate::runtime_config::sections::InternalState;

use super::adapter::BoxFuture;
use super::models::SourceName;

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One `user_connections` row. `data` is whatever the owning feature
/// stored: sealed JSON for media servers and ListenBrainz, plain JSON with
/// sealed fields for Last.fm.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionRow {
    /// Service tag (`navidrome`, `jellyfin`, `plex`, `listenbrainz`, ...).
    pub service: String,
    /// Whether the row is enabled.
    pub enabled: bool,
    /// Stored document. Never logged.
    pub data: String,
}

impl std::fmt::Debug for ConnectionRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionRow")
            .field("service", &self.service)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// Persistence for `user_connections`. Errors carry a log-only cause.
pub trait ConnectionStore: Send + Sync {
    /// One user's row for one service. None is absence.
    fn get<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConnectionRow>, String>>;

    /// Insert or replace one user's row for one service (enabled).
    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
        data: String,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Delete one user's row. False when there was none.
    fn delete<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>>;

    /// Every row for one user, ordered by service.
    fn list<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<Vec<ConnectionRow>, String>>;
}

/// `user_connections` over the shared pool (reads) and the writer lane
/// (writes).
#[derive(Clone)]
pub struct SqliteConnectionStore {
    pool: sqlx::SqlitePool,
    lane: crate::db::WriteLane,
}

impl SqliteConnectionStore {
    /// Bind the store to a migrated database.
    pub fn new(pool: sqlx::SqlitePool, lane: crate::db::WriteLane) -> Self {
        Self { pool, lane }
    }
}

impl ConnectionStore for SqliteConnectionStore {
    fn get<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConnectionRow>, String>> {
        Box::pin(async move {
            let row: Option<(String, i64, String)> = sqlx::query_as(
                "SELECT service, enabled, connection_data FROM user_connections \
                 WHERE user_id = ?1 AND service = ?2",
            )
            .bind(user_id)
            .bind(service)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                crate::db::map_sqlx_busy("remotes.connection.get", error).to_string()
            })?;
            Ok(row.map(|(service, enabled, data)| ConnectionRow {
                service,
                enabled: enabled != 0,
                data,
            }))
        })
    }

    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
        data: String,
    ) -> BoxFuture<'a, Result<(), String>> {
        let user_id = user_id.to_owned();
        let service = service.to_owned();
        Box::pin(async move {
            self.lane
                .write(
                    crate::db::Lane::Foreground,
                    "remotes.connection.upsert",
                    move |tx| {
                        let now = crate::auth::times::to_iso(unix_now());
                        tx.execute(
                            "INSERT INTO user_connections \
                             (user_id, service, connection_data, enabled, created_at, updated_at) \
                             VALUES (?1, ?2, ?3, 1, ?4, ?4) \
                             ON CONFLICT (user_id, service) DO UPDATE SET \
                             connection_data = excluded.connection_data, enabled = 1, \
                             updated_at = excluded.updated_at",
                            rusqlite::params![user_id, service, data, now],
                        )?;
                        Ok(())
                    },
                )
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn delete<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let user_id = user_id.to_owned();
        let service = service.to_owned();
        Box::pin(async move {
            self.lane
                .write(
                    crate::db::Lane::Foreground,
                    "remotes.connection.delete",
                    move |tx| {
                        Ok(tx.execute(
                            "DELETE FROM user_connections WHERE user_id = ?1 AND service = ?2",
                            rusqlite::params![user_id, service],
                        )?)
                    },
                )
                .await
                .map(|changed| changed > 0)
                .map_err(|error| error.to_string())
        })
    }

    fn list<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<Vec<ConnectionRow>, String>> {
        Box::pin(async move {
            let rows: Vec<(String, i64, String)> = sqlx::query_as(
                "SELECT service, enabled, connection_data FROM user_connections \
                 WHERE user_id = ?1 ORDER BY service",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                crate::db::map_sqlx_busy("remotes.connection.list", error).to_string()
            })?;
            Ok(rows
                .into_iter()
                .map(|(service, enabled, data)| ConnectionRow {
                    service,
                    enabled: enabled != 0,
                    data,
                })
                .collect())
        })
    }
}

/// In-memory rows for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryConnectionStore {
    inner: std::sync::Mutex<std::collections::BTreeMap<(String, String), ConnectionRow>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryConnectionStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn guard(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<(String, String), ConnectionRow>>
    {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ConnectionStore for MemoryConnectionStore {
    fn get<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConnectionRow>, String>> {
        let row = self
            .guard()
            .get(&(user_id.to_owned(), service.to_owned()))
            .cloned();
        Box::pin(async move { Ok(row) })
    }

    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
        data: String,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.guard().insert(
            (user_id.to_owned(), service.to_owned()),
            ConnectionRow {
                service: service.to_owned(),
                enabled: true,
                data,
            },
        );
        Box::pin(async { Ok(()) })
    }

    fn delete<'a>(
        &'a self,
        user_id: &'a str,
        service: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        let removed = self
            .guard()
            .remove(&(user_id.to_owned(), service.to_owned()))
            .is_some();
        Box::pin(async move { Ok(removed) })
    }

    fn list<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<Vec<ConnectionRow>, String>> {
        let rows = self
            .guard()
            .iter()
            .filter(|((owner, _), _)| owner == user_id)
            .map(|(_, row)| row.clone())
            .collect();
        Box::pin(async move { Ok(rows) })
    }
}

// ---------------------------------------------------------------------------
// Sealing
// ---------------------------------------------------------------------------

/// Seals and opens connection documents under the config key.
#[derive(Debug)]
pub struct CredentialCoder {
    crypto: Arc<Crypto>,
}

impl CredentialCoder {
    /// Wrap the shared config key.
    pub fn new(crypto: Arc<Crypto>) -> Self {
        Self { crypto }
    }

    /// Seal one plaintext document for storage.
    pub fn seal(&self, plaintext: &str) -> Result<String, CryptoError> {
        self.crypto.encrypt(plaintext)
    }

    /// Open one stored ciphertext. Fails closed on any undecryptable input.
    pub fn open(&self, sealed: &str) -> Result<String, CryptoError> {
        self.crypto.decrypt(sealed)
    }
}

// ---------------------------------------------------------------------------
// Per-user links
// ---------------------------------------------------------------------------

/// One user's own account on the admin's server, as stored.
#[derive(Clone, PartialEq, Eq)]
pub enum UserLink {
    /// Navidrome login. Subsonic token auth needs the raw password on every
    /// request, so it is kept (sealed).
    Navidrome {
        /// Navidrome username.
        username: String,
        /// Navidrome password.
        password: String,
    },
    /// Jellyfin user session from `AuthenticateByName`. The password is
    /// never kept.
    Jellyfin {
        /// User-scoped access token.
        access_token: String,
        /// Jellyfin-side user id.
        jellyfin_user_id: String,
        /// Jellyfin display name.
        username: String,
    },
    /// Plex account from the PIN link flow.
    Plex {
        /// Account token.
        auth_token: String,
        /// Server-scoped token, when resolved.
        server_access_token: String,
        /// Plex account uuid.
        plex_user_id: String,
        /// Plex display name.
        username: String,
    },
}

impl std::fmt::Debug for UserLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserLink")
            .field("source", &self.source())
            .field("username", &self.username())
            .finish_non_exhaustive()
    }
}

/// The stored document, v2 field names. Every field is optional here; the
/// per-source conversion decides which ones a usable link needs.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct LinkDocument {
    #[serde(skip_serializing_if = "String::is_empty")]
    username: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    password: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    access_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    jellyfin_user_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    auth_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    server_access_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    plex_user_id: String,
}

impl UserLink {
    /// Which server this link is for.
    pub fn source(&self) -> SourceName {
        match self {
            Self::Navidrome { .. } => SourceName::Navidrome,
            Self::Jellyfin { .. } => SourceName::Jellyfin,
            Self::Plex { .. } => SourceName::Plex,
        }
    }

    /// Account label shown to the user.
    pub fn username(&self) -> &str {
        match self {
            Self::Navidrome { username, .. }
            | Self::Jellyfin { username, .. }
            | Self::Plex { username, .. } => username,
        }
    }

    fn to_document(&self) -> LinkDocument {
        match self.clone() {
            Self::Navidrome { username, password } => LinkDocument {
                username,
                password,
                ..LinkDocument::default()
            },
            Self::Jellyfin {
                access_token,
                jellyfin_user_id,
                username,
            } => LinkDocument {
                username,
                access_token,
                jellyfin_user_id,
                ..LinkDocument::default()
            },
            Self::Plex {
                auth_token,
                server_access_token,
                plex_user_id,
                username,
            } => LinkDocument {
                username,
                auth_token,
                server_access_token,
                plex_user_id,
                ..LinkDocument::default()
            },
        }
    }

    /// Rebuild a link from its document. None when a field the source
    /// needs is missing (the user relinks).
    fn from_document(source: SourceName, doc: LinkDocument) -> Option<Self> {
        match source {
            SourceName::Navidrome => (!doc.username.is_empty() && !doc.password.is_empty())
                .then_some(Self::Navidrome {
                    username: doc.username,
                    password: doc.password,
                }),
            SourceName::Jellyfin => (!doc.access_token.is_empty()
                && !doc.jellyfin_user_id.is_empty())
            .then_some(Self::Jellyfin {
                access_token: doc.access_token,
                jellyfin_user_id: doc.jellyfin_user_id,
                username: doc.username,
            }),
            SourceName::Plex => (!doc.auth_token.is_empty()).then_some(Self::Plex {
                auth_token: doc.auth_token,
                server_access_token: doc.server_access_token,
                plex_user_id: doc.plex_user_id,
                username: doc.username,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Admin servers
// ---------------------------------------------------------------------------

/// The admin's credential for one server, used in shared mode and by the
/// presence poller.
#[derive(Clone, PartialEq, Eq)]
pub struct SharedCredential {
    /// Login name (Navidrome), empty otherwise.
    pub username: String,
    /// Password (Navidrome), API key (Jellyfin), or token (Plex).
    pub credential: String,
    /// Jellyfin user id the admin picked, empty otherwise.
    pub user_id: String,
}

impl std::fmt::Debug for SharedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedCredential")
            .field("username", &self.username)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// One admin-configured server that is enabled and has a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    /// Server base URL without a trailing slash.
    pub base_url: String,
    /// The admin's own credential, when one is saved.
    pub shared: Option<SharedCredential>,
    /// Install-wide client identifier the server sees: the Plex client id,
    /// or the Jellyfin device id.
    pub client_id: String,
    /// Pinned Plex music sections.
    pub section_ids: Vec<String>,
}

/// Admin server settings, read fresh on every call.
pub trait ServerConfig: Send + Sync {
    /// The server for one source. `Ok(None)` when it is disabled or has no
    /// URL; `Err` carries a log-only cause when the settings cannot be read.
    fn server(&self, source: SourceName) -> Result<Option<ServerSettings>, String>;
}

/// Server settings from the config store.
#[derive(Clone)]
pub struct ConfigServers {
    store: Arc<ConfigStore>,
}

impl ConfigServers {
    /// Read servers from the shared config store.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

impl ServerConfig for ConfigServers {
    fn server(&self, source: SourceName) -> Result<Option<ServerSettings>, String> {
        let shared = |username: String, credential: String, user_id: String| {
            (!credential.is_empty()).then_some(SharedCredential {
                username,
                credential,
                user_id,
            })
        };
        let internal = || self.store.get::<InternalState>().unwrap_or_default();
        let settings = match source {
            SourceName::Jellyfin => {
                let section = self
                    .store
                    .get_raw::<JellyfinConnection>()
                    .map_err(|error| error.to_string())?;
                let device_id = internal()
                    .droppedneedle_device_id
                    .unwrap_or_else(|| "droppedneedle".to_owned());
                (section.enabled && !section.jellyfin_url.is_empty()).then(|| ServerSettings {
                    base_url: section.jellyfin_url.trim_end_matches('/').to_owned(),
                    shared: shared(
                        String::new(),
                        section.api_key.expose().to_owned(),
                        section.user_id.clone(),
                    ),
                    client_id: device_id,
                    section_ids: Vec::new(),
                })
            }
            SourceName::Navidrome => {
                let section = self
                    .store
                    .get_raw::<NavidromeConnection>()
                    .map_err(|error| error.to_string())?;
                (section.enabled && !section.navidrome_url.is_empty()).then(|| ServerSettings {
                    base_url: section.navidrome_url.trim_end_matches('/').to_owned(),
                    shared: if section.username.is_empty() {
                        None
                    } else {
                        shared(
                            section.username.clone(),
                            section.password.expose().to_owned(),
                            String::new(),
                        )
                    },
                    client_id: String::new(),
                    section_ids: Vec::new(),
                })
            }
            SourceName::Plex => {
                let section = self
                    .store
                    .get_raw::<PlexConnection>()
                    .map_err(|error| error.to_string())?;
                let client_id = internal().plex_client_id.unwrap_or_default();
                (section.enabled && !section.plex_url.is_empty()).then(|| ServerSettings {
                    base_url: section.plex_url.trim_end_matches('/').to_owned(),
                    shared: shared(
                        String::new(),
                        section.plex_token.expose().to_owned(),
                        String::new(),
                    ),
                    client_id,
                    section_ids: section.music_library_ids.clone(),
                })
            }
        };
        Ok(settings)
    }
}

/// No servers configured: every source reads as not configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoServers;

impl ServerConfig for NoServers {
    fn server(&self, _source: SourceName) -> Result<Option<ServerSettings>, String> {
        Ok(None)
    }
}

/// Fixed servers for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct FixedServers {
    servers: std::collections::HashMap<&'static str, ServerSettings>,
}

#[cfg(any(test, feature = "test-support"))]
impl FixedServers {
    /// Add one server.
    pub fn with(mut self, source: SourceName, settings: ServerSettings) -> Self {
        self.servers.insert(source.as_str(), settings);
        self
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ServerConfig for FixedServers {
    fn server(&self, source: SourceName) -> Result<Option<ServerSettings>, String> {
        Ok(self.servers.get(source.as_str()).cloned())
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Decrypted connection material, ready for adapter construction. Short
/// lived: build the adapter and drop it.
pub struct ResolvedConnection {
    /// Owning source.
    pub source: SourceName,
    /// Server base URL.
    pub base_url: String,
    /// Login name (Navidrome).
    pub username: String,
    /// Plaintext credential. Never logged; the type has no `Display`.
    pub credential: String,
    /// "linked" (own account) or "shared" (admin account).
    pub account_mode: String,
    /// Display label, e.g. the login name.
    pub account_label: String,
    /// Cache scope segment isolating this owner's entries.
    pub cache_scope: String,
    /// Plex client identifier.
    pub client_id: String,
    /// Jellyfin user id.
    pub user_id: String,
    /// Pinned Plex music section ids.
    pub section_ids: Vec<String>,
}

impl std::fmt::Debug for ResolvedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedConnection")
            .field("source", &self.source)
            .field("base_url", &self.base_url)
            .field("account_mode", &self.account_mode)
            .finish_non_exhaustive()
    }
}

/// Every way connection resolution can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The admin has not configured this server, or nobody has a usable
    /// credential for it.
    NotConfigured,
    /// The caller's stored link no longer opens or is incomplete. Relink.
    Stale,
    /// The settings or the connection rows could not be read.
    Store(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("remote source is not configured"),
            Self::Stale => f.write_str("stored remote credential no longer opens"),
            Self::Store(cause) => write!(f, "connection store failed: {cause}"),
        }
    }
}

/// Every way saving or removing a link can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The secrets core refused to seal.
    SealFailed,
    /// The row could not be written.
    Store(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SealFailed => f.write_str("could not seal the credential"),
            Self::Store(cause) => write!(f, "connection store failed: {cause}"),
        }
    }
}

/// One row of the caller's linked accounts (any service). Never carries
/// credential material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSummary {
    /// Service tag.
    pub service: String,
    /// Whether the row is enabled.
    pub enabled: bool,
    /// Linked account name, when the stored document names one.
    pub username: String,
}

/// Resolves a caller's connection for one source, and stores their links.
pub struct ConnectionResolver {
    rows: Arc<dyn ConnectionStore>,
    coder: Arc<CredentialCoder>,
    servers: Arc<dyn ServerConfig>,
}

impl std::fmt::Debug for ConnectionResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionResolver").finish_non_exhaustive()
    }
}

impl ConnectionResolver {
    /// Build a resolver over the link rows, the sealing key, and the admin
    /// server settings.
    pub fn new(
        rows: Arc<dyn ConnectionStore>,
        coder: Arc<CredentialCoder>,
        servers: Arc<dyn ServerConfig>,
    ) -> Self {
        Self {
            rows,
            coder,
            servers,
        }
    }

    /// The admin's server for one source.
    pub fn server(&self, source: SourceName) -> Result<Option<ServerSettings>, ResolveError> {
        self.servers.server(source).map_err(ResolveError::Store)
    }

    /// The caller's own link first, then the admin's shared credential.
    /// A stored link that no longer opens or is incomplete falls back to
    /// the shared account, as v2 `PerUserClientFactory` did; it reads as
    /// stale only when there is no shared account to fall back to.
    pub async fn resolve(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> Result<ResolvedConnection, ResolveError> {
        let server = self.server(source)?.ok_or(ResolveError::NotConfigured)?;
        let stale = match self.link(user_id, source).await {
            Ok(Some(link)) => return Ok(linked(user_id, &server, link)),
            Ok(None) => false,
            Err(ResolveError::Stale) => {
                tracing::warn!(
                    source = source.as_str(),
                    "stored link no longer opens; using the shared account"
                );
                true
            }
            Err(other) => return Err(other),
        };
        match shared(user_id, source, &server) {
            Err(ResolveError::NotConfigured) if stale => Err(ResolveError::Stale),
            other => other,
        }
    }

    /// The admin's credential only (presence polling, shared browse).
    pub fn resolve_shared(&self, source: SourceName) -> Result<ResolvedConnection, ResolveError> {
        let server = self.server(source)?.ok_or(ResolveError::NotConfigured)?;
        shared("shared", source, &server)
    }

    /// The caller's stored link. A row that no longer opens, or opens to
    /// an incomplete document, reads as stale.
    pub async fn link(
        &self,
        user_id: &str,
        source: SourceName,
    ) -> Result<Option<UserLink>, ResolveError> {
        let Some(row) = self
            .rows
            .get(user_id, source.as_str())
            .await
            .map_err(ResolveError::Store)?
        else {
            return Ok(None);
        };
        if !row.enabled {
            return Ok(None);
        }
        let plaintext = self
            .coder
            .open(&row.data)
            .map_err(|_| ResolveError::Stale)?;
        let document: LinkDocument =
            serde_json::from_str(&plaintext).map_err(|_| ResolveError::Stale)?;
        UserLink::from_document(source, document)
            .map(Some)
            .ok_or(ResolveError::Stale)
    }

    /// Seal and store one user's link, replacing any earlier one.
    pub async fn save_link(&self, user_id: &str, link: &UserLink) -> Result<(), SaveError> {
        let document = serde_json::to_string(&link.to_document())
            .map_err(|error| SaveError::Store(error.to_string()))?;
        let sealed = self
            .coder
            .seal(&document)
            .map_err(|_| SaveError::SealFailed)?;
        self.rows
            .upsert(user_id, link.source().as_str(), sealed)
            .await
            .map_err(SaveError::Store)
    }

    /// Remove one user's link. False when there was none.
    pub async fn delete_link(&self, user_id: &str, service: &str) -> Result<bool, SaveError> {
        self.rows
            .delete(user_id, service)
            .await
            .map_err(SaveError::Store)
    }

    /// Every linked account the user has, across services. Documents that
    /// do not open still list (with no name) so the user can unlink them.
    pub async fn list_links(&self, user_id: &str) -> Result<Vec<LinkSummary>, ResolveError> {
        let rows = self.rows.list(user_id).await.map_err(ResolveError::Store)?;
        Ok(rows
            .into_iter()
            .map(|row| LinkSummary {
                username: self.document_username(&row.data),
                service: row.service,
                enabled: row.enabled,
            })
            .collect())
    }

    /// The `username` field of a stored document, sealed whole or plain.
    fn document_username(&self, data: &str) -> String {
        let plaintext = if data.starts_with(crate::runtime_config::crypto::CIPHER_PREFIX) {
            match self.coder.open(data) {
                Ok(plaintext) => plaintext,
                Err(_) => return String::new(),
            }
        } else {
            data.to_owned()
        };
        #[derive(Deserialize)]
        struct Named {
            #[serde(default)]
            username: Option<String>,
        }
        serde_json::from_str::<Named>(&plaintext)
            .ok()
            .and_then(|named| named.username)
            .unwrap_or_default()
    }
}

fn linked(user_id: &str, server: &ServerSettings, link: UserLink) -> ResolvedConnection {
    let label = if link.username().is_empty() {
        link.source().display().to_owned()
    } else {
        link.username().to_owned()
    };
    let source = link.source();
    let (username, credential, remote_user) = match link {
        UserLink::Navidrome { username, password } => (username, password, String::new()),
        UserLink::Jellyfin {
            access_token,
            jellyfin_user_id,
            ..
        } => (String::new(), access_token, jellyfin_user_id),
        UserLink::Plex {
            auth_token,
            server_access_token,
            ..
        } => {
            let token = if server_access_token.is_empty() {
                auth_token
            } else {
                server_access_token
            };
            (String::new(), token, String::new())
        }
    };
    ResolvedConnection {
        source,
        cache_scope: cache_scope(user_id, source, &server.base_url, &credential),
        base_url: server.base_url.clone(),
        username,
        credential,
        account_mode: "linked".to_owned(),
        account_label: label,
        client_id: server.client_id.clone(),
        user_id: remote_user,
        section_ids: server.section_ids.clone(),
    }
}

fn shared(
    user_id: &str,
    source: SourceName,
    server: &ServerSettings,
) -> Result<ResolvedConnection, ResolveError> {
    let credential = server.shared.clone().ok_or(ResolveError::NotConfigured)?;
    Ok(ResolvedConnection {
        source,
        cache_scope: cache_scope(user_id, source, &server.base_url, &credential.credential),
        base_url: server.base_url.clone(),
        account_label: format!("Shared {} account", source.display()),
        username: credential.username,
        credential: credential.credential,
        account_mode: "shared".to_owned(),
        client_id: server.client_id.clone(),
        user_id: credential.user_id,
        section_ids: server.section_ids.clone(),
    })
}

/// Per-owner cache scope: `user:{id}:{generation}` where the generation
/// hashes the connection material, so a credential change retires the old
/// scope instead of serving another account's entries.
fn cache_scope(user_id: &str, source: SourceName, base_url: &str, credential: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(base_url.as_bytes());
    hasher.update([0]);
    hasher.update(credential.as_bytes());
    let digest = hasher.finalize();
    let generation: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("user:{user_id}:{generation}")
}

/// Stores the media link a Plex or Jellyfin sign-in hands back, so the
/// account works for playback with no extra setup (v2 auto-link). Failures
/// log and never fail the sign-in.
#[derive(Clone)]
pub struct SignInLinks {
    resolver: Arc<ConnectionResolver>,
}

impl SignInLinks {
    /// Write links through the shared resolver.
    pub fn new(resolver: Arc<ConnectionResolver>) -> Self {
        Self { resolver }
    }

    async fn store(&self, user_id: &str, link: UserLink) {
        if let Err(error) = self.resolver.save_link(user_id, &link).await {
            tracing::warn!(
                source = link.source().as_str(),
                %error,
                "could not store the signed-in media link; the user can link it by hand"
            );
        }
    }
}

impl crate::auth::federated::plex::PlexConnectionLink for SignInLinks {
    async fn link(&self, user_id: &str, profile: &crate::auth::federated::plex::PlexProfile) {
        self.store(
            user_id,
            UserLink::Plex {
                auth_token: profile.auth_token.clone(),
                server_access_token: profile.server_access_token.clone(),
                plex_user_id: profile.uuid.clone(),
                username: profile.display_name.clone(),
            },
        )
        .await;
    }
}

impl crate::auth::federated::jellyfin_login::JellyfinConnectionLink for SignInLinks {
    async fn link(
        &self,
        user_id: &str,
        profile: &crate::auth::federated::jellyfin_login::JellyfinProfile,
    ) {
        self.store(
            user_id,
            UserLink::Jellyfin {
                access_token: profile.access_token.clone(),
                jellyfin_user_id: profile.jellyfin_user_id.clone(),
                username: profile.username.clone(),
            },
        )
        .await;
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver(servers: FixedServers) -> (ConnectionResolver, Arc<MemoryConnectionStore>) {
        let rows = Arc::new(MemoryConnectionStore::new());
        let crypto = Crypto::from_key_bytes(&[3u8; 32]).expect("test key");
        let resolver = ConnectionResolver::new(
            rows.clone(),
            Arc::new(CredentialCoder::new(Arc::new(crypto))),
            Arc::new(servers),
        );
        (resolver, rows)
    }

    fn navidrome_server(shared: bool) -> ServerSettings {
        ServerSettings {
            base_url: "http://navidrome.test".to_owned(),
            shared: shared.then(|| SharedCredential {
                username: "admin".to_owned(),
                credential: "admin-pass".to_owned(),
                user_id: String::new(),
            }),
            client_id: String::new(),
            section_ids: Vec::new(),
        }
    }

    /// A user with no link of their own uses the admin's server account;
    /// once they link, their own account wins, sealed at rest.
    #[tokio::test]
    async fn own_link_wins_over_the_shared_admin_account() {
        let (resolver, rows) =
            resolver(FixedServers::default().with(SourceName::Navidrome, navidrome_server(true)));
        let shared = resolver
            .resolve("ada", SourceName::Navidrome)
            .await
            .expect("shared resolves");
        assert_eq!(shared.account_mode, "shared");
        assert_eq!(shared.credential, "admin-pass");

        resolver
            .save_link(
                "ada",
                &UserLink::Navidrome {
                    username: "ada".to_owned(),
                    password: "s3cret".to_owned(),
                },
            )
            .await
            .expect("link saves");
        let row = rows
            .get("ada", "navidrome")
            .await
            .expect("row reads")
            .expect("row exists");
        assert!(row.data.starts_with("v3:"), "sealed at rest");
        assert!(!row.data.contains("s3cret"));

        let own = resolver
            .resolve("ada", SourceName::Navidrome)
            .await
            .expect("own resolves");
        assert_eq!(own.account_mode, "linked");
        assert_eq!(own.credential, "s3cret");

        // A link that no longer opens falls back to the shared account.
        rows.upsert("bea", "navidrome", "v3:garbage".to_owned())
            .await
            .expect("row writes");
        let fallback = resolver
            .resolve("bea", SourceName::Navidrome)
            .await
            .expect("stale link falls back");
        assert_eq!(fallback.account_mode, "shared");
    }

    /// A disabled or missing admin server hides every link: the URL is the
    /// admin's, so nothing resolves without it.
    #[tokio::test]
    async fn links_need_the_admin_server() {
        let (resolver, _) = resolver(FixedServers::default());
        resolver
            .save_link(
                "ada",
                &UserLink::Navidrome {
                    username: "ada".to_owned(),
                    password: "s3cret".to_owned(),
                },
            )
            .await
            .expect("link saves");
        assert!(matches!(
            resolver.resolve("ada", SourceName::Navidrome).await,
            Err(ResolveError::NotConfigured)
        ));
    }
}
