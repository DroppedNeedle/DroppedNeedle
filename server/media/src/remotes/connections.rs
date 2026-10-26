//! Remote server connections: the admin's servers plus per-user links.
//!
//! The account rules are v2's: the server URL and enabled flag belong to
//! the admin (Settings > Jellyfin, > Navidrome, > Plex) and are read from
//! the config store on every resolution. A user may link their own account
//! on that server; the link holds only their credential and lives in
//! `user_connections` (one row per user and service, the JSON document
//! sealed whole, v2 field names kept). Resolution prefers the caller's own
//! link (`linked`) and falls back to the admin's credential (`shared`), so
//! a server the admin configured is usable by every user. A link that no
//! longer opens falls back too, as in v2. A Plex link saved without a
//! server-scoped token looks one up once and stores it (v2
//! `resolve_plex_playlist`).
//!
//! Cache scoping keys off the user id plus a hash of the connection
//! material, so one user's stale password never poisons another's entries.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::crypto::{Crypto, CryptoError};
use droppedneedle_kernel::runtime_config::secret_sections::{
    JellyfinConnection, NavidromeConnection, PlexConnection,
};
use droppedneedle_kernel::runtime_config::sections::InternalState;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};

use super::adapter::{AdapterError, BoxFuture, RemoteHandle, RemoteHttp, USER_READS_PER_SOURCE};
use super::jellyfin::JellyfinAdapter;
use super::models::SourceName;
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;

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

