//! Production compat password store: both protocols over the real users
//! tables.
//!
//! Reads `connect_app_passwords` (decrypting `secret_encrypted` for the
//! Subsonic token scheme) and `auth_users` only. Account passwords and
//! native tokens are unreachable here by construction, so presenting one
//! fails exactly like an unknown credential. `note_use` throttles
//! `last_used_at` writes to one per secret per five minutes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::jellyfin::{JellyfinStoreError, JellyfinUser};
use super::subsonic::{AppSecret, SubsonicStoreError};
use crate::session::tokens;
use crate::users::models::UserRecord;
use crate::users::stores::StoreError;
use crate::users::{UsersDeps, clock_now};
use droppedneedle_kernel::runtime_config::Crypto;

/// Minimum gap between `last_used_at` writes for one secret.
const TOUCH_THROTTLE: Duration = Duration::from_secs(5 * 60);

/// Compat-contract store over the real users tables.
#[derive(Clone)]
pub struct ProdCompatPasswords {
    users: UsersDeps,
    crypto: Arc<Crypto>,
    touched: Arc<Mutex<HashMap<String, Instant>>>,
}

impl ProdCompatPasswords {
    /// Wrap the users deps and the config crypto handle.
    pub fn new(users: UsersDeps, crypto: Arc<Crypto>) -> Self {
        Self {
            users,
            crypto,
            touched: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The platform clock the users deps carry.
    pub fn clock(&self) -> &Arc<dyn droppedneedle_kernel::clock::Clock> {
        &self.users.clock
    }

    async fn user_row(&self, username_lower: &str) -> Result<Option<UserRecord>, StoreError> {
        self.users.users.get_by_username(username_lower).await
    }

    fn jellyfin_user(row: &UserRecord, last_used_at: Option<i64>) -> JellyfinUser {
        JellyfinUser {
            id: row.id.clone(),
            username: row.username.clone(),
            username_display: row.username_display.clone(),
            display_name: row.display_name.clone(),
            role: row.role.as_str().to_owned(),
            last_used_at,
        }
    }

    /// Best-effort throttled use stamp. A failed stamp logs and never fails
    /// auth; stamps closer than [`TOUCH_THROTTLE`] apart are skipped to
    /// spare the writer.
    pub(crate) async fn note_use(&self, secret: &str, client: Option<&str>) {
        let sha = tokens::hash_token(secret);
        let now = Instant::now();
        let due = self
            .touched
            .lock()
            .ok()
            .map(|mut touched| match touched.get(&sha) {
                Some(last) if now.duration_since(*last) < TOUCH_THROTTLE => false,
                _ => {
                    touched.insert(sha.clone(), now);
                    true
                }
            })
            .unwrap_or(true);
        if !due {
            return;
        }
        if let Err(error) = self
            .users
            .app_passwords
            .touch(&sha, clock_now(&self.users), client)
            .await
        {
            tracing::warn!(%error, "app-password use stamp failed");
        }
    }

    /// User id for a lowercased username.
    pub(crate) async fn user_id_for_username(
        &self,
        username_lower: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        let row = self.user_row(username_lower).await.map_err(|error| {
            tracing::error!(%error, "compat user lookup failed");
            SubsonicStoreError
        })?;
        Ok(row.map(|row| row.id))
    }

    /// Active secrets for a user (plaintext needed for `t/s` auth).
    pub(crate) async fn active_secrets(
        &self,
        user_id: &str,
    ) -> Result<Vec<AppSecret>, SubsonicStoreError> {
        let rows = self
            .users
            .app_passwords
            .list_active_by_user(user_id)
            .await
            .map_err(|_| SubsonicStoreError)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let plaintext = self
                .crypto
                .decrypt(&row.secret_encrypted)
                .map_err(|_| SubsonicStoreError)?;
            out.push(AppSecret {
                sha256: row.secret_sha256,
                plaintext,
            });
        }
        Ok(out)
    }

    /// Owning user id for a secret hash.
    pub(crate) async fn owner_of_secret(
        &self,
        secret_sha256: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        Ok(self
            .users
            .app_passwords
            .get_active_by_sha256(secret_sha256)
            .await
            .map_err(|_| SubsonicStoreError)?
            .map(|row| row.user_id))
    }

    /// User for a presented token (SHA-256 lookup inside).
    pub async fn user_for_token(
        &self,
        token: &str,
    ) -> Result<Option<JellyfinUser>, JellyfinStoreError> {
        let row = self
            .users
            .app_passwords
            .get_active_by_sha256(&tokens::hash_token(token))
            .await
            .map_err(|_| JellyfinStoreError)?;
        let Some(secret) = row else {
            return Ok(None);
        };
        let user = self
            .users
            .users
            .get_by_id(&secret.user_id)
            .await
            .map_err(|_| JellyfinStoreError)?;
        Ok(user
            .as_ref()
            .map(|user| Self::jellyfin_user(user, secret.last_used_at)))
    }

    /// User for a username plus app password. The username rule is v2
    /// verbatim: the stored lowercased username must equal the input
    /// stripped and lowercased (display names never match).
    pub(crate) async fn user_for_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<JellyfinUser>, JellyfinStoreError> {
        let wanted = username.trim().to_lowercase();
        let row = self
            .users
            .app_passwords
            .get_active_by_sha256(&tokens::hash_token(password))
            .await
            .map_err(|_| JellyfinStoreError)?;
        let Some(secret) = row else {
            return Ok(None);
        };
        let user = self
            .users
            .users
            .get_by_id(&secret.user_id)
            .await
            .map_err(|_| JellyfinStoreError)?;
        // v2 rule verbatim: the stored lowercased username must equal
        // the input stripped and lowercased; display names never match.
        Ok(user
            .filter(|row| row.username.as_deref() == Some(wanted.as_str()))
            .as_ref()
            .map(|user| Self::jellyfin_user(user, secret.last_used_at)))
    }
}
