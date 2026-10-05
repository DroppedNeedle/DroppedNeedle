//! Per-user remote connections with encrypted credentials.
//!
//! Each row pins one user to one remote source: the server URL plus the
//! credential, sealed at rest with the stage-2 secrets core
//! ([`Crypto`](crate::runtime_config::crypto::Crypto)). Reads
//! decrypt on the way out; status views never carry credential material.
//!
//! Account modes follow the v2 factory rule: the server URL and enabled
//! flag are admin-owned, only the credential is per-user. Resolution
//! prefers the caller's own row (`linked`) and falls back to the shared
//! admin row (`shared`); anything else reads as not configured. Cache
//! scoping keys off the user id plus a hash of the connection material so
//! one user's stale password never poisons another's entries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::runtime_config::crypto::{Crypto, CryptoError};
use sha2::{Digest, Sha256};

use super::adapter::BoxFuture;
use super::models::SourceName;

/// Owner id of the shared admin rows that back `shared`-mode resolution.
pub const SHARED_OWNER: &str = "shared-admin";

/// One stored connection. Credential fields hold `v3:` ciphertext, never
/// plaintext; the `Debug` impl redacts them anyway.
#[derive(Clone)]
pub struct StoredConnection {
    /// Owning user id, or [`SHARED_OWNER`] for the admin row.
    pub owner_id: String,
    /// Remote source.
    pub source: SourceName,
    /// Server base URL (admin-owned).
    pub base_url: String,
    /// Login name or key label. Empty when the source needs none.
    pub username: String,
    /// Sealed primary credential (password, API key, or token).
    pub sealed_credential: String,
    /// Whether the source is enabled for this owner.
    pub enabled: bool,
    /// Plex client identifier, Plex only.
    pub client_id: String,
    /// Jellyfin user id hint, Jellyfin only.
    pub user_id: String,
    /// Pinned Plex music section ids, Plex only.
    pub section_ids: Vec<String>,
}

impl std::fmt::Debug for StoredConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredConnection")
            .field("owner_id", &self.owner_id)
            .field("source", &self.source)
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// Decrypted connection material, ready for adapter construction. Short
/// lived: build the adapter and drop it.
pub struct ResolvedConnection {
    /// Owning source.
    pub source: SourceName,
    /// Server base URL.
    pub base_url: String,
    /// Login name or key label.
    pub username: String,
    /// Plaintext credential. Never logged; the type has no `Display`.
    pub credential: String,
    /// "linked" (own row) or "shared" (admin row).
    pub account_mode: String,
    /// Display label, e.g. the login name.
    pub account_label: String,
    /// Cache scope segment isolating this owner's entries.
    pub cache_scope: String,
    /// Plex client identifier.
    pub client_id: String,
    /// Jellyfin user id hint.
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

/// Connection persistence port. The slice ships a memory store; SQLite
/// arrives with the integrator's persistence tier.
pub trait ConnectionStore: Send + Sync {
    /// Fetch one owner's row for one source. None is absence.
    fn get<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
    ) -> BoxFuture<'a, Option<StoredConnection>>;

    /// Insert or replace one row.
    fn put<'a>(&'a self, connection: StoredConnection) -> BoxFuture<'a, ()>;

    /// Delete one owner's row for one source. Missing rows are a no-op.
    fn delete<'a>(&'a self, owner_id: &'a str, source: SourceName) -> BoxFuture<'a, ()>;

    /// Every row for one owner (status views).
    fn list_owner<'a>(&'a self, owner_id: &'a str) -> BoxFuture<'a, Vec<StoredConnection>>;

    /// Every row in the store (re-encryption walks this).
    fn list_all<'a>(&'a self) -> BoxFuture<'a, Vec<StoredConnection>>;
}

/// In-memory connection store.
#[derive(Debug, Default)]
pub struct MemoryConnectionStore {
    inner: Mutex<HashMap<(String, u8), StoredConnection>>,
}

impl MemoryConnectionStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn key(owner_id: &str, source: SourceName) -> (String, u8) {
        let discriminant = match source {
            SourceName::Jellyfin => 0,
            SourceName::Navidrome => 1,
            SourceName::Plex => 2,
        };
        (owner_id.to_owned(), discriminant)
    }
}

