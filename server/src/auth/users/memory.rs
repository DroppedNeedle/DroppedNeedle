//! Test doubles: in-memory stores, fake seams, and the test rig.
//!
//! These fakes back the tests. The memory session store implements the
//! [`SessionManager`](super::stores::SessionManager) trait, so the sessions
//! backend runs in tests without SQLite.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use axum::{
    Router,
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
};
use tokio::sync::{Mutex, RwLock};

use super::super::federated::password_import::PasswordHasher;
use super::super::federated::users::ProviderBinding;
use super::super::session::extract::Transport;
use super::super::session::middleware::CurrentSession;
use super::super::session::store::SessionKind;
use super::super::session::tokens;
use super::UsersDeps;
use super::hibp::{HibpHttp, HibpHttpError};
use super::models::{
    AppPasswordRecord, LastFmConnection, LocalCredential, ManagedSession, RecoveryCode,
    SessionOwner, UserRecord,
};
use super::roles::Role;
use super::services::COMPANION_LABEL_PREFIX;
use super::stores::{
    AppPasswordStore, AvatarStore, BoxFuture, Clock, DirectoryError, DirectoryUser,
    FalliblePasswordHasher, HashError, HibpPolicy, LastFmAuthClient, LastFmError, LastFmStore,
    LastFmSwitch, LoadedAvatar, RecoveryStore, RoleChange, SecurityPolicy, SessionManager,
    StoreError, UserDeletion, UserDirectory, UserStore,
};
use crate::{ids::IdGenerator, runtime_config::crypto::Crypto};

// ---------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------

/// In-memory [`UserStore`].
#[derive(Debug, Default)]
pub struct MemoryUserStore {
    state: RwLock<MemoryUserState>,
    reset_peers: OnceLock<ResetPeers>,
}

/// Peer stores the atomic recovery reset writes to. Linked once by the
/// rig; an unlinked store fails the reset loudly instead of half-applying it.
#[derive(Debug, Clone)]
struct ResetPeers {
    sessions: Arc<MemorySessionManager>,
    recovery: Arc<MemoryRecoveryStore>,
}

/// In-memory [`SessionManager`]. Takes a clock so expiry tests control time.
pub struct MemorySessionManager {
    clock: Arc<dyn Clock>,
    state: RwLock<MemorySessionState>,
}

impl std::fmt::Debug for MemorySessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySessionManager")
            .finish_non_exhaustive()
    }
}

/// In-memory [`AppPasswordStore`].
#[derive(Debug, Default)]
pub struct MemoryAppPasswordStore {
    state: RwLock<MemoryAppPasswordState>,
}

/// In-memory [`LastFmStore`].
#[derive(Debug, Default)]
pub struct MemoryLastFmStore {
    state: RwLock<HashMap<String, LastFmConnection>>,
}

/// In-memory [`RecoveryStore`].
#[derive(Debug, Default)]
pub struct MemoryRecoveryStore {
    state: RwLock<HashMap<String, RecoveryCode>>,
}

/// In-memory [`AvatarStore`].
#[derive(Debug, Default)]
pub struct MemoryAvatarStore {
    state: RwLock<HashMap<String, (Vec<u8>, String)>>,
}

#[derive(Debug, Default)]
struct MemoryUserState {
    users: HashMap<String, UserRecord>,
    local: HashMap<String, LocalCredential>,
    /// Federated bindings by `(provider, provider_uid)`, mirroring the
    /// UNIQUE pair on `auth_providers`.
    providers: HashMap<(String, String), ProviderBinding>,
}

#[derive(Debug, Clone)]
struct StoredSession {
    user_id: String,
    token_hash: String,
    kind: SessionKind,
    user_agent: String,
    created_at: i64,
    last_seen_at: i64,
    expires_at: i64,
    revoked: bool,
}

#[derive(Debug, Default)]
struct MemorySessionState {
    sessions: HashMap<String, StoredSession>,
}

#[derive(Debug, Default)]
struct MemoryAppPasswordState {
    rows: HashMap<String, AppPasswordRecord>,
    /// Soft-revoke tombstones, mirroring the SQLite `revoked` flag.
    revoked: HashSet<String>,
}

