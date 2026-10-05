//! Accounts and local credentials over `auth_users` plus the `local`
//! rows of `auth_providers`, and the native login lookup.

use rusqlite::OptionalExtension as _;
use sqlx::Row as _;

use super::{
    AuthDb, USER_COLUMNS, internal, is_write_conflict, op_error, parse_local_data,
    render_local_data, row_exists,
};
use crate::auth::federated::users::ProviderBinding;
use crate::auth::session::login::{CredentialLookup, LocalCredential, LoginError};
use crate::auth::times::{parse_iso, to_iso};
use crate::auth::users::models::{LocalCredential as UsersLocalCredential, UserRecord};
use crate::auth::users::roles::Role;
use crate::auth::users::stores::{BoxFuture, RoleChange, StoreError, UserDeletion, UserStore};
use crate::db::{Lane, map_sqlx_busy};

/// Account and credential rows over `auth_users` plus the `local` rows of
/// `auth_providers`.
///
/// The local credential lives in `provider_data` as
/// `{"password_hash": ..., "scheme": ...}` (see [`parse_local_data`]); there
/// is no password column and none is needed. Renames sync the local
/// `provider_uid` in the same transaction; deletes cascade through the
/// baseline foreign keys.
#[derive(Clone, Debug)]
pub struct SqliteUserStore {
    db: AuthDb,
}

impl SqliteUserStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

/// Map one `auth_users` row. Unknown roles degrade to least privilege;
/// unparseable display times degrade to 0.
fn map_user(row: &sqlx::sqlite::SqliteRow) -> UserRecord {
    let created_raw: String = row.get("created_at");
    let login_raw: Option<String> = row.get("last_login_at");
    let role_raw: String = row.get("role");
    UserRecord {
        id: row.get("id"),
        username: row.get("username"),
        username_display: row.get("username_display"),
        display_name: row.get("display_name"),
        email: row.get("email"),
        avatar_url: row.get("avatar_url"),
        role: Role::from_stored(&role_raw),
        created_at: parse_iso(&created_raw).unwrap_or(0),
        last_login_at: login_raw.as_deref().and_then(parse_iso),
    }
}