impl ConnectionStore for MemoryConnectionStore {
    fn get<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
    ) -> BoxFuture<'a, Option<StoredConnection>> {
        Box::pin(async move {
            self.inner
                .lock()
                .map(|guard| guard.get(&Self::key(owner_id, source)).cloned())
                .unwrap_or(None)
        })
    }

    fn put<'a>(&'a self, connection: StoredConnection) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Ok(mut guard) = self.inner.lock() {
                guard.insert(
                    Self::key(&connection.owner_id, connection.source),
                    connection,
                );
            }
        })
    }

    fn delete<'a>(&'a self, owner_id: &'a str, source: SourceName) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Ok(mut guard) = self.inner.lock() {
                guard.remove(&Self::key(owner_id, source));
            }
        })
    }

    fn list_owner<'a>(&'a self, owner_id: &'a str) -> BoxFuture<'a, Vec<StoredConnection>> {
        Box::pin(async move {
            self.inner
                .lock()
                .map(|guard| {
                    guard
                        .values()
                        .filter(|row| row.owner_id == owner_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn list_all<'a>(&'a self) -> BoxFuture<'a, Vec<StoredConnection>> {
        Box::pin(async move {
            self.inner
                .lock()
                .map(|guard| guard.values().cloned().collect())
                .unwrap_or_default()
        })
    }
}

/// Seals and opens connection credentials under one stage-2 key.
#[derive(Debug)]
pub struct CredentialCoder {
    crypto: Arc<Crypto>,
}

impl CredentialCoder {
    /// Wrap the shared stage-2 key.
    pub fn new(crypto: Arc<Crypto>) -> Self {
        Self { crypto }
    }

    /// Seal one plaintext credential for storage.
    pub fn seal(&self, plaintext: &str) -> Result<String, CryptoError> {
        self.crypto.encrypt(plaintext)
    }

    /// Open one stored ciphertext. Fails closed on any undecryptable input.
    pub fn open(&self, sealed: &str) -> Result<String, CryptoError> {
        self.crypto.decrypt(sealed)
    }
}

/// Fields for [`save_connection`]. `None` fields on an existing row keep
/// their stored values; a missing row needs a base URL and a credential.
/// The `Debug` impl redacts the plaintext credential.
#[derive(Clone, Default)]
pub struct ConnectionDraft {
    /// Server base URL.
    pub base_url: Option<String>,
    /// Login name or key label.
    pub username: Option<String>,
    /// Plaintext password, API key, or token. Sealed on save.
    pub credential: Option<String>,
    /// Plex client identifier.
    pub client_id: Option<String>,
    /// Jellyfin user id hint.
    pub user_id: Option<String>,
    /// Plex music section id to pin.
    pub section_id: Option<String>,
}

impl std::fmt::Debug for ConnectionDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionDraft")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("client_id", &self.client_id)
            .field("user_id", &self.user_id)
            .field("section_id", &self.section_id)
            .finish_non_exhaustive()
    }
}

/// Store one owner's connection, sealing the credential.
pub async fn save_connection(
    store: &dyn ConnectionStore,
    coder: &CredentialCoder,
    owner_id: &str,
    source: SourceName,
    draft: ConnectionDraft,
) -> Result<StoredConnection, SaveError> {
    let existing = store.get(owner_id, source).await;
    let sealed = match (draft.credential, existing.as_ref()) {
        (Some(secret), _) if !secret.is_empty() => {
            coder.seal(&secret).map_err(|_| SaveError::SealFailed)?
        }
        (Some(_), Some(row)) => row.sealed_credential.clone(),
        (Some(_), None) => return Err(SaveError::MissingCredential),
        (None, Some(row)) => row.sealed_credential.clone(),
        (None, None) => return Err(SaveError::MissingCredential),
    };
    let mut sections = existing
        .as_ref()
        .map(|row| row.section_ids.clone())
        .unwrap_or_default();
    if let Some(section) = draft.section_id
        && !section.is_empty()
        && !sections.contains(&section)
    {
        sections.push(section);
    }
    let row = StoredConnection {
        owner_id: owner_id.to_owned(),
        source,
        base_url: draft
            .base_url
            .or_else(|| existing.as_ref().map(|row| row.base_url.clone()))
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_owned(),
        username: draft
            .username
            .or_else(|| existing.as_ref().map(|row| row.username.clone()))
            .unwrap_or_default(),
        sealed_credential: sealed,
        enabled: true,
        client_id: draft
            .client_id
            .or_else(|| existing.as_ref().map(|row| row.client_id.clone()))
            .unwrap_or_default(),
        user_id: draft
            .user_id
            .or_else(|| existing.as_ref().map(|row| row.user_id.clone()))
            .unwrap_or_default(),
        section_ids: sections,
    };
    if row.base_url.is_empty() {
        return Err(SaveError::MissingBaseUrl);
    }
    store.put(row.clone()).await;
    Ok(row)
}

/// Every way a connection save can fail. All render as 4xx input errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// No credential supplied and none stored.
    MissingCredential,
    /// No base URL supplied and none stored.
    MissingBaseUrl,
    /// The secrets core refused to seal.
    SealFailed,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredential => f.write_str("a credential is required"),
            Self::MissingBaseUrl => f.write_str("a server URL is required"),
            Self::SealFailed => f.write_str("could not seal the credential"),
        }
    }
}