impl MemoryUserStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Link the peer stores the atomic recovery reset writes to. The
    /// rig calls this once at build; first link wins.
    pub fn link_reset_peers(
        &self,
        sessions: Arc<MemorySessionManager>,
        recovery: Arc<MemoryRecoveryStore>,
    ) {
        let _ = self.reset_peers.set(ResetPeers { sessions, recovery });
    }
}

impl Default for MemorySessionManager {
    fn default() -> Self {
        Self {
            clock: Arc::new(super::stores::SystemClock),
            state: RwLock::default(),
        }
    }
}

impl MemorySessionManager {
    /// Empty store reading expiry from `clock`.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            state: RwLock::default(),
        }
    }

    /// Seed one standard session directly (test setup).
    pub async fn seed_standard(
        &self,
        id: &str,
        user_id: &str,
        token_hash: &str,
        user_agent: &str,
        now: i64,
        expires_at: i64,
    ) {
        self.state.write().await.sessions.insert(
            id.to_owned(),
            StoredSession {
                user_id: user_id.to_owned(),
                token_hash: token_hash.to_owned(),
                kind: SessionKind::Standard,
                user_agent: user_agent.to_owned(),
                created_at: now,
                last_seen_at: now,
                expires_at,
                revoked: false,
            },
        );
    }
}