impl UserStore for SqliteUserStore {
    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users WHERE id = ?"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(row.as_ref().map(map_user))
        })
    }

    fn get_by_username<'a>(
        &'a self,
        username: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users WHERE username = ?"
            ))
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(row.as_ref().map(map_user))
        })
    }

    fn get_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<UserRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users WHERE email = ?"
            ))
            .bind(email)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(row.as_ref().map(map_user))
        })
    }

    fn get_by_ids<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<UserRecord>, StoreError>> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql = format!("SELECT {USER_COLUMNS} FROM auth_users WHERE id IN ({placeholders})");
            let mut query = sqlx::query(&sql);
            for id in ids {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(|error| internal(map_sqlx_busy("auth.users.get", error)))?;
            Ok(rows.iter().map(map_user).collect())
        })
    }

    fn insert<'a>(&'a self, user: UserRecord) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome = lane
                .write(Lane::Foreground, "auth.users.insert", move |tx| {
                    insert_user_row(tx, &user)
                })
                .await;
            conflict_or_internal(outcome)
        })
    }

    fn insert_with_local_credential<'a>(
        &'a self,
        user: UserRecord,
        credential: UsersLocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome = lane
                .write(Lane::Foreground, "auth.users.create", move |tx| {
                    insert_user_row(tx, &user)?;
                    insert_local_row(tx, &credential)
                })
                .await;
            conflict_or_internal(outcome)
        })
    }

    fn update_profile<'a>(
        &'a self,
        id: &'a str,
        display_name: Option<&'a str>,
        avatar_url: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, display_name, avatar_url) = (
                id.to_owned(),
                display_name.map(str::to_owned),
                avatar_url.map(str::to_owned),
            );
            lane.write(Lane::Foreground, "auth.users.profile", move |tx| {
                tx.execute(
                    "UPDATE auth_users SET display_name = COALESCE(?, display_name), \
                     avatar_url = COALESCE(?, avatar_url) WHERE id = ?",
                    rusqlite::params![display_name, avatar_url, id],
                )
                .map_err(op_error)?;
                row_exists(tx, "auth_users", &id)
            })
            .await
            .map_err(internal)
        })
    }

    fn update_username<'a>(
        &'a self,
        id: &'a str,
        username: &'a str,
        username_display: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, username, username_display) = (
                id.to_owned(),
                username.to_owned(),
                username_display.to_owned(),
            );
            let outcome: Result<bool, crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.rename", move |tx| {
                    tx.execute(
                        "UPDATE auth_users SET username = ?, username_display = ? WHERE id = ?",
                        rusqlite::params![username, username_display, id],
                    )
                    .map_err(op_error)?;
                    // Same transaction: the local binding tracks the rename.
                    tx.execute(
                        "UPDATE auth_providers SET provider_uid = ? \
                         WHERE user_id = ? AND provider = 'local'",
                        rusqlite::params![username, id],
                    )
                    .map_err(op_error)?;
                    row_exists(tx, "auth_users", &id)
                })
                .await;
            match outcome {
                Ok(exists) => Ok(exists),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn update_email<'a>(
        &'a self,
        id: &'a str,
        email: Option<&'a str>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, email) = (id.to_owned(), email.map(str::to_owned));
            let outcome: Result<bool, crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.email", move |tx| {
                    tx.execute(
                        "UPDATE auth_users SET email = ? WHERE id = ?",
                        rusqlite::params![email, id],
                    )
                    .map_err(op_error)?;
                    row_exists(tx, "auth_users", &id)
                })
                .await;
            match outcome {
                Ok(exists) => Ok(exists),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }

    fn set_role<'a>(
        &'a self,
        id: &'a str,
        role: Role,
    ) -> BoxFuture<'a, Result<RoleChange, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let id = id.to_owned();
            lane.write(Lane::Foreground, "auth.users.role", move |tx| {
                let Some(current) = role_of(tx, &id)? else {
                    return Ok(RoleChange::NotFound);
                };
                if current.is_admin() && !role.is_admin() && admin_count(tx)? <= 1 {
                    return Ok(RoleChange::LastAdmin);
                }
                tx.execute(
                    "UPDATE auth_users SET role = ? WHERE id = ?",
                    rusqlite::params![role.as_str(), id],
                )
                .map_err(op_error)?;
                Ok(RoleChange::Changed)
            })
            .await
            .map_err(internal)
        })
    }

    fn touch_login<'a>(&'a self, id: &'a str, at: i64) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let id = id.to_owned();
            lane.write(Lane::Foreground, "auth.users.touch", move |tx| {
                tx.execute(
                    "UPDATE auth_users SET last_login_at = ? WHERE id = ?",
                    rusqlite::params![to_iso(at), id],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }

    fn delete<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<UserDeletion, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let id = id.to_owned();
            lane.write(Lane::Foreground, "auth.users.delete", move |tx| {
                let Some(current) = role_of(tx, &id)? else {
                    return Ok(UserDeletion::NotFound);
                };
                if current.is_admin() && admin_count(tx)? <= 1 {
                    return Ok(UserDeletion::LastAdmin);
                }
                let mut holders = Vec::new();
                for (table, column, label) in RESTRICTING_HISTORY {
                    let found: Option<i64> = tx
                        .query_row(
                            &format!("SELECT 1 FROM {table} WHERE {column} = ? LIMIT 1"),
                            rusqlite::params![id],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(op_error)?;
                    if found.is_some() {
                        holders.push(*label);
                    }
                }
                if !holders.is_empty() {
                    return Ok(UserDeletion::Referenced(holders));
                }
                tx.execute("DELETE FROM auth_users WHERE id = ?", rusqlite::params![id])
                    .map_err(op_error)?;
                Ok(UserDeletion::Deleted)
            })
            .await
            .map_err(internal)
        })
    }

    fn list<'a>(
        &'a self,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<UserRecord>, u64), StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let rows = sqlx::query(&format!(
                "SELECT {USER_COLUMNS} FROM auth_users ORDER BY created_at ASC, id ASC \
                 LIMIT ? OFFSET ?"
            ))
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.list", error)))?;
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_users")
                .fetch_one(pool)
                .await
                .map_err(|error| internal(map_sqlx_busy("auth.users.list", error)))?;
            Ok((rows.iter().map(map_user).collect(), total.max(0) as u64))
        })
    }

    fn provider_names<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<String>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let rows: Vec<String> = sqlx::query_scalar(
                "SELECT provider FROM auth_providers WHERE user_id = ? \
                 ORDER BY provider ASC",
            )
            .bind(id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.providers", error)))?;
            Ok(rows)
        })
    }

    fn local_credential<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<UsersLocalCredential>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(
                "SELECT id, provider_data FROM auth_providers \
                 WHERE user_id = ? AND provider = 'local'",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.credential", error)))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let data: Option<String> = row.get("provider_data");
            let Some(data) = data else {
                return Ok(None);
            };
            let Some((scheme, hash)) = parse_local_data(&data) else {
                return Ok(None);
            };
            Ok(Some(UsersLocalCredential {
                id: row.get("id"),
                user_id: id.to_owned(),
                scheme,
                hash,
            }))
        })
    }

    fn insert_local_credential<'a>(
        &'a self,
        credential: UsersLocalCredential,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome = lane
                .write(Lane::Foreground, "auth.users.credential", move |tx| {
                    insert_local_row(tx, &credential)
                })
                .await;
            conflict_or_internal(outcome)
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
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, expected_hash, scheme, new_hash, keep) = (
                id.to_owned(),
                expected_hash.to_owned(),
                scheme.to_owned(),
                new_hash.to_owned(),
                keep_session_id.to_owned(),
            );
            lane.write(Lane::Foreground, "auth.users.password", move |tx| {
                if !swap_local_hash(tx, &id, &expected_hash, &scheme, &new_hash)? {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ? AND id != ?",
                    rusqlite::params![id, keep],
                )
                .map_err(op_error)?;
                Ok(true)
            })
            .await
            .map_err(internal)
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
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let (id, expected_hash, scheme, new_hash) = (
                id.to_owned(),
                expected_hash.to_owned(),
                scheme.to_owned(),
                new_hash.to_owned(),
            );
            lane.write(Lane::Foreground, "auth.users.recovery_reset", move |tx| {
                // One transaction: the new hash, no live sessions, no live
                // code. A crash mid-reset retries cleanly (same guard).
                if !swap_local_hash(tx, &id, &expected_hash, &scheme, &new_hash)? {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ?",
                    rusqlite::params![id],
                )
                .map_err(op_error)?;
                tx.execute(
                    "DELETE FROM auth_password_recovery_codes WHERE user_id = ?",
                    rusqlite::params![id],
                )
                .map_err(op_error)?;
                Ok(true)
            })
            .await
            .map_err(internal)
        })
    }

    fn get_provider_binding<'a>(
        &'a self,
        provider: &'a str,
        provider_uid: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderBinding>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let row = sqlx::query(
                "SELECT id, user_id, provider, provider_uid FROM auth_providers \
                 WHERE provider = ? AND provider_uid = ?",
            )
            .bind(provider)
            .bind(provider_uid)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.users.binding", error)))?;
            Ok(row.map(|row| ProviderBinding {
                id: row.get("id"),
                user_id: row.get("user_id"),
                provider: row.get("provider"),
                provider_uid: row.get("provider_uid"),
            }))
        })
    }

    fn insert_provider_binding<'a>(
        &'a self,
        binding: ProviderBinding,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth user store is not wired"));
            };
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.users.bind", move |tx| {
                    tx.execute(
                        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
                         provider_data, created_at) VALUES (?, ?, ?, ?, NULL, ?)",
                        rusqlite::params![
                            binding.id,
                            binding.user_id,
                            binding.provider,
                            binding.provider_uid,
                            to_iso(AuthDb::now_unix()),
                        ],
                    )
                    .map_err(op_error)?;
                    Ok(())
                })
                .await;
            match outcome {
                Ok(()) => Ok(()),
                Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
                Err(error) => Err(internal(error)),
            }
        })
    }
}

