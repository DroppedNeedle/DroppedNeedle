//! App-password rows over `connect_app_passwords`.

use sqlx::Row as _;

use super::{AuthDb, internal, is_write_conflict, op_error};
use crate::auth::times::{parse_iso, to_iso};
use crate::auth::users::models::AppPasswordRecord;
use crate::auth::users::stores::{AppPasswordStore, BoxFuture, StoreError};
use crate::db::{Lane, map_sqlx_busy};

/// App-password rows over `connect_app_passwords`.
///
/// Verification stays decrypt-free: `secret_sha256` resolves the row and the
/// touch stamps last use. `secret_encrypted` round-trips untouched so exports
/// can re-encrypt it under a fresh key.
#[derive(Clone, Debug)]
pub struct SqliteAppPasswordStore {
    db: AuthDb,
}

impl SqliteAppPasswordStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

/// Columns read for every app-password row.
const APP_PASSWORD_COLUMNS: &str = "id, user_id, name, secret_sha256, secret_encrypted, \
    created_at, last_used_at, last_client";

fn map_app_password(row: &sqlx::sqlite::SqliteRow) -> AppPasswordRecord {
    let created_raw: String = row.get("created_at");
    let used_raw: Option<String> = row.get("last_used_at");
    AppPasswordRecord {
        id: row.get("id"),
        user_id: row.get("user_id"),
        name: row.get("name"),
        secret_sha256: row.get("secret_sha256"),
        secret_encrypted: row.get("secret_encrypted"),
        created_at: parse_iso(&created_raw).unwrap_or(0),
        last_used_at: used_raw.as_deref().and_then(parse_iso),
        last_client: row.get("last_client"),
    }
}

impl AppPasswordStore for SqliteAppPasswordStore {
    fn insert<'a>(&'a self, row: AppPasswordRecord) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let outcome: Result<(), crate::db::DbError> = lane
                .write(Lane::Foreground, "auth.app_passwords.insert", move |tx| {
                    tx.execute(
                        "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256, \
                         secret_encrypted, created_at, last_used_at, last_client, revoked) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)",
                        rusqlite::params![
                            row.id,
                            row.user_id,
                            row.name,
                            row.secret_sha256,
                            row.secret_encrypted,
                            to_iso(row.created_at),
                            row.last_used_at.map(to_iso),
                            row.last_client,
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

    fn get_by_id<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords WHERE id = ?"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.get", error)))?;
            Ok(row.as_ref().map(map_app_password))
        })
    }

    fn get_active_by_sha256<'a>(
        &'a self,
        secret_sha256: &'a str,
    ) -> BoxFuture<'a, Result<Option<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let row = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords \
                 WHERE secret_sha256 = ? AND revoked = 0"
            ))
            .bind(secret_sha256)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.get", error)))?;
            Ok(row.as_ref().map(map_app_password))
        })
    }

    fn list_active_by_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let rows = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords \
                 WHERE user_id = ? AND revoked = 0 ORDER BY created_at ASC, id ASC"
            ))
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.list", error)))?;
            Ok(rows.iter().map(map_app_password).collect())
        })
    }

    fn count_active_by_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let total: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM connect_app_passwords WHERE user_id = ? AND revoked = 0",
            )
            .bind(user_id)
            .fetch_one(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.count", error)))?;
            Ok(total.max(0) as u64)
        })
    }

    fn list_all_active<'a>(&'a self) -> BoxFuture<'a, Result<Vec<AppPasswordRecord>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let rows = sqlx::query(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM connect_app_passwords \
                 WHERE revoked = 0 ORDER BY user_id ASC, created_at ASC, id ASC"
            ))
            .fetch_all(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.app_passwords.list", error)))?;
            Ok(rows.iter().map(map_app_password).collect())
        })
    }

    fn revoke<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let id = id.to_owned();
            let changed: usize = lane
                .write(Lane::Foreground, "auth.app_passwords.revoke", move |tx| {
                    tx.execute(
                        "UPDATE connect_app_passwords SET revoked = 1 \
                         WHERE id = ? AND revoked = 0",
                        rusqlite::params![id],
                    )
                    .map_err(op_error)
                })
                .await
                .map_err(internal)?;
            Ok(changed > 0)
        })
    }

    fn touch<'a>(
        &'a self,
        secret_sha256: &'a str,
        last_used_at: i64,
        last_client: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth app-password store is not wired"));
            };
            let (secret_sha256, last_client) =
                (secret_sha256.to_owned(), last_client.map(str::to_owned));
            lane.write(Lane::Foreground, "auth.app_passwords.touch", move |tx| {
                tx.execute(
                    "UPDATE connect_app_passwords SET last_used_at = ?, \
                     last_client = COALESCE(?, last_client) WHERE secret_sha256 = ?",
                    rusqlite::params![to_iso(last_used_at), last_client, secret_sha256],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }
}