impl MemoryAppPasswordStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl MemoryLastFmStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl MemoryRecoveryStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl MemoryAvatarStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl UserStore for MemoryUserStore {
    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move { Ok(self.state.read().await.users.get(id).cloned()) })
    }

    fn get_by_username<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            Ok(self
                .state
                .read()
                .await
                .users
                .values()
                .find(|user| user.username.as_deref() == Some(username))
                .cloned())
        })
    }

    fn get_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            Ok(self
                .state
                .read()
                .await
                .users
                .values()
                .find(|user| user.email.as_deref() == Some(email))
                .cloned())
        })
    }

    fn get_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<UserRecord>, StoreError>> {
        Box::pin(async move {
            let guard = self.state.read().await;
            Ok(ids
                .iter()
                .filter_map(|id| guard.users.get(id).cloned())
                .collect())
        })
    }

    fn insert<'a>(&'a self, user: UserRecord) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if guard.users.contains_key(&user.id) {
                return Err(StoreError::Conflict);
            }
            if let Some(username) = user.username.as_deref()
                && guard
                    .users
                    .values()
                    .any(|row| row.username.as_deref() == Some(username))
            {
                return Err(StoreError::Conflict);
            }
            if let Some(email) = user.email.as_deref()
                && guard
                    .users
                    .values()
                    .any(|row| row.email.as_deref() == Some(email))
            {
                return Err(StoreError::Conflict);
            }
            guard.users.insert(user.id.clone(), user);
            Ok(())
        })
    }

    fn insert_with_local_credential<'a>(
        &'a self,
        user: UserRecord,
        credential: LocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.insert(user).await?;
            self.insert_local_credential(credential).await
        })
    }

    fn insert_first_user<'a>(
        &'a self,
        user: UserRecord,
        credential: LocalCredential,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            if !self.state.read().await.users.is_empty() {
                return Ok(false);
            }
            self.insert_with_local_credential(user, credential).await?;
            Ok(true)
        })
    }

    fn update_profile<'a>(
        &'a self,
        id: &'a str,
        display_name: Option<&'a str>,
        avatar_url: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            let Some(row) = guard.users.get_mut(id) else {
                return Ok(false);
            };
            if let Some(name) = display_name {
                row.display_name = name.to_owned();
            }
            if let Some(url) = avatar_url {
                row.avatar_url = Some(url.to_owned());
            }
            Ok(true)
        })
    }

    fn update_username<'a>(
        &'a self,
        id: &'a str,
        username: &'a str,
        username_display: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if guard
                .users
                .values()
                .any(|row| row.id != id && row.username.as_deref() == Some(username))
            {
                return Err(StoreError::Conflict);
            }
            let Some(row) = guard.users.get_mut(id) else {
                return Ok(false);
            };
            row.username = Some(username.to_owned());
            row.username_display = Some(username_display.to_owned());
            Ok(true)
        })
    }

    fn update_email<'a>(
        &'a self,
        id: &'a str,
        email: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if let Some(candidate) = email
                && guard
                    .users
                    .values()
                    .any(|row| row.id != id && row.email.as_deref() == Some(candidate))
            {
                return Err(StoreError::Conflict);
            }
            let Some(row) = guard.users.get_mut(id) else {
                return Ok(false);
            };
            row.email = email.map(str::to_owned);
            Ok(true)
        })
    }

    fn set_role<'a>(
        &'a self,
        id: &'a str,
        role: Role,
    ) -> BoxFuture<'a, Result<RoleChange, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            let admins = guard
                .users
                .values()
                .filter(|row| row.role.is_admin())
                .count();
            let Some(row) = guard.users.get_mut(id) else {
                return Ok(RoleChange::NotFound);
            };
            if row.role.is_admin() && !role.is_admin() && admins <= 1 {
                return Ok(RoleChange::LastAdmin);
            }
            row.role = role;
            Ok(RoleChange::Changed)
        })
    }

    fn touch_login<'a>(&'a self, id: &'a str, at: i64) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            if let Some(row) = self.state.write().await.users.get_mut(id) {
                row.last_login_at = Some(at);
            }
            Ok(())
        })
    }

    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<UserDeletion, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            let admins = guard
                .users
                .values()
                .filter(|row| row.role.is_admin())
                .count();
            let Some(row) = guard.users.get(id) else {
                return Ok(UserDeletion::NotFound);
            };
            if row.role.is_admin() && admins <= 1 {
                return Ok(UserDeletion::LastAdmin);
            }
            guard.local.remove(id);
            guard.providers.retain(|_, binding| binding.user_id != id);
            guard.users.remove(id);
            Ok(UserDeletion::Deleted)
        })
    }

    fn list<'a>(
        &'a self,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<UserRecord>, u64), StoreError>> {
        Box::pin(async move {
            let guard = self.state.read().await;
            let mut users: Vec<UserRecord> = guard.users.values().cloned().collect();
            users.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            let total = users.len() as u64;
            let offset = (offset as usize).min(users.len());
            let end = offset.saturating_add(limit as usize).min(users.len());
            Ok((users[offset..end].to_vec(), total))
        })
    }

    fn provider_names<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<String>, StoreError>> {
        Box::pin(async move {
            let guard = self.state.read().await;
            let mut names = Vec::new();
            if guard.local.contains_key(id) {
                names.push("local".to_owned());
            }
            for binding in guard.providers.values() {
                if binding.user_id == id && !names.contains(&binding.provider) {
                    names.push(binding.provider.clone());
                }
            }
            names.sort();
            Ok(names)
        })
    }

    fn local_credential<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LocalCredential>, StoreError>> {
        Box::pin(async move { Ok(self.state.read().await.local.get(id).cloned()) })
    }

    fn insert_local_credential<'a>(
        &'a self,
        credential: LocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if guard.local.contains_key(&credential.user_id) {
                return Err(StoreError::Conflict);
            }
            guard.local.insert(credential.user_id.clone(), credential);
            Ok(())
        })
    }

    fn change_local_hash<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
        keep_session_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some(peers) = self.reset_peers.get() else {
                return Err(StoreError::Internal(
                    "password change peers are not linked".to_owned(),
                ));
            };
            let mut users = self.state.write().await;
            let Some(row) = users.local.get_mut(id) else {
                return Ok(false);
            };
            if row.hash != expected_hash {
                return Ok(false);
            }
            row.scheme = scheme.to_owned();
            row.hash = new_hash.to_owned();
            for (session_id, row) in peers.sessions.state.write().await.sessions.iter_mut() {
                if row.user_id == id && session_id != keep_session_id {
                    row.revoked = true;
                }
            }
            Ok(true)
        })
    }

    fn complete_recovery_reset<'a>(
        &'a self,
        id: &'a str,
        expected_hash: &'a str,
        scheme: &'a str,
        new_hash: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some(peers) = self.reset_peers.get() else {
                return Err(StoreError::Internal(
                    "recovery reset peers are not linked".to_owned(),
                ));
            };
            // Fixed lock order (users, sessions, recovery): this is the only
            // path that ever holds two of these locks at once.
            let mut users = self.state.write().await;
            let Some(row) = users.local.get_mut(id) else {
                return Ok(false);
            };
            if row.hash != expected_hash {
                return Ok(false);
            }
            row.scheme = scheme.to_owned();
            row.hash = new_hash.to_owned();
            for row in peers.sessions.state.write().await.sessions.values_mut() {
                if row.user_id == id {
                    row.revoked = true;
                }
            }
            peers.recovery.state.write().await.remove(id);
            Ok(true)
        })
    }

    fn get_provider_binding<'a>(
        &'a self,
        provider: &'a str,
        provider_uid: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderBinding>, StoreError>> {
        Box::pin(async move {
            Ok(self
                .state
                .read()
                .await
                .providers
                .get(&(provider.to_owned(), provider_uid.to_owned()))
                .cloned())
        })
    }

    fn insert_provider_binding<'a>(
        &'a self,
        binding: ProviderBinding,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            let key = (binding.provider.clone(), binding.provider_uid.clone());
            if guard.providers.contains_key(&key) {
                return Err(StoreError::Conflict);
            }
            guard.providers.insert(key, binding);
            Ok(())
        })
    }
}

