//! Accounts and local credentials over `auth_users` plus the `local`
//! rows of `auth_providers`, and the sign-in lookup.

use rusqlite::OptionalExtension as _;
use rusqlite::Transaction;

use super::sql;
use super::{
    AuthDb, WriteError, is_write_conflict, op_error, parse_local_data, render_local_data,
    store_error,
};
use crate::federated::users::ProviderBinding;
use crate::session::login::{LocalCredential as LoginCredential, LoginError};
use crate::times::{parse_iso, to_iso};
use crate::users::models::{LocalCredential, UserRecord};
use crate::users::roles::Role;
use crate::users::stores::{RoleChange, StoreError, UserDeletion};
use droppedneedle_kernel::db::statement::{ListLen, Statement};
use droppedneedle_kernel::db::{DbError, Lane};

/// Accounts and their credentials over `auth_users` plus `auth_providers`.
///
/// The local credential lives in the `local` row's `provider_data` as
/// `{"password_hash": ..., "scheme": ...}` (see [`parse_local_data`]); a
/// missing scheme reads as `bcrypt` (v2 rows predate it). Renames sync the
/// local `provider_uid` in the same transaction; deletes cascade through
/// the baseline foreign keys.
#[derive(Clone, Debug)]
pub struct Users {
    db: AuthDb,
}

/// Map one `auth_users` row read with [`sql::USER_COLUMNS`]. Unknown roles
/// degrade to least privilege; unparseable display times degrade to 0.
fn map_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserRecord> {
    let created_raw: String = row.get(5)?;
    let login_raw: Option<String> = row.get(6)?;
    let role_raw: String = row.get(4)?;
    Ok(UserRecord {
        id: row.get(0)?,
        display_name: row.get(1)?,
        email: row.get(2)?,
        avatar_url: row.get(3)?,
        role: Role::from_stored(&role_raw),
        created_at: parse_iso(&created_raw).unwrap_or(0),
        last_login_at: login_raw.as_deref().and_then(parse_iso),
        username: row.get(7)?,
        username_display: row.get(8)?,
    })
}

/// Tables that keep a user's name on library history with
/// `ON DELETE RESTRICT`, as (statement, readable name).
const RESTRICTING_HISTORY: [(Statement, &str); 3] = [
    (
        sql::HELD_BY_RECONCILIATION_DISMISSALS,
        "artist reconciliation dismissals",
    ),
    (sql::HELD_BY_EDITION_MANIFESTS, "custom edition manifests"),
    (
        sql::HELD_BY_MANAGEMENT_EXCLUSIONS,
        "library management exclusions",
    ),
];

/// Discovery batches, which have no foreign key (the tables predate it),
/// deleted with the account as in v2. The batch items go first; v2 left
/// the batches behind. Discover's other per-user rows go with the account
/// by their foreign keys.
const DISCOVERY_ROWS: [Statement; 2] = [
    sql::DELETE_DISCOVERY_BATCH_ITEMS,
    sql::DELETE_DISCOVERY_BATCHES,
];

/// A constraint conflict is the caller's `Conflict`; the rest as
/// [`store_error`].
fn conflict_or_store<R>(outcome: Result<R, WriteError>) -> Result<R, StoreError> {
    match outcome {
        Ok(value) => Ok(value),
        Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
        Err(error) => Err(store_error(error)),
    }
}

impl Users {
    /// Accounts over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }

    /// One user read by a statement taking one text value.
    async fn one_user(
        &self,
        name: &'static str,
        statement: Statement,
        value: &str,
    ) -> Result<Option<UserRecord>, StoreError> {
        let value = value.to_owned();
        self.db
            .read(name, move |conn| {
                conn.prepare_cached(statement.text())?
                    .query_row([value], map_user)
                    .optional()
            })
            .await
    }

    /// Fetch one user by id. None is absence, never failure.
    pub async fn get_by_id(&self, id: &str) -> Result<Option<UserRecord>, StoreError> {
        self.one_user("auth.users.get", sql::USER_BY_ID, id).await
    }

