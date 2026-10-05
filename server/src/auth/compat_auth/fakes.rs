//! In-memory compat password store implementing both protocols. Users
//! carry an account password (which must never verify) plus app-password
//! secrets (the only compat credential). Clones share state through
//! `Arc`, so tests seed one handle and verify through another.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::jellyfin::{JellyfinPasswordStore, JellyfinStoreError, JellyfinUser};
use super::subsonic::{AppSecret, SubsonicPasswordStore, SubsonicStoreError, sha256_hex};

#[derive(Debug, Clone)]
struct FakeCompatUser {
    id: String,
    username: String,
    username_display: String,
    display_name: String,
    role: String,
    secrets: Vec<String>,
}

#[derive(Debug, Default)]
struct FakeCompatRows {
    users: HashMap<String, FakeCompatUser>,
    touches: Vec<(String, Option<String>)>,
}

/// Fake store for both compat protocols.
#[derive(Debug, Clone, Default)]
pub struct FakeCompatPasswords {
    rows: Arc<Mutex<FakeCompatRows>>,
}

impl FakeCompatPasswords {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a user. The username is stored lowercased (v2 parity). The
    /// account password is accepted and dropped: compat logins never
    /// verify against it, only against app passwords.
    pub fn add_user(
        &self,
        user_id: &str,
        username: &str,
        display_name: &str,
        role: &str,
        _account_password: &str,
        app_secrets: &[&str],
    ) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.users.insert(
                username.to_lowercase(),
                FakeCompatUser {
                    id: user_id.to_owned(),
                    username: username.to_lowercase(),
                    username_display: username.to_owned(),
                    display_name: display_name.to_owned(),
                    role: role.to_owned(),
                    secrets: app_secrets
                        .iter()
                        .map(|secret| secret.to_string())
                        .collect(),
                },
            );
        }
    }

    /// Use stamps recorded as `(secret_sha256, client)`.
    pub fn touches(&self) -> Vec<(String, Option<String>)> {
        self.rows
            .lock()
            .map(|rows| rows.touches.clone())
            .unwrap_or_default()
    }

    fn locked(&self) -> Result<std::sync::MutexGuard<'_, FakeCompatRows>, SubsonicStoreError> {
        self.rows.lock().map_err(|_| SubsonicStoreError)
    }
}

impl SubsonicPasswordStore for FakeCompatPasswords {
    async fn user_id_for_username(
        &self,
        username_lower: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        Ok(self
            .locked()?
            .users
            .get(username_lower)
            .map(|user| user.id.clone()))
    }

    async fn active_secrets(&self, user_id: &str) -> Result<Vec<AppSecret>, SubsonicStoreError> {
        let rows = self.locked()?;
        let secrets = rows
            .users
            .values()
            .find(|user| user.id == user_id)
            .map(|user| {
                user.secrets
                    .iter()
                    .map(|plaintext| AppSecret {
                        sha256: sha256_hex(plaintext),
                        plaintext: plaintext.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(secrets)
    }

    async fn owner_of_secret(
        &self,
        secret_sha256: &str,
    ) -> Result<Option<String>, SubsonicStoreError> {
        let rows = self.locked()?;
        for user in rows.users.values() {
            for secret in &user.secrets {
                if sha256_hex(secret) == secret_sha256 {
                    return Ok(Some(user.id.clone()));
                }
            }
        }
        Ok(None)
    }

    async fn note_use(&self, secret_plaintext: &str, client: Option<&str>) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.touches
                .push((sha256_hex(secret_plaintext), client.map(str::to_owned)));
        }
    }
}

impl JellyfinPasswordStore for FakeCompatPasswords {
    async fn user_for_token(
        &self,
        token: &str,
    ) -> Result<Option<JellyfinUser>, JellyfinStoreError> {
        let rows = self.rows.lock().map_err(|_| JellyfinStoreError)?;
        for user in rows.users.values() {
            if user.secrets.iter().any(|secret| secret == token) {
                return Ok(Some(jellyfin_user(user)));
            }
        }
        Ok(None)
    }

    async fn user_for_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<JellyfinUser>, JellyfinStoreError> {
        let rows = self.rows.lock().map_err(|_| JellyfinStoreError)?;
        let digest = sha256_hex(password);
        for user in rows.users.values() {
            let owned = user
                .secrets
                .iter()
                .any(|secret| sha256_hex(secret) == digest);
            if owned && user.username == username.trim().to_lowercase() {
                return Ok(Some(jellyfin_user(user)));
            }
        }
        Ok(None)
    }

    async fn note_use(&self, secret_plaintext: &str, client: Option<&str>) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.touches
                .push((sha256_hex(secret_plaintext), client.map(str::to_owned)));
        }
    }
}

fn jellyfin_user(user: &FakeCompatUser) -> JellyfinUser {
    JellyfinUser {
        id: user.id.clone(),
        username: Some(user.username.clone()),
        username_display: Some(user.username_display.clone()),
        display_name: user.display_name.clone(),
        role: user.role.clone(),
    }
}