impl SessionManager for MemorySessionManager {
    fn list_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ManagedSession>, StoreError>> {
        Box::pin(async move {
            let now = self.clock.now_unix();
            let mut rows: Vec<ManagedSession> = self
                .state
                .read()
                .await
                .sessions
                .iter()
                .filter(|(_, row)| row.user_id == user_id && !row.revoked && row.expires_at > now)
                .map(|(id, row)| ManagedSession {
                    id: id.clone(),
                    user_id: row.user_id.clone(),
                    kind: row.kind,
                    label: row.user_agent.clone(),
                    created_at: row.created_at,
                    last_seen_at: row.last_seen_at,
                    expires_at: row.expires_at,
                })
                .collect();
            rows.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
            Ok(rows)
        })
    }

    fn revoke_scoped<'a>(
        &'a self,
        user_id: &'a str,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            match guard.sessions.get_mut(session_id) {
                Some(row) if row.user_id == user_id && !row.revoked => {
                    row.revoked = true;
                    Ok(true)
                }
                _ => Ok(false),
            }
        })
    }

    fn revoke_all_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let mut count = 0u64;
            for row in self.state.write().await.sessions.values_mut() {
                if row.user_id == user_id && !row.revoked {
                    row.revoked = true;
                    count += 1;
                }
            }
            Ok(count)
        })
    }

    fn replace_companion<'a>(
        &'a self,
        id: &'a str,
        user_id: &'a str,
        token_hash: &'a str,
        label: &'a str,
        issued_at: i64,
        expires_at: i64,
    ) -> BoxFuture<'a, Result<ManagedSession, StoreError>> {
        Box::pin(async move {
            let user_agent = format!("{COMPANION_LABEL_PREFIX}{label}");
            let mut guard = self.state.write().await;
            if guard.sessions.contains_key(id) {
                return Err(StoreError::Conflict);
            }
            guard.sessions.insert(
                id.to_owned(),
                StoredSession {
                    user_id: user_id.to_owned(),
                    token_hash: token_hash.to_owned(),
                    kind: SessionKind::Companion,
                    user_agent: user_agent.clone(),
                    created_at: issued_at,
                    last_seen_at: issued_at,
                    expires_at,
                    revoked: false,
                },
            );
            // Same write lock: the replace is atomic.
            for (other_id, row) in guard.sessions.iter_mut() {
                if other_id.as_str() != id
                    && row.user_id == user_id
                    && row.user_agent == user_agent
                    && row.kind == SessionKind::Companion
                    && !row.revoked
                    && row.expires_at > issued_at
                {
                    row.revoked = true;
                }
            }
            Ok(ManagedSession {
                id: id.to_owned(),
                user_id: user_id.to_owned(),
                kind: SessionKind::Companion,
                label: user_agent,
                created_at: issued_at,
                last_seen_at: issued_at,
                expires_at,
            })
        })
    }

    fn owner_by_hash<'a>(
        &'a self,
        token_hash: &'a str,
    ) -> BoxFuture<'a, Result<Option<SessionOwner>, StoreError>> {
        Box::pin(async move {
            let now = self.clock.now_unix();
            Ok(self
                .state
                .read()
                .await
                .sessions
                .iter()
                .find(|(_, row)| {
                    row.token_hash == token_hash && !row.revoked && row.expires_at > now
                })
                .map(|(id, row)| SessionOwner {
                    session_id: id.clone(),
                    user_id: row.user_id.clone(),
                    kind: row.kind,
                }))
        })
    }
}