/// Tables that keep a user's name on library history with
/// `ON DELETE RESTRICT`, as (table, column, readable name).
const RESTRICTING_HISTORY: &[(&str, &str, &str)] = &[
    (
        "library_artist_reconciliation_dismissals",
        "dismissed_by_user_id",
        "artist reconciliation dismissals",
    ),
    (
        "library_custom_edition_manifests",
        "sealed_by_user_id",
        "custom edition manifests",
    ),
    (
        "library_management_exclusions",
        "excluded_by_user_id",
        "library management exclusions",
    ),
    (
        "library_edition_conversion_jobs",
        "requested_by_user_id",
        "edition conversion jobs",
    ),
];

/// Map a write outcome: constraint conflicts become `Conflict`.
fn conflict_or_internal(outcome: Result<(), crate::db::DbError>) -> Result<(), StoreError> {
    match outcome {
        Ok(()) => Ok(()),
        Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
        Err(error) => Err(internal(error)),
    }
}

/// Insert one `auth_users` row.
fn insert_user_row(
    tx: &rusqlite::Transaction,
    user: &UserRecord,
) -> Result<(), crate::db::OpError> {
    tx.execute(
        "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
         created_at, last_login_at, username, username_display) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

/// Insert the `local` provider row; the binding's uid is the username.
fn insert_local_row(
    tx: &rusqlite::Transaction,
    credential: &UsersLocalCredential,
) -> Result<(), crate::db::OpError> {
    let username: Option<String> = tx
        .query_row(
            "SELECT username FROM auth_users WHERE id = ?",
            rusqlite::params![credential.user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(op_error)?
        .flatten();
    let Some(username) = username else {
        return Err(crate::db::OpError::Abort(
            "cannot set a local password without a username".to_owned(),
        ));
    };
    tx.execute(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
         provider_data, created_at) VALUES (?, ?, 'local', ?, ?, ?)",
        rusqlite::params![
            credential.id,
            credential.user_id,
            username,
            render_local_data(&credential.scheme, &credential.hash),
            to_iso(AuthDb::now_unix()),
        ],
    )
    .map_err(op_error)?;
    Ok(())
}

/// Replace the local hash when it still equals `expected_hash`. Returns
/// false (writing nothing) when the row is gone or the hash moved on.
fn swap_local_hash(
    tx: &rusqlite::Transaction,
    id: &str,
    expected_hash: &str,
    scheme: &str,
    new_hash: &str,
) -> Result<bool, crate::db::OpError> {
    let current: Option<Option<String>> = tx
        .query_row(
            "SELECT provider_data FROM auth_providers WHERE user_id = ? AND provider = 'local'",
            rusqlite::params![id],
            |row| row.get(0),
        )
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
        "UPDATE auth_providers SET provider_data = ? WHERE user_id = ? AND provider = 'local'",
        rusqlite::params![render_local_data(scheme, new_hash), id],
    )
    .map_err(op_error)?;
    Ok(true)
}

/// Current role of one user, or `None` when the row is gone.
fn role_of(tx: &rusqlite::Transaction, id: &str) -> Result<Option<Role>, crate::db::OpError> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT role FROM auth_users WHERE id = ?",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(op_error)?;
    Ok(raw.as_deref().map(Role::from_stored))
}