/// Resolve a caller's connection: own enabled row first (`linked`), then
/// the shared admin row (`shared`). Unopenable ciphertext reads as a stale
/// credential needing relink, never as a retryable error.
pub async fn resolve_connection(
    store: &dyn ConnectionStore,
    coder: &CredentialCoder,
    user_id: &str,
    source: SourceName,
) -> Result<ResolvedConnection, ResolveError> {
    if let Some(row) = store.get(user_id, source).await
        && row.enabled
    {
        return open_row(coder, &row, user_id, "linked").await;
    }
    if let Some(row) = store.get(SHARED_OWNER, source).await
        && row.enabled
    {
        return open_row(coder, &row, user_id, "shared").await;
    }
    Err(ResolveError::NotConfigured)
}

async fn open_row(
    coder: &CredentialCoder,
    row: &StoredConnection,
    user_id: &str,
    mode: &str,
) -> Result<ResolvedConnection, ResolveError> {
    let credential = coder
        .open(&row.sealed_credential)
        .map_err(|_| ResolveError::Stale)?;
    if row.base_url.is_empty() || credential.is_empty() {
        return Err(ResolveError::NotConfigured);
    }
    Ok(ResolvedConnection {
        source: row.source,
        base_url: row.base_url.clone(),
        username: row.username.clone(),
        credential,
        account_mode: mode.to_owned(),
        account_label: if row.username.is_empty() {
            row.source.display().to_owned()
        } else {
            row.username.clone()
        },
        cache_scope: cache_scope(user_id, row),
        client_id: row.client_id.clone(),
        user_id: row.user_id.clone(),
        section_ids: row.section_ids.clone(),
    })
}

/// Every way connection resolution can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No usable row for this caller and source.
    NotConfigured,
    /// The stored ciphertext no longer opens under this key. Relink.
    Stale,
}

/// Per-owner cache scope: `user:{id}:{generation}` where the generation
/// hashes the connection material, so a credential change retires the old
/// scope instead of serving another account's entries.
fn cache_scope(user_id: &str, row: &StoredConnection) -> String {
    let mut hasher = Sha256::new();
    hasher.update(row.source.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(row.base_url.as_bytes());
    hasher.update([0]);
    hasher.update(row.sealed_credential.as_bytes());
    let digest = hasher.finalize();
    let generation: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("user:{user_id}:{generation}")
}

/// Re-encrypt every stored credential from `old` to `new`, in place. Rows
/// that no longer open are left untouched and reported by owner+source so
/// the caller can flag them for relink instead of destroying them.
pub async fn rekey_store(
    store: &dyn ConnectionStore,
    old: &CredentialCoder,
    new: &CredentialCoder,
) -> Vec<(String, SourceName)> {
    let mut stale = Vec::new();
    for mut row in store.list_all().await {
        let opened = old.open(&row.sealed_credential);
        let Ok(plaintext) = opened else {
            stale.push((row.owner_id.clone(), row.source));
            continue;
        };
        let Ok(resealed) = new.seal(&plaintext) else {
            stale.push((row.owner_id.clone(), row.source));
            continue;
        };
        row.sealed_credential = resealed;
        store.put(row).await;
    }
    stale
}