impl AppPasswordStore for MemoryAppPasswordStore {
    fn insert_capped<'a>(
        &'a self,
        row: AppPasswordRecord,
        max_active: u64,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if guard.rows.contains_key(&row.id)
                || guard
                    .rows
                    .values()
                    .any(|existing| existing.secret_sha256 == row.secret_sha256)
            {
                return Err(StoreError::Conflict);
            }
            let active = guard
                .rows
                .values()
                .filter(|existing| {
                    existing.user_id == row.user_id && !guard.revoked.contains(&existing.id)
                })
                .count() as u64;
            if active >= max_active {
                return Ok(false);
            }
            guard.rows.insert(row.id.clone(), row);
            Ok(true)
        })
    }

    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>> {
        Box::pin(async move { Ok(self.state.read().await.rows.get(id).cloned()) })
    }

    fn get_active_by_sha256<'a>(
        &'a self,
        secret_sha256: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let guard = self.state.read().await;
            Ok(guard
                .rows
                .values()
                .find(|row| row.secret_sha256 == secret_sha256 && !guard.revoked.contains(&row.id))
                .cloned())
        })
    }

    fn list_active_by_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let guard = self.state.read().await;
            let mut rows: Vec<AppPasswordRecord> = guard
                .rows
                .values()
                .filter(|row| row.user_id == user_id && !guard.revoked.contains(&row.id))
                .cloned()
                .collect();
            rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(rows)
        })
    }

    fn list_all_active<'a>(&'a self) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let guard = self.state.read().await;
            let mut rows: Vec<AppPasswordRecord> = guard
                .rows
                .values()
                .filter(|row| !guard.revoked.contains(&row.id))
                .cloned()
                .collect();
            rows.sort_by(|a, b| {
                a.user_id
                    .cmp(&b.user_id)
                    .then(a.created_at.cmp(&b.created_at))
            });
            Ok(rows)
        })
    }

    fn revoke<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            if !guard.rows.contains_key(id) || guard.revoked.contains(id) {
                return Ok(false);
            }
            guard.revoked.insert(id.to_owned());
            Ok(true)
        })
    }

    fn touch<'a>(
        &'a self,
        secret_sha256: &'a str,
        last_used_at: i64,
        last_client: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            if let Some(row) = self
                .state
                .write()
                .await
                .rows
                .values_mut()
                .find(|row| row.secret_sha256 == secret_sha256)
            {
                row.last_used_at = Some(last_used_at);
                if last_client.is_some() {
                    row.last_client = last_client.map(str::to_owned);
                }
            }
            Ok(())
        })
    }
}

impl LastFmStore for MemoryLastFmStore {
    fn get<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LastFmConnection>, StoreError>> {
        Box::pin(async move { Ok(self.state.read().await.get(user_id).cloned()) })
    }

    fn upsert<'a>(
        &'a self,
        user_id: &'a str,
        link: LastFmConnection,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.state.write().await.insert(user_id.to_owned(), link);
            Ok(())
        })
    }

    fn delete<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move { Ok(self.state.write().await.remove(user_id).is_some()) })
    }
}

impl RecoveryStore for MemoryRecoveryStore {
    fn store<'a>(&'a self, code: RecoveryCode) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut guard = self.state.write().await;
            // Cheap sweep (v2 parity): rows already expired at mint time go.
            guard.retain(|_, row| row.expires_at > code.created_at);
            guard.insert(code.user_id.clone(), code);
            Ok(())
        })
    }

    fn find_live_by_hash<'a>(
        &'a self,
        code_hash: &'a str,
        now: i64,
    ) -> BoxFuture<'a, Result<Option<RecoveryCode>, StoreError>> {
        Box::pin(async move {
            Ok(self
                .state
                .read()
                .await
                .values()
                .find(|row| row.code_hash == code_hash && row.expires_at > now)
                .cloned())
        })
    }

    fn delete_for_user<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.state.write().await.remove(user_id);
            Ok(())
        })
    }
}

impl AvatarStore for MemoryAvatarStore {
    fn save<'a>(
        &'a self,
        user_id: &'a str,
        content_type: &'a str,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<String, StoreError>> {
        Box::pin(async move {
            let ext = match content_type {
                "image/jpeg" => "jpg",
                "image/png" => "png",
                "image/webp" => "webp",
                "image/gif" => "gif",
                _ => {
                    return Err(StoreError::Internal("unsupported avatar type".to_owned()));
                }
            };
            self.state.write().await.insert(
                user_id.to_owned(),
                (bytes.to_vec(), content_type.to_owned()),
            );
            Ok(ext.to_owned())
        })
    }

    fn load<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedAvatar>, StoreError>> {
        Box::pin(async move { Ok(self.state.read().await.get(user_id).cloned()) })
    }
}

