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

use super::jellyfin::{JellyfinPasswordStore, JellyfinStoreError, JellyfinUser};
use super::subsonic::{AppSecret, SubsonicPasswordStore, SubsonicStoreError};
use crate::auth::session::tokens;
use crate::auth::users::models::UserRecord;
use crate::auth::users::{UsersDeps, clock_now};
use crate::runtime_config::Crypto;

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

    async fn user_row(&self, username_lower: &str) -> Option<UserRecord> {
        self.users
            .users
            .get_by_username(username_lower)
            .await
            .ok()
            .flatten()
    }

    fn jellyfin_user(row: &UserRecord) -> JellyfinUser {
        JellyfinUser {
            id: row.id.clone(),
            username: row.username.clone(),
            username_display: row.username_display.clone(),
            display_name: row.display_name.clone(),
            role: row.role.as_str().to_owned(),
        }
    }

    /// Best-effort throttled use stamp. Failures are ignored (a lost stamp
    /// must never fail auth), and stamps closer than [`TOUCH_THROTTLE`]
    /// apart are skipped to spare the writer.
    async fn touch(&self, secret: &str, client: Option<&str>) {
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
        let _ = self
            .users
            .app_passwords
            .touch(&sha, clock_now(&self.users), client)
            .await;
    }
}

impl SubsonicPasswordStore for ProdCompatPasswords {
    async fn user_id_for_username(
        &self,
        username_lower: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        Ok(self.user_row(username_lower).await.map(|row| row.id))
    }

    async fn active_secrets(&self, user_id: &str) -> Result<Vec<AppSecret>, SubsonicStoreError> {
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

    async fn owner_of_secret(
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

    async fn note_use(&self, secret_plaintext: &str, client: Option<&str>) {
        self.touch(secret_plaintext, client).await;
    }
}

impl JellyfinPasswordStore for ProdCompatPasswords {
    async fn user_for_token(
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
        Ok(user.as_ref().map(Self::jellyfin_user))
    }

    async fn user_for_credentials(
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
            .map(Self::jellyfin_user))
    }

    async fn note_use(&self, secret_plaintext: &str, client: Option<&str>) {
        self.touch(secret_plaintext, client).await;
    }
}