    /// Fetch one user by lowercased username.
    pub async fn get_by_username(&self, username: &str) -> Result<Option<UserRecord>, StoreError> {
        self.one_user("auth.users.get", sql::USER_BY_USERNAME, username)
            .await
    }

    /// Fetch one user by lowercased email.
    pub async fn get_by_email(&self, email: &str) -> Result<Option<UserRecord>, StoreError> {
        self.one_user("auth.users.get", sql::USER_BY_EMAIL, email)
            .await
    }

    /// Fetch several users by id (admin app-password owner enrichment).
    pub async fn get_by_ids(&self, ids: &[String]) -> Result<Vec<UserRecord>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ids.to_vec();
        self.db
            .read("auth.users.get", move |conn| {
                conn.prepare_cached(&sql::users_by_ids(ListLen::new(ids.len())))?
                    .query_map(rusqlite::params_from_iter(ids.iter()), map_user)?
                    .collect()
            })
            .await
    }

    /// Insert a user row. Conflict when the id, username, or email is taken.
    pub async fn insert(&self, user: UserRecord) -> Result<(), StoreError> {
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.insert", move |tx| {
                Self::insert_user_in(tx, &user)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// Insert a user row and its local credential in one transaction.
    /// Conflict when the id, username, or email is taken; nothing is
    /// written then.
    pub async fn insert_with_local_credential(
        &self,
        user: UserRecord,
        credential: LocalCredential,
    ) -> Result<(), StoreError> {
        let created_at = self.db.now_unix();
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.create", move |tx| {
                Self::insert_user_in(tx, &user)?;
                Self::insert_local_credential_in(tx, &credential, created_at)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// First-run setup: insert the user and its local credential only when
    /// no user exists yet, decided inside the same transaction. `Ok(false)`
    /// when any user (local or federated) is already there; nothing is
    /// written then.
    pub async fn insert_first_user(
        &self,
        user: UserRecord,
        credential: LocalCredential,
    ) -> Result<bool, StoreError> {
        let created_at = self.db.now_unix();
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.create_first", move |tx| {
                let any: Option<i64> = tx
                    .query_row(sql::ANY_USER.text(), [], |row| row.get(0))
                    .optional()
                    .map_err(op_error)?;
                if any.is_some() {
                    return Ok(false);
                }
                Self::insert_user_in(tx, &user)?;
                Self::insert_local_credential_in(tx, &credential, created_at)?;
                Ok(true)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// Update display name and/or avatar URL. None fields stay untouched.
    /// False for an unknown id.
    pub async fn update_profile(
        &self,
        id: &str,
        display_name: Option<&str>,
        avatar_url: Option<&str>,
    ) -> Result<bool, StoreError> {
        let (id, display_name, avatar_url) = (
            id.to_owned(),
            display_name.map(str::to_owned),
            avatar_url.map(str::to_owned),
        );
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.profile", move |tx| {
                tx.execute(
                    sql::UPDATE_PROFILE.text(),
                    rusqlite::params![display_name, avatar_url, id],
                )
                .map_err(op_error)?;
                user_exists(tx, &id)
            })
            .await
            .map_err(store_error)
    }

    /// Rename a user, syncing the local `provider_uid` in the same write.
    /// Conflict when the username is taken. Returns false for unknown ids.
    pub async fn update_username(
        &self,
        id: &str,
        username: &str,
        username_display: &str,
    ) -> Result<bool, StoreError> {
        let (id, username, username_display) = (
            id.to_owned(),
            username.to_owned(),
            username_display.to_owned(),
        );
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.rename", move |tx| {
                tx.execute(
                    sql::UPDATE_USERNAME.text(),
                    rusqlite::params![username, username_display, id],
                )
                .map_err(op_error)?;
                // Same transaction: the local binding tracks the rename.
                tx.execute(sql::SET_LOCAL_UID.text(), rusqlite::params![username, id])
                    .map_err(op_error)?;
                user_exists(tx, &id)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// Set or clear the email. Conflict when another account holds it.
    /// Returns false for unknown ids.
    pub async fn update_email(&self, id: &str, email: Option<&str>) -> Result<bool, StoreError> {
        let (id, email) = (id.to_owned(), email.map(str::to_owned));
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.email", move |tx| {
                tx.execute(sql::UPDATE_EMAIL.text(), rusqlite::params![email, id])
                    .map_err(op_error)?;
                user_exists(tx, &id)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// Set the role, refusing to demote the last admin. The admin count is
    /// read inside the same write transaction as the update.
    pub async fn set_role(&self, id: &str, role: Role) -> Result<RoleChange, StoreError> {
        let id = id.to_owned();
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.role", move |tx| {
                let Some(current) = role_of(tx, &id)? else {
                    return Ok(RoleChange::NotFound);
                };
                if current.is_admin() && !role.is_admin() && admin_count(tx)? <= 1 {
                    return Ok(RoleChange::LastAdmin);
                }
                tx.execute(
                    sql::UPDATE_ROLE.text(),
                    rusqlite::params![role.as_str(), id],
                )
                .map_err(op_error)?;
                Ok(RoleChange::Changed)
            })
            .await
            .map_err(store_error)
    }

    /// Stamp the last-login time.
    pub async fn touch_login(&self, id: &str, at: i64) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.touch", move |tx| {
                tx.execute(sql::TOUCH_LOGIN.text(), rusqlite::params![to_iso(at), id])
                    .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    /// Delete a user, refusing the last admin and users that library
    /// history still names. Cascades to providers, tokens, codes, app
    /// passwords, and connections via the baseline foreign keys.
    pub async fn delete(&self, id: &str) -> Result<UserDeletion, StoreError> {
        let id = id.to_owned();
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.delete", move |tx| {
                let Some(current) = role_of(tx, &id)? else {
                    return Ok(UserDeletion::NotFound);
                };
                if current.is_admin() && admin_count(tx)? <= 1 {
                    return Ok(UserDeletion::LastAdmin);
                }
                let mut holders = Vec::new();
                for (statement, label) in RESTRICTING_HISTORY {
                    let found: Option<i64> = tx
                        .query_row(statement.text(), rusqlite::params![id], |row| row.get(0))
                        .optional()
                        .map_err(op_error)?;
                    if found.is_some() {
                        holders.push(label);
                    }
                }
                if !holders.is_empty() {
                    return Ok(UserDeletion::Referenced(holders));
                }
                for statement in DISCOVERY_ROWS {
                    tx.execute(statement.text(), rusqlite::params![id])
                        .map_err(op_error)?;
                }
                tx.execute(sql::DELETE_USER.text(), rusqlite::params![id])
                    .map_err(op_error)?;
                Ok(UserDeletion::Deleted)
            })
            .await
            .map_err(store_error)
    }

    /// One page of users (creation order, id tiebreak) plus the total count.
    pub async fn list(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<UserRecord>, u64), StoreError> {
        let (limit, offset) = (
            i64::try_from(limit).unwrap_or(i64::MAX),
            i64::try_from(offset).unwrap_or(i64::MAX),
        );
        let rows = self
            .db
            .read("auth.users.list", move |conn| {
                conn.prepare_cached(sql::USER_PAGE.text())?
                    .query_map(rusqlite::params![limit, offset], map_user)?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await?;
        let total: i64 = self
            .db
            .read("auth.users.list", |conn| {
                conn.prepare_cached(sql::USER_COUNT.text())?
                    .query_row([], |row| row.get(0))
            })
            .await?;
        Ok((rows, total.max(0) as u64))
    }

    /// Bound provider names for one user, e.g. `["local"]`.
    pub async fn provider_names(&self, id: &str) -> Result<Vec<String>, StoreError> {
        let id = id.to_owned();
        self.db
            .read("auth.users.providers", move |conn| {
                conn.prepare_cached(sql::USER_PROVIDERS.text())?
                    .query_map([id], |row| row.get(0))?
                    .collect()
            })
            .await
    }

    /// The local credential, when the account has one. A corrupt document
    /// reads as none.
    pub async fn local_credential(&self, id: &str) -> Result<Option<LocalCredential>, StoreError> {
        let user_id = id.to_owned();
        let row: Option<(String, Option<String>)> = self
            .db
            .read("auth.users.credential", move |conn| {
                conn.prepare_cached(sql::LOCAL_CREDENTIAL.text())?
                    .query_row([user_id], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()
            })
            .await?;
        let Some((credential_id, Some(data))) = row else {
            return Ok(None);
        };
        Ok(
            parse_local_data(&data).map(|(scheme, hash)| LocalCredential {
                id: credential_id,
                user_id: id.to_owned(),
                scheme,
                hash,
            }),
        )
    }

    /// The stored credential for a lowercased username, for the sign-in to
    /// verify. Unknown users, missing local rows and corrupt documents all
    /// read as absent so the caller burns exactly one dummy verify.
    pub async fn login_credential(
        &self,
        username_lc: &str,
    ) -> Result<Option<LoginCredential>, LoginError> {
        let username = username_lc.to_owned();
        let row: Option<(String, String, Option<String>)> = self
            .db
            .db
            .read("auth.login.lookup", move |conn| {
                conn.prepare_cached(sql::LOGIN_CREDENTIAL.text())
                    .and_then(|mut statement| {
                        statement
                            .query_row([username], |row| {
                                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                            })
                            .optional()
                    })
                    .map_err(DbError::from)
            })
            .await
            .map_err(|error| match error {
                DbError::Busy { .. } => LoginError::Busy,
                error => {
                    tracing::error!(%error, "login lookup failed");
                    LoginError::Unavailable
                }
            })?;
        let Some((user_id, display_name, data)) = row else {
            return Ok(None);
        };
        let Some((scheme, hash)) = data.as_deref().and_then(parse_local_data) else {
            return Ok(None);
        };
        Ok(Some(LoginCredential {
            user_id,
            display_name,
            password: crate::passwords::StoredPassword { scheme, hash },
        }))
    }

    /// Insert the local credential. Conflict when one already exists or the
    /// `(local, username)` binding is taken.
    pub async fn insert_local_credential(
        &self,
        credential: LocalCredential,
    ) -> Result<(), StoreError> {
        let created_at = self.db.now_unix();
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.credential", move |tx| {
                Self::insert_local_credential_in(tx, &credential, created_at)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// Replace the local hash and revoke every other session of the user,
    /// in one transaction. Guarded by the expected current hash so a
    /// concurrent change fails instead of silently winning; returns false
    /// (and writes nothing) when the guard mismatches. `keep_session_id`
    /// is the session making the change, which stays live.
    pub async fn change_local_hash(
        &self,
        id: &str,
        expected_hash: &str,
        scheme: &str,
        new_hash: &str,
        keep_session_id: &str,
    ) -> Result<bool, StoreError> {
        let (id, expected_hash, scheme, new_hash, keep) = (
            id.to_owned(),
            expected_hash.to_owned(),
            scheme.to_owned(),
            new_hash.to_owned(),
            keep_session_id.to_owned(),
        );
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.password", move |tx| {
                if !swap_local_hash(tx, &id, &expected_hash, &scheme, &new_hash)? {
                    return Ok(false);
                }
                tx.execute(
                    sql::REVOKE_OTHER_SESSIONS.text(),
                    rusqlite::params![id, keep],
                )
                .map_err(op_error)?;
                Ok(true)
            })
            .await
            .map_err(store_error)
    }

    /// Complete a recovery reset atomically: the guarded hash replace plus
    /// revoking every session of the user plus deleting the consumed
    /// recovery code, in one transaction (v2 parity). Returns false when
    /// the guard mismatches (nothing is revoked or deleted then).
    pub async fn complete_recovery_reset(
        &self,
        id: &str,
        expected_hash: &str,
        scheme: &str,
        new_hash: &str,
    ) -> Result<bool, StoreError> {
        let (id, expected_hash, scheme, new_hash) = (
            id.to_owned(),
            expected_hash.to_owned(),
            scheme.to_owned(),
            new_hash.to_owned(),
        );
        self.db
            .lane()
            .write(Lane::Foreground, "auth.users.recovery_reset", move |tx| {
                // One transaction: the new hash, no live sessions, no live
                // code. A crash mid-reset retries cleanly (same guard).
                if !swap_local_hash(tx, &id, &expected_hash, &scheme, &new_hash)? {
                    return Ok(false);
                }
                tx.execute(sql::REVOKE_USER_SESSIONS.text(), rusqlite::params![id])
                    .map_err(op_error)?;
                tx.execute(sql::DELETE_USER_RECOVERY_CODE.text(), rusqlite::params![id])
                    .map_err(op_error)?;
                Ok(true)
            })
            .await
            .map_err(store_error)
    }

    /// Complete a recovery reset for an account with no local password:
    /// consume the code, insert the credential and revoke every session of
    /// the user, in one transaction. The code is the only authorisation
    /// here, so it is checked again inside the transaction. Returns false
    /// (and writes nothing) when the code with `code_hash` is gone or
    /// expired at `now`, the account already has a local credential, or it
    /// has no username to sign in with.
    pub async fn complete_recovery_first_password(
        &self,
        credential: LocalCredential,
        code_hash: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let created_at = self.db.now_unix();
        let code_hash = code_hash.to_owned();
        self.db
            .lane()
            .write(
                Lane::Foreground,
                "auth.users.recovery_first_password",
                move |tx| {
                    let id = credential.user_id.as_str();
                    let ready: bool = tx
                        .query_row(
                            sql::FIRST_PASSWORD_READY.text(),
                            rusqlite::params![id],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(op_error)?
                        .unwrap_or(false);
                    if !ready {
                        return Ok(false);
                    }
                    // The code is the only authorisation: no live row for
                    // it, no password.
                    let consumed = tx
                        .execute(
                            sql::CONSUME_RECOVERY_CODE.text(),
                            rusqlite::params![id, code_hash, to_iso(now)],
                        )
                        .map_err(op_error)?;
                    if consumed == 0 {
                        return Ok(false);
                    }
                    Self::insert_local_credential_in(tx, &credential, created_at)?;
                    tx.execute(sql::REVOKE_USER_SESSIONS.text(), rusqlite::params![id])
                        .map_err(op_error)?;
                    Ok(true)
                },
            )
            .await
            .map_err(store_error)
    }

    /// Fetch one sign-in binding by `(provider, provider_uid)`. None is
    /// absence, never failure.
    pub async fn get_provider_binding(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> Result<Option<ProviderBinding>, StoreError> {
        let (provider, provider_uid) = (provider.to_owned(), provider_uid.to_owned());
        self.db
            .read("auth.users.binding", move |conn| {
                conn.prepare_cached(sql::PROVIDER_BINDING.text())?
                    .query_row([provider, provider_uid], |row| {
                        Ok(ProviderBinding {
                            id: row.get(0)?,
                            user_id: row.get(1)?,
                            provider: row.get(2)?,
                            provider_uid: row.get(3)?,
                        })
                    })
                    .optional()
            })
            .await
    }

    /// Insert a pre-linked sign-in binding: `provider_data` stays NULL (a
    /// login identity, not a credential store; the first SSO login seals the
    /// real tokens). Conflict when the `(provider, provider_uid)` pair is
    /// taken.
    pub async fn insert_provider_binding(
        &self,
        binding: ProviderBinding,
    ) -> Result<(), StoreError> {
        let created_at = self.db.now_unix();
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.users.bind", move |tx| {
                Self::insert_provider_binding_in(tx, &binding, None, created_at)
            })
            .await;
        conflict_or_store(outcome)
    }

    /// Insert one `auth_users` row inside the caller's transaction. A taken
    /// id, username or email is [`WriteError::Conflict`].
    pub fn insert_user_in(tx: &Transaction<'_>, user: &UserRecord) -> Result<(), WriteError> {
        tx.execute(
            sql::INSERT_USER.text(),
            rusqlite::params![
                user.id,
                user.display_name,
                user.email,
                user.avatar_url,
                user.role.as_str(),
                to_iso(user.created_at),
                user.last_login_at.map(to_iso),
                user.username,
                user.username_display,
            ],
        )
        .map_err(op_error)?;
        Ok(())
    }

    /// Insert the `local` provider row inside the caller's transaction; the
    /// binding's uid is the account's username, read through `tx`. An
    /// account with no username fails; a taken binding is
    /// [`WriteError::Conflict`].
    pub fn insert_local_credential_in(
        tx: &Transaction<'_>,
        credential: &LocalCredential,
        created_at: i64,
    ) -> Result<(), WriteError> {
        let username: Option<String> = tx
            .query_row(
                sql::USER_USERNAME.text(),
                rusqlite::params![credential.user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(op_error)?
            .flatten();
        let Some(username) = username else {
            return Err(WriteError::Db(DbError::WriteFailed {
                operation: "auth.users.local_row".to_owned(),
                cause: "cannot set a local password without a username".to_owned(),
            }));
        };
        tx.execute(
            sql::INSERT_PROVIDER.text(),
            rusqlite::params![
                credential.id,
                credential.user_id,
                "local",
                username,
                render_local_data(&credential.scheme, &credential.hash),
                to_iso(created_at),
            ],
        )
        .map_err(op_error)?;
        Ok(())
    }

    /// Insert one sign-in binding inside the caller's transaction, with its
    /// sealed token document when there is one. A taken
    /// `(provider, provider_uid)` pair is [`WriteError::Conflict`].
    pub fn insert_provider_binding_in(
        tx: &Transaction<'_>,
        binding: &ProviderBinding,
        provider_data: Option<&str>,
        created_at: i64,
    ) -> Result<(), WriteError> {
        tx.execute(
            sql::INSERT_PROVIDER.text(),
            rusqlite::params![
                binding.id,
                binding.user_id,
                binding.provider,
                binding.provider_uid,
                provider_data,
                to_iso(created_at),
            ],
        )
        .map_err(op_error)?;
        Ok(())
    }
}

/// True when the account exists. Used after updates that may legitimately
/// change nothing, so an unchanged row still reads as found.
fn user_exists(tx: &Transaction<'_>, id: &str) -> Result<bool, WriteError> {
    let found: Option<i64> = tx
        .query_row(sql::USER_EXISTS.text(), rusqlite::params![id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(op_error)?;
    Ok(found.is_some())
}

/// Replace the local hash when it still equals `expected_hash`. Returns
/// false (writing nothing) when the row is gone or the hash moved on.
fn swap_local_hash(
    tx: &Transaction<'_>,
    id: &str,
    expected_hash: &str,
    scheme: &str,
    new_hash: &str,
) -> Result<bool, WriteError> {
    let current: Option<Option<String>> = tx
        .query_row(sql::LOCAL_DATA.text(), rusqlite::params![id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(op_error)?;
    let Some(Some(data)) = current else {
        return Ok(false);
    };
    let Some((_, hash)) = parse_local_data(&data) else {
        return Ok(false);
    };
    if hash != expected_hash {
        return Ok(false);
    }
    tx.execute(
        sql::SET_LOCAL_DATA.text(),
        rusqlite::params![render_local_data(scheme, new_hash), id],
    )
    .map_err(op_error)?;
    Ok(true)
}

/// Current role of one user, or `None` when the row is gone.
fn role_of(tx: &Transaction<'_>, id: &str) -> Result<Option<Role>, WriteError> {
    let raw: Option<String> = tx
        .query_row(sql::USER_ROLE.text(), rusqlite::params![id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(op_error)?;
    Ok(raw.as_deref().map(Role::from_stored))
}

/// Admin accounts right now, read inside the caller's transaction.
fn admin_count(tx: &Transaction<'_>) -> Result<i64, WriteError> {
    tx.query_row(
        sql::ROLE_COUNT.text(),
        rusqlite::params![Role::Admin.as_str()],
        |row| row.get(0),
    )
    .map_err(op_error)
}