// ---------------------------------------------------------------------------
// Hashing, time, policies, seams
// ---------------------------------------------------------------------------

/// Test-only password hasher over the federated [`PasswordHasher`] trait:
/// unsalted SHA-256 stored as `argon2id` rows so scheme dispatch runs.
/// Fast and deterministic for tests; never valid in production, where the
/// wiring installs the real bcrypt/Argon2id hasher.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sha256TestHasher;

impl Sha256TestHasher {
    /// Test hash encoding for one password.
    pub fn test_hash(password: &str) -> String {
        format!("argon2id${}", tokens::hash_token(password))
    }
}

impl PasswordHasher for Sha256TestHasher {
    fn verify_bcrypt(&self, _password: &str, _hash: &str) -> bool {
        false
    }

    fn verify_argon2id(&self, password: &str, hash: &str) -> bool {
        hash == Self::test_hash(password)
    }

    fn hash_argon2id(&self, password: &str) -> String {
        Self::test_hash(password)
    }

    fn dummy_verify(&self) {}
}

impl FalliblePasswordHasher for Sha256TestHasher {
    fn try_hash_argon2id(&self, password: &str) -> Result<String, HashError> {
        Ok(Self::test_hash(password))
    }
}

/// Manually advanced clock for expiry tests.
#[derive(Debug)]
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    /// Clock pinned at `now`.
    pub fn new(now: i64) -> Self {
        Self {
            now: AtomicI64::new(now),
        }
    }

    /// Move time forward (or back) to `now`.
    pub fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

/// Fixed HIBP knobs for tests.
#[derive(Debug, Clone)]
pub struct StaticSecurityPolicy {
    /// Knobs returned verbatim.
    pub policy: HibpPolicy,
}

impl SecurityPolicy for StaticSecurityPolicy {
    fn hibp(&self) -> HibpPolicy {
        self.policy.clone()
    }
}

/// Fixed Last.fm master switch for tests.
#[derive(Debug, Clone, Copy)]
pub struct StaticLastFmSwitch(pub bool);

impl LastFmSwitch for StaticLastFmSwitch {
    fn enabled(&self) -> bool {
        self.0
    }
}

/// Scripted Last.fm auth client. Unknown tokens exchange as unapproved,
/// exactly like the real flow before browser approval.
#[derive(Debug, Default)]
pub struct FakeLastFmAuthClient {
    state: Mutex<FakeLastFmState>,
}

#[derive(Debug, Default)]
struct FakeLastFmState {
    token: (String, String),
    token_error: Option<LastFmError>,
    sessions: HashMap<String, Result<(String, String), LastFmError>>,
}

impl FakeLastFmAuthClient {
    /// Fake whose token step returns `token`/`auth_url`.
    pub fn new(token: &str, auth_url: &str) -> Self {
        Self {
            state: Mutex::new(FakeLastFmState {
                token: (token.to_owned(), auth_url.to_owned()),
                token_error: None,
                sessions: HashMap::new(),
            }),
        }
    }

    /// Approve `token` for `username` with `session_key`.
    pub async fn approve(&self, token: &str, username: &str, session_key: &str) {
        self.state.lock().await.sessions.insert(
            token.to_owned(),
            Ok((username.to_owned(), session_key.to_owned())),
        );
    }

    /// Fail the token step with `error`.
    pub async fn fail_token_with(&self, error: LastFmError) {
        self.state.lock().await.token_error = Some(error);
    }
}

impl LastFmAuthClient for FakeLastFmAuthClient {
    fn request_token<'a>(
        &'a self,
        _api_key: &'a str,
        _shared_secret: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async move {
            let guard = self.state.lock().await;
            match guard.token_error.clone() {
                Some(error) => Err(error),
                None => Ok(guard.token.clone()),
            }
        })
    }

    fn exchange_session<'a>(
        &'a self,
        _api_key: &'a str,
        _shared_secret: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async move {
            match self.state.lock().await.sessions.get(token).cloned() {
                Some(result) => result,
                None => Err(LastFmError::TokenNotAuthorized),
            }
        })
    }
}