/// Admin accounts right now, read inside the caller's transaction.
fn admin_count(tx: &rusqlite::Transaction) -> Result<i64, crate::db::OpError> {
    tx.query_row(
        "SELECT COUNT(*) FROM auth_users WHERE role = ?",
        rusqlite::params![Role::Admin.as_str()],
        |row| row.get(0),
    )
    .map_err(op_error)
}

/// Native login lookup over `auth_users` plus the `local` provider row.
///
/// Returns the tagged stored hash (`scheme$hash`) for the session verifier;
/// unknown users, missing local rows, and corrupt documents all read as
/// absent so the caller burns exactly one dummy verify.
#[derive(Clone, Debug)]
pub struct SqliteCredentialLookup {
    db: AuthDb,
}

impl SqliteCredentialLookup {
    /// Lookup over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

impl CredentialLookup for SqliteCredentialLookup {
    async fn local_user(&self, username_lc: &str) -> Result<Option<LocalCredential>, LoginError> {
        let Some((pool, _)) = self.db.live() else {
            return Err(LoginError::Unavailable);
        };
        let row = sqlx::query(
            "SELECT u.id AS id, u.display_name AS display_name, \
             p.provider_data AS provider_data FROM auth_users u \
             LEFT JOIN auth_providers p ON p.user_id = u.id AND p.provider = 'local' \
             WHERE u.username = ?",
        )
        .bind(username_lc)
        .fetch_optional(pool)
        .await
        .map_err(|error| {
            tracing::error!(error = %map_sqlx_busy("auth.login.lookup", error), "login lookup failed");
            LoginError::Unavailable
        })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let data: Option<String> = row.get("provider_data");
        let Some((scheme, hash)) = data.as_deref().and_then(parse_local_data) else {
            return Ok(None);
        };
        Ok(Some(LocalCredential {
            user_id: row.get("id"),
            display_name: row.get("display_name"),
            stored_hash: format!("{scheme}${hash}"),
        }))
    }
}
