//! Federated user import over `auth_users` plus the non-local rows of
//! `auth_providers`.

use std::sync::Arc;

use rusqlite::OptionalExtension as _;

use super::users::read_binding;
use super::{AuthDb, is_write_conflict, op_error, sql, store_unavailable};
use crate::federated::FederatedError;
use crate::federated::users::{
    NewFederatedUser, ProviderBinding, ROLE_ADMIN, ROLE_USER, StoredUser,
};
use crate::times::secs_to_ms;
use droppedneedle_kernel::db::statement::Statement;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::crypto::Crypto;

/// Federated user import over `auth_users` plus the non-local rows of
/// `auth_providers`.
///
/// Every `token_json` is sealed with the deployment key before it reaches
/// `provider_data`, on both insert and rotation. No read path decrypts: the
/// store never returns token material.
#[derive(Clone)]
pub struct FederatedUsers {
    db: AuthDb,
    crypto: Arc<Crypto>,
    ids: Arc<dyn IdGenerator>,
}

impl std::fmt::Debug for FederatedUsers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FederatedUsers")
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl FederatedUsers {
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
fn map_federated_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredUser> {
    let row = sql::UserRow::read(row)?;
    Ok(StoredUser {
        id: row.id,
        display_name: row.display_name,
        role: row.role,
        email: row.email,
        avatar_url: row.avatar_url,
        username: row.username.unwrap_or_default(),
        username_display: row.username_display.unwrap_or_default(),
    })
}

impl FederatedUsers {
    /// One read on the request read pool; a busy database stays busy.
    async fn read<R, F>(&self, op: F) -> Result<R, FederatedError>
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.db
            .db
            .read("auth.federated.get", move |conn| {
                op(conn).map_err(DbError::from)
            })
            .await
            .map_err(store_unavailable)
    }

    /// One user read by a statement taking one text value.
    async fn one_user(
        &self,
        statement: Statement,
        value: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        let value = value.to_owned();
        self.read(move |conn| {
            conn.prepare_cached(statement.text())?
                .query_row([value], map_federated_user)
                .optional()
        })
        .await
    }

    /// Look up a provider binding by `(provider, provider_uid)`.
    pub async fn get_provider(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> Result<Option<ProviderBinding>, FederatedError> {
        let (provider, provider_uid) = (provider.to_owned(), provider_uid.to_owned());
        self.read(move |conn| {
            conn.prepare_cached(sql::PROVIDER_BINDING.text())?
                .query_row([provider, provider_uid], read_binding)
                .optional()
        })
        .await
    }

    /// Look up a user by id.
    pub async fn get_user_by_id(
        &self,
        user_id: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        self.one_user(sql::USER_BY_ID, user_id).await
    }

    /// Look up a user by lowercased email.
    pub async fn get_user_by_email(
        &self,
        email: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        self.one_user(sql::USER_BY_EMAIL, email).await
    }

    /// Look up a user by lowercased username (drives derivation).
    pub async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        self.one_user(sql::USER_BY_USERNAME, username).await
    }

    /// Insert a user and its provider binding in one transaction, making
    /// the user admin when no user exists yet. Maps a uniqueness race (the
    /// username, or the same provider account signing in twice at once) to
    /// [`FederatedError::UsernameTaken`] so the caller looks again.
    pub async fn create_user(&self, user: NewFederatedUser) -> Result<StoredUser, FederatedError> {
        let lane = self.db.lane();
        let id = self.ids.new_id();
        let binding_id = self.ids.new_id();
        let sealed = self.crypto.encrypt(&user.token_json).map_err(|error| {
            FederatedError::StoreUnavailable(format!("cannot seal provider tokens: {error}"))
        })?;
        let created = secs_to_ms(self.db.now_unix());
        let row = user.clone();
        let row_id = id.clone();
        let outcome = lane
            .write(Lane::Foreground, "auth.federated.create", move |tx| {
                let has_users = tx
                    .query_row(sql::ANY_USER.text(), [], |_| Ok(()))
                    .optional()
                    .map_err(op_error)?
                    .is_some();
                let role = if has_users { ROLE_USER } else { ROLE_ADMIN };
                tx.execute(
                    sql::INSERT_USER.text(),
                    rusqlite::params![
                        row_id,
                        row.display_name,
                        row.email,
                        row.avatar_url,
                        role,
                        created,
                        None::<i64>,
                        row.username,
                        row.username_display,
                    ],
                )
                .map_err(op_error)?;
                tx.execute(
                    sql::INSERT_PROVIDER.text(),
                    rusqlite::params![
                        binding_id,
                        row_id,
                        row.provider,
                        row.provider_uid,
                        sealed,
                        created,
                    ],
                )
                .map_err(op_error)?;
                Ok(role)
            })
            .await;
        match outcome {
            Ok(role) => Ok(StoredUser {
                id,
                display_name: user.display_name,
                role: role.to_owned(),
                email: user.email,
                avatar_url: user.avatar_url,
                username: user.username,
                username_display: user.username_display,
            }),
            // Only a uniqueness conflict retries with a fresh username; any
            // other failure must surface, not spin the retry loop.
            Err(error) if is_write_conflict(&error) => Err(FederatedError::UsernameTaken),
            Err(error) => Err(store_unavailable(error)),
        }
    }

    /// Insert a `(provider, provider_uid)` binding with sealed tokens.
    pub async fn create_provider(
        &self,
        user_id: &str,
        provider: &str,
        provider_uid: &str,
        token_json: &str,
    ) -> Result<ProviderBinding, FederatedError> {
        let lane = self.db.lane();
        let sealed = self.crypto.encrypt(token_json).map_err(|error| {
            FederatedError::StoreUnavailable(format!("cannot seal provider tokens: {error}"))
        })?;
        let (id, user_id, provider, provider_uid) = (
            self.ids.new_id(),
            user_id.to_owned(),
            provider.to_owned(),
            provider_uid.to_owned(),
        );
        let created = secs_to_ms(self.db.now_unix());
        let (bid, buser, bprovider, buid) = (
            id.clone(),
            user_id.clone(),
            provider.clone(),
            provider_uid.clone(),
        );
        lane.write(Lane::Foreground, "auth.federated.bind", move |tx| {
            tx.execute(
                sql::INSERT_PROVIDER.text(),
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

    /// Replace the sealed tokens on a binding (called on every login).
    pub async fn update_provider_tokens(
        &self,
        binding_id: &str,
        token_json: &str,
    ) -> Result<(), FederatedError> {
        let lane = self.db.lane();
        let sealed = self.crypto.encrypt(token_json).map_err(|error| {
            FederatedError::StoreUnavailable(format!("cannot seal provider tokens: {error}"))
        })?;
        let binding_id = binding_id.to_owned();
        lane.write(Lane::Foreground, "auth.federated.rotate", move |tx| {
            tx.execute(
                sql::SET_PROVIDER_DATA.text(),
                rusqlite::params![sealed, binding_id],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)
    }
}