/// Scripted media-server user directory: serves a fixed catalog, or a
/// scripted fault, for one provider.
#[derive(Debug, Default)]
pub struct FakeUserDirectory {
    provider: &'static str,
    state: Mutex<FakeDirectoryState>,
}

#[derive(Debug, Default)]
struct FakeDirectoryState {
    users: Vec<DirectoryUser>,
    error: Option<DirectoryError>,
}

impl FakeUserDirectory {
    /// Fake Jellyfin directory serving `users`.
    pub fn jellyfin(users: Vec<DirectoryUser>) -> Self {
        Self::for_provider(super::super::federated::users::PROVIDER_JELLYFIN, users)
    }

    /// Fake Plex directory serving `users`.
    pub fn plex(users: Vec<DirectoryUser>) -> Self {
        Self::for_provider(super::super::federated::users::PROVIDER_PLEX, users)
    }

    fn for_provider(provider: &'static str, users: Vec<DirectoryUser>) -> Self {
        Self {
            provider,
            state: Mutex::new(FakeDirectoryState { users, error: None }),
        }
    }

    /// Fail every listing with `error`.
    pub async fn fail_with(&self, error: DirectoryError) {
        self.state.lock().await.error = Some(error);
    }
}

impl UserDirectory for FakeUserDirectory {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>> {
        Box::pin(async move {
            let guard = self.state.lock().await;
            match guard.error.clone() {
                Some(error) => Err(error),
                None => Ok(guard.users.clone()),
            }
        })
    }
}

/// Scripted HIBP range client: `hits` holds full uppercase digests.
#[derive(Debug, Default)]
pub struct FakeHibpHttp {
    hits: HashSet<String>,
}

impl FakeHibpHttp {
    /// Fake that reports every digest in `hits` as breached.
    pub fn with_hits(hits: &[&str]) -> Self {
        Self {
            hits: hits.iter().map(|hit| hit.to_string()).collect(),
        }
    }
}

impl HibpHttp for FakeHibpHttp {
    fn range<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, HibpHttpError>> {
        Box::pin(async move {
            Ok(self
                .hits
                .iter()
                .filter(|hit| hit.starts_with(prefix))
                .filter_map(|hit| hit.get(super::hibp::RANGE_PREFIX_LEN..).map(str::to_owned))
                .collect())
        })
    }
}

/// Deterministic id generator: `prefix` plus a zero-padded counter.
#[derive(Debug)]
pub struct CounterIdGenerator {
    prefix: String,
    next: AtomicU64,
}

impl CounterIdGenerator {
    /// Generator minting `prefix0001`, `prefix0002`, ...
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            next: AtomicU64::new(1),
        }
    }
}

impl IdGenerator for CounterIdGenerator {
    fn new_id(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        format!("{}{n:04}", self.prefix)
    }
}

// ---------------------------------------------------------------------------
// Test rig
// ---------------------------------------------------------------------------

/// Pinned test time: 2024-01-01T00:00:00Z.
pub const TEST_NOW: i64 = 1_704_067_200;

/// A full fake-backed [`UsersDeps`] plus handles to the fakes.
pub struct TestRig {
    /// Wired dependencies for routers and services.
    pub deps: UsersDeps,
    /// User store handle for seeding and assertions.
    pub users: Arc<MemoryUserStore>,
    /// Session store handle (stands in for the SQLite session store).
    pub sessions: Arc<MemorySessionManager>,
    /// Last.fm store handle.
    pub lastfm: Arc<MemoryLastFmStore>,
    /// Clock handle for time travel.
    pub clock: Arc<ManualClock>,
    /// Last.fm fake for scripting approvals and faults.
    pub lastfm_client: Arc<FakeLastFmAuthClient>,
}