/// `user_connections` over the database.
#[derive(Clone)]
pub struct SqliteConnectionStore {
    db: droppedneedle_kernel::db::Db,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl SqliteConnectionStore {
    /// Bind the store to a migrated database, its rows dated by `clock`.
    pub fn new(
        db: droppedneedle_kernel::db::Db,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self { db, clock }
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
            .fetch_optional(self.db.sqlx())
            .await
            .map_err(|error| {
                droppedneedle_kernel::db::map_sqlx_busy("remotes.connection.get", error).to_string()
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
        let now = droppedneedle_auth::times::to_iso(self.clock.now_unix());
        Box::pin(async move {
            self.db
                .write(
                    droppedneedle_kernel::db::Lane::Foreground,
                    "remotes.connection.upsert",
                    move |tx| {
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
                .map_err(|error: droppedneedle_kernel::db::DbError| error.to_string())
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
            self.db
                .write(
                    droppedneedle_kernel::db::Lane::Foreground,
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
                .map_err(|error: droppedneedle_kernel::db::DbError| error.to_string())
        })
    }

    fn list<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<Vec<ConnectionRow>, String>> {
        Box::pin(async move {
            let rows: Vec<(String, i64, String)> = sqlx::query_as(
                "SELECT service, enabled, connection_data FROM user_connections \
                 WHERE user_id = ?1 ORDER BY service",
            )
            .bind(user_id)
            .fetch_all(self.db.sqlx())
            .await
            .map_err(|error| {
                droppedneedle_kernel::db::map_sqlx_busy("remotes.connection.list", error)
                    .to_string()
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

/// Delete every link whose document holds a value sealed under a data key
/// that is gone, inside the caller's transaction: one sealed whole (media
/// servers, ListenBrainz, Spotify) or plain JSON with sealed fields
/// (Last.fm). The user links the service again. Answers the rows deleted.
pub fn forget_sealed(conn: &rusqlite::Connection) -> rusqlite::Result<usize> {
    // `json_tree` fails the whole statement on text that is not JSON, so a
    // document sealed whole is walked as an empty array.
    conn.execute(
        "DELETE FROM user_connections WHERE connection_data LIKE 'v3:%' OR EXISTS (
             SELECT 1 FROM json_tree(CASE WHEN json_valid(user_connections.connection_data)
                 THEN user_connections.connection_data ELSE '[]' END) AS tree
             WHERE tree.type = 'text' AND tree.value LIKE 'v3:%')",
        [],
    )
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

/// The admin's server settings, read live on every call.
#[derive(Clone)]
pub struct ConfigServers {
    jellyfin: Live<JellyfinConnection>,
    navidrome: Live<NavidromeConnection>,
    plex: Live<PlexConnection>,
    internal: Live<InternalState>,
}

impl ConfigServers {
    /// Live handles on the server sections in `config`.
    pub fn new(config: &ConfigStore) -> Self {
        Self {
            jellyfin: config.live(),
            navidrome: config.live(),
            plex: config.live(),
            internal: config.live(),
        }
    }
}

impl ConfigServers {
    /// The server for one source. `Ok(None)` when it is disabled or has no
    /// URL; `Err` carries a log-only cause when the settings cannot be read
    /// ([`Live::read`] logs it once per revision).
    pub fn server(&self, source: SourceName) -> Result<Option<ServerSettings>, String> {
        let shared = |username: String, credential: String, user_id: String| {
            (!credential.is_empty()).then_some(SharedCredential {
                username,
                credential,
                user_id,
            })
        };
        // Install ids are never defaults: a made-up client id would sign in
        // as another device.
        let internal = || {
            self.internal
                .read()
                .ok_or_else(|| "internal settings cannot be read".to_owned())
        };
        let settings = match source {
            SourceName::Jellyfin => {
                let section = self.jellyfin.get().map_err(|error| error.to_string())?;
                let device_id = internal()?
                    .droppedneedle_device_id
                    .clone()
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
                let section = self.navidrome.get().map_err(|error| error.to_string())?;
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
                let section = self.plex.get().map_err(|error| error.to_string())?;
                let client_id = internal()?.plex_client_id.clone().unwrap_or_default();
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

impl ResolvedConnection {
    /// Values not stored anywhere yet (a settings check, a login being
    /// checked, a server probe), with no account behind them.
    pub fn unsaved(source: SourceName, base_url: String, credential: String) -> Self {
        Self {
            source,
            base_url,
            username: String::new(),
            credential,
            account_mode: String::new(),
            account_label: String::new(),
            cache_scope: String::new(),
            client_id: String::new(),
            user_id: String::new(),
            section_ids: Vec::new(),
        }
    }
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

/// Looks up the server-scoped token for a Plex account token: the
/// server's machine id, then its `accessToken` on plex.tv.
pub trait PlexServerTokens: Send + Sync {
    /// `Ok(None)` when the server or the account's access to it is not
    /// found; [`AdapterError::Auth`] when the account token is refused.
    fn server_token<'a>(
        &'a self,
        server: &'a ServerSettings,
        auth_token: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AdapterError>>;
}

/// Whose reads a client serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reads {
    /// A person waits on them: they share the source's bound of
    /// [`USER_READS_PER_SOURCE`].
    User,
    /// Background work or an audio stream: not counted.
    Background,
}

/// Resolves a caller's connection for one source, stores their links, and
/// builds the source clients.
pub struct ConnectionResolver {
    rows: Arc<dyn ConnectionStore>,
    coder: Arc<CredentialCoder>,
    servers: Arc<ConfigServers>,
    plex_tokens: Option<Arc<dyn PlexServerTokens>>,
    /// One bound per source on the reads people trigger, in
    /// [`source_slot`] order.
    user_reads: [Arc<Semaphore>; 3],
}

/// A source's place in [`ConnectionResolver::user_reads`].
fn source_slot(source: SourceName) -> usize {
    match source {
        SourceName::Jellyfin => 0,
        SourceName::Navidrome => 1,
        SourceName::Plex => 2,
    }
}

/// Build the client for `connection`, its calls held to `bound` when
/// there is one.
fn build_client(
    http: &reqwest::Client,
    connection: ResolvedConnection,
    bound: Option<Arc<Semaphore>>,
) -> RemoteHandle {
    let http = RemoteHttp::new(http.clone(), connection.source, bound);
    match connection.source {
        SourceName::Jellyfin => RemoteHandle::Jellyfin(JellyfinAdapter::over(
            http,
            connection.base_url,
            connection.credential,
            connection.user_id,
        )),
        SourceName::Navidrome => RemoteHandle::Navidrome(NavidromeAdapter::over(
            http,
            connection.base_url,
            connection.username,
            connection.credential,
        )),
        SourceName::Plex => RemoteHandle::Plex(PlexAdapter::over(
            http,
            connection.base_url,
            connection.credential,
            connection.client_id,
            connection.section_ids,
        )),
    }
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
        servers: Arc<ConfigServers>,
    ) -> Self {
        Self {
            rows,
            coder,
            servers,
            plex_tokens: None,
            user_reads: std::array::from_fn(|_| Arc::new(Semaphore::new(USER_READS_PER_SOURCE))),
        }
    }

    /// The resolver over `platform`: the link rows in its database, sealed
    /// with its key, over the admin's server settings.
    pub fn over(platform: &Platform) -> Self {
        Self::new(
            Arc::new(SqliteConnectionStore::new(
                platform.db.clone(),
                platform.clock.clone(),
            )),
            Arc::new(CredentialCoder::new(platform.crypto.clone())),
            Arc::new(ConfigServers::new(&platform.settings)),
        )
    }

    /// The client for `connection`, over `http`. This and
    /// [`Self::unsaved_client`] are the one place source clients are
    /// built. A person's reads wait for a place in the source's bound.
    pub fn client(
        &self,
        http: &reqwest::Client,
        connection: ResolvedConnection,
        reads: Reads,
    ) -> RemoteHandle {
        let bound = match reads {
            Reads::User => Some(Arc::clone(&self.user_reads[source_slot(connection.source)])),
            Reads::Background => None,
        };
        build_client(http, connection, bound)
    }

    /// The client for values not stored yet (see
    /// [`ResolvedConnection::unsaved`]), outside any bound.
    pub fn unsaved_client(http: &reqwest::Client, connection: ResolvedConnection) -> RemoteHandle {
        build_client(http, connection, None)
    }

    /// Look up and store missing Plex server tokens through `probe`.
    /// Without one, a Plex link lacking a server token uses its account
    /// token (v2 `resolve_plex`).
    pub fn with_plex_tokens(mut self, probe: Arc<dyn PlexServerTokens>) -> Self {
        self.plex_tokens = Some(probe);
        self
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
        let stale = match self.usable_link(user_id, &server, source).await {
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

    /// The caller's link, ready to use on `server`: a Plex link without a
    /// server token gets one first. Stale when it cannot be used.
    pub async fn usable_link(
        &self,
        user_id: &str,
        server: &ServerSettings,
        source: SourceName,
    ) -> Result<Option<UserLink>, ResolveError> {
        match self.link(user_id, source).await? {
            Some(link) => self.server_scoped(user_id, server, link).await.map(Some),
            None => Ok(None),
        }
    }

    /// Port of the v2 machine-id probe: look up the server token for a
    /// Plex link that has none, and store it so the lookup runs once. A
    /// refused account or no access to the server means relink; an
    /// outage keeps the account token for this call.
    async fn server_scoped(
        &self,
        user_id: &str,
        server: &ServerSettings,
        link: UserLink,
    ) -> Result<UserLink, ResolveError> {
        let Some(probe) = &self.plex_tokens else {
            return Ok(link);
        };
        let UserLink::Plex {
            auth_token,
            server_access_token,
            plex_user_id,
            username,
        } = link
        else {
            return Ok(link);
        };
        if !server_access_token.is_empty() {
            return Ok(UserLink::Plex {
                auth_token,
                server_access_token,
                plex_user_id,
                username,
            });
        }
        match probe.server_token(server, &auth_token).await {
            Ok(Some(token)) if !token.is_empty() => {
                let link = UserLink::Plex {
                    auth_token,
                    server_access_token: token,
                    plex_user_id,
                    username,
                };
                if let Err(error) = self.save_link(user_id, &link).await {
                    tracing::warn!(%error, "could not store the Plex server token");
                }
                Ok(link)
            }
            Ok(_) | Err(AdapterError::Auth) => Err(ResolveError::Stale),
            Err(error) => {
                tracing::warn!(%error, "Plex server token lookup failed; using the account token");
                Ok(UserLink::Plex {
                    auth_token,
                    server_access_token,
                    plex_user_id,
                    username,
                })
            }
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
        let plaintext =
            if data.starts_with(droppedneedle_kernel::runtime_config::crypto::CIPHER_PREFIX) {
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
/// account works for playback with no extra setup (v2 auto-link). The
/// sign-in flows decide whether a failure is fatal.
#[derive(Clone)]
pub struct SignInLinks {
    resolver: Arc<ConnectionResolver>,
}

impl SignInLinks {
    /// Write links through the shared resolver.
    pub fn new(resolver: Arc<ConnectionResolver>) -> Self {
        Self { resolver }
    }

    async fn store(&self, user_id: &str, link: UserLink) -> Result<(), String> {
        self.resolver
            .save_link(user_id, &link)
            .await
            .map_err(|error| {
                format!(
                    "could not store the {} link: {error}",
                    link.source().as_str()
                )
            })
    }
}

impl droppedneedle_auth::federated::MediaLinkStore for SignInLinks {
    fn link_plex<'a>(
        &'a self,
        user_id: &'a str,
        profile: &'a droppedneedle_auth::federated::plex::PlexProfile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(self.store(
            user_id,
            UserLink::Plex {
                auth_token: profile.auth_token.clone(),
                server_access_token: profile.server_access_token.clone(),
                plex_user_id: profile.uuid.clone(),
                username: profile.display_name.clone(),
            },
        ))
    }

    fn link_jellyfin<'a>(
        &'a self,
        user_id: &'a str,
        profile: &'a droppedneedle_auth::federated::jellyfin_login::JellyfinProfile,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(self.store(
            user_id,
            UserLink::Jellyfin {
                access_token: profile.access_token.clone(),
                jellyfin_user_id: profile.jellyfin_user_id.clone(),
                username: profile.username.clone(),
            },
        ))
    }
}
