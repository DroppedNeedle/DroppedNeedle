//! Federated user import over `auth_users` plus the non-local rows of
//! `auth_providers`.

use std::sync::Arc;

use sqlx::Row as _;

use super::{AuthDb, USER_COLUMNS, is_write_conflict, op_error, store_unavailable};
use crate::auth::federated::FederatedError;
use crate::auth::federated::users::{
    FederatedUserStore, NewFederatedUser, ProviderBinding, StoredUser,
};
use crate::auth::times::to_iso;
use crate::db::{Lane, map_sqlx_busy};
use crate::ids::IdGenerator;
use crate::runtime_config::crypto::Crypto;

/// Federated user import over `auth_users` plus the non-local rows of
/// `auth_providers`.
///
/// Every `token_json` is sealed with the deployment key before it reaches
/// `provider_data`, on both insert and rotation. No read path decrypts: the
/// port never returns token material.
#[derive(Clone)]
pub struct SqliteFederatedStore {
    db: AuthDb,
    crypto: Arc<Crypto>,
    ids: Arc<dyn IdGenerator>,
}

impl std::fmt::Debug for SqliteFederatedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteFederatedStore")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl SqliteFederatedStore {
    /// Adapter over one handle, sealing tokens with `crypto` and minting ids
    /// with `ids`.
    pub fn new(db: &AuthDb, crypto: Arc<Crypto>, ids: Arc<dyn IdGenerator>) -> Self {
        Self {
            db: db.clone(),
            crypto,
            ids,
        }
    }
}

/// Map one `auth_users` row into the federated view. Federated users always
/// carry usernames (derived at import); a missing value degrades to empty
/// rather than failing the login that just succeeded.
fn map_federated_user(row: &sqlx::sqlite::SqliteRow) -> StoredUser {
    let username: Option<String> = row.get("username");
    let display: Option<String> = row.get("username_display");
    StoredUser {
        id: row.get("id"),
        display_name: row.get("display_name"),
        role: row.get("role"),
        email: row.get("email"),
        avatar_url: row.get("avatar_url"),
        username: username.unwrap_or_default(),
        username_display: display.unwrap_or_default(),
    }
}

impl FederatedUserStore for SqliteFederatedStore {
    async fn get_provider(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> Result<Option<ProviderBinding>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(
            "SELECT id, user_id, provider, provider_uid FROM auth_providers \
             WHERE provider = ? AND provider_uid = ?",
        )
        .bind(provider)
        .bind(provider_uid)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.map(|row| ProviderBinding {
            id: row.get("id"),
            user_id: row.get("user_id"),
            provider: row.get("provider"),
            provider_uid: row.get("provider_uid"),
        }))
    }

    async fn get_user_by_id(&self, user_id: &str) -> Result<Option<StoredUser>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM auth_users WHERE id = ?"
        ))
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.as_ref().map(map_federated_user))
    }

    async fn get_user_by_email(&self, email: &str) -> Result<Option<StoredUser>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM auth_users WHERE email = ?"
        ))
        .bind(email)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.as_ref().map(map_federated_user))
    }

    async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS} FROM auth_users WHERE username = ?"
        ))
        .bind(username)
        .fetch_optional(pool)
        .await
        .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(row.as_ref().map(map_federated_user))
    }

    async fn has_any_users(&self) -> Result<bool, FederatedError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM auth_users LIMIT 1")
            .fetch_optional(pool)
            .await
            .map_err(|error| store_unavailable(map_sqlx_busy("auth.federated.get", error)))?;
        Ok(found.is_some())
    }

    async fn create_user(&self, user: NewFederatedUser) -> Result<StoredUser, FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let id = self.ids.new_id();
        let created = to_iso(AuthDb::now_unix());
        let stored = StoredUser {
            id: id.clone(),
            display_name: user.display_name.clone(),
            role: user.role.clone(),
            email: user.email.clone(),
            avatar_url: user.avatar_url.clone(),
            username: user.username.clone(),
            username_display: user.username_display.clone(),
        };
        let outcome: Result<(), crate::db::DbError> = lane
            .write(Lane::Foreground, "auth.federated.create", move |tx| {
                tx.execute(
                    "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
                     created_at, username, username_display) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        id,
                        user.display_name,
                        user.email,
                        user.avatar_url,
                        user.role,
                        created,
                        user.username,
                        user.username_display,
                    ],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await;
        match outcome {
            Ok(()) => Ok(stored),
            // Only a uniqueness conflict retries with a fresh username; any
            // other failure must surface, not spin the retry loop.
            Err(error) if is_write_conflict(&error) => Err(FederatedError::UsernameTaken),
            Err(error) => Err(store_unavailable(error)),
        }
    }

    async fn create_provider(
        &self,
        user_id: &str,
        provider: &str,
        provider_uid: &str,
        token_json: &str,
    ) -> Result<ProviderBinding, FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let sealed = self
            .crypto
            .encrypt(token_json)
            .map_err(|error| store_unavailable(format!("cannot seal provider tokens: {error}")))?;
        let (id, user_id, provider, provider_uid) = (
            self.ids.new_id(),
            user_id.to_owned(),
            provider.to_owned(),
            provider_uid.to_owned(),
        );
        let created = to_iso(AuthDb::now_unix());
        let (bid, buser, bprovider, buid) = (
            id.clone(),
            user_id.clone(),
            provider.clone(),
            provider_uid.clone(),
        );
        lane.write(Lane::Foreground, "auth.federated.bind", move |tx| {
            tx.execute(
                "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
                 provider_data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![bid, buser, bprovider, buid, sealed, created],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)?;
        Ok(ProviderBinding {
            id,
            user_id,
            provider,
            provider_uid,
        })
    }

    async fn update_provider_tokens(
        &self,
        binding_id: &str,
        token_json: &str,
    ) -> Result<(), FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth federated store is not wired"));
        };
        let sealed = self
            .crypto
            .encrypt(token_json)
            .map_err(|error| store_unavailable(format!("cannot seal provider tokens: {error}")))?;
        let binding_id = binding_id.to_owned();
        lane.write(Lane::Foreground, "auth.federated.rotate", move |tx| {
            tx.execute(
                "UPDATE auth_providers SET provider_data = ? WHERE id = ?",
                rusqlite::params![sealed, binding_id],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)
    }
}