impl TestRig {
    /// Build a rig: memory stores, test hasher, HIBP off, Last.fm on,
    /// fixed test crypto key. Fails only when the test key is rejected.
    pub fn new() -> Result<Self, crate::runtime_config::crypto::CryptoError> {
        let clock = Arc::new(ManualClock::new(TEST_NOW));
        let users = Arc::new(MemoryUserStore::new());
        let sessions = Arc::new(MemorySessionManager::new(
            Arc::clone(&clock) as Arc<dyn Clock>
        ));
        let app_passwords = Arc::new(MemoryAppPasswordStore::new());
        let lastfm = Arc::new(MemoryLastFmStore::new());
        let recovery = Arc::new(MemoryRecoveryStore::new());
        let avatars = Arc::new(MemoryAvatarStore::new());
        users.link_reset_peers(Arc::clone(&sessions), Arc::clone(&recovery));
        let lastfm_client = Arc::new(FakeLastFmAuthClient::new(
            "fake-token",
            "https://www.last.fm/api/auth/?api_key=k&token=fake-token",
        ));
        let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32])?);
        let screen: Arc<super::hibp::HibpScreen> = Arc::new(super::hibp::HibpScreen::new(
            Arc::new(FakeHibpHttp::default()),
        ));
        let deps = UsersDeps {
            users: Arc::clone(&users) as Arc<dyn UserStore>,
            sessions: Arc::clone(&sessions) as Arc<dyn SessionManager>,
            app_passwords: Arc::clone(&app_passwords) as Arc<dyn AppPasswordStore>,
            lastfm: Arc::clone(&lastfm) as Arc<dyn LastFmStore>,
            recovery: Arc::clone(&recovery) as Arc<dyn RecoveryStore>,
            avatars: Arc::clone(&avatars) as Arc<dyn AvatarStore>,
            passwords: Arc::new(Sha256TestHasher),
            screen,
            clock: Arc::clone(&clock) as Arc<dyn Clock>,
            ids: Arc::new(CounterIdGenerator::new("test-id-")),
            crypto,
            lastfm_client: Arc::clone(&lastfm_client) as Arc<dyn LastFmAuthClient>,
            lastfm_switch: Arc::new(StaticLastFmSwitch(true)),
            security: Arc::new(StaticSecurityPolicy {
                policy: HibpPolicy {
                    check: false,
                    local_path: String::new(),
                },
            }),
            jellyfin_directory: Arc::new(FakeUserDirectory::jellyfin(Vec::new())),
            plex_directory: Arc::new(FakeUserDirectory::plex(Vec::new())),
        };
        Ok(Self {
            deps,
            users,
            sessions,
            lastfm,
            clock,
            lastfm_client,
        })
    }

    /// Seed one user with a local password, returning the row.
    pub async fn seed_user(&self, username: &str, role: Role) -> UserRecord {
        let now = self.clock.now_unix();
        let row = UserRecord {
            id: format!("user-{username}"),
            username: Some(username.to_owned()),
            username_display: Some(username.to_owned()),
            display_name: username.to_owned(),
            email: None,
            avatar_url: None,
            role,
            created_at: now,
            last_login_at: None,
        };
        self.users.insert(row.clone()).await.ok();
        self.users
            .insert_local_credential(LocalCredential {
                id: format!("cred-{username}"),
                user_id: row.id.clone(),
                scheme: "argon2id".to_owned(),
                // Note: every seeded password is `correct horse battery staple`.
                hash: Sha256TestHasher::test_hash("correct horse battery staple"),
            })
            .await
            .ok();
        row
    }

    /// Seed one standard session for `user_id`, returning (id, raw token).
    pub async fn seed_session(&self, user_id: &str, session_id: &str) -> String {
        let now = self.clock.now_unix();
        let token = format!("raw-token-for-{session_id}");
        self.sessions
            .seed_standard(
                session_id,
                user_id,
                &tokens::hash_token(&token),
                "TestBrowser/1.0",
                now,
                now + tokens::SESSION_MAX_AGE_SECS,
            )
            .await;
        token
    }
}

/// Session for `user` on `session_id`, mimicking what the session
/// middleware stashes after resolving a token.
pub fn test_principal(user: &UserRecord, session_id: &str, companion: bool) -> CurrentSession {
    CurrentSession {
        user_id: user.id.clone(),
        session_id: session_id.to_owned(),
        kind: if companion {
            SessionKind::Companion
        } else {
            SessionKind::Standard
        },
        transport: Transport::Cookie,
    }
}

async fn insert_test_principal(
    State(session): State<CurrentSession>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(session);
    next.run(request).await
}

/// Wrap a router with the test principal layer, standing in for the
/// session middleware (session resolution only, no origin check).
pub fn with_test_principal(router: Router, session: CurrentSession) -> Router {
    router.layer(middleware::from_fn_with_state(
        session,
        insert_test_principal,
    ))
}
