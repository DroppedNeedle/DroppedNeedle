//! Password-recovery codes over `auth_password_recovery_codes`.

use sqlx::Row as _;

use super::{AuthDb, internal, op_error};
use crate::auth::times::{parse_iso, to_iso};
use crate::auth::users::models::RecoveryCode;
use crate::auth::users::stores::{BoxFuture, RecoveryStore, StoreError};
use crate::db::{Lane, map_sqlx_busy};

/// Single-active-code recovery rows over `auth_password_recovery_codes`.
#[derive(Clone, Debug)]
pub struct SqliteRecoveryStore {
    db: AuthDb,
}

impl SqliteRecoveryStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

impl RecoveryStore for SqliteRecoveryStore {
    fn store<'a>(&'a self, code: RecoveryCode) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth recovery store is not wired"));
            };
            lane.write(Lane::Foreground, "auth.recovery.store", move |tx| {
                // Cheap sweep in the same tx (v2 parity): expired rows never
                // accumulate, and no background loop owns this table.
                let now = to_iso(AuthDb::now_unix());
                tx.execute(
                    "DELETE FROM auth_password_recovery_codes WHERE expires_at <= ?",
                    rusqlite::params![now],
                )
                .map_err(op_error)?;
                tx.execute(
                    "INSERT OR REPLACE INTO auth_password_recovery_codes \
                     (user_id, code_hash, created_at, expires_at) VALUES (?, ?, ?, ?)",
                    rusqlite::params![
                        code.user_id,
                        code.code_hash,
                        to_iso(code.created_at),
                        to_iso(code.expires_at),
                    ],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }

    fn find_live_by_hash<'a>(
        &'a self,
        code_hash: &'a str,
        now: i64,
    ) -> BoxFuture<'a, Result<Option<RecoveryCode>, StoreError>> {
        Box::pin(async move {
            let Some((pool, _)) = self.db.live() else {
                return Err(internal("auth recovery store is not wired"));
            };
            let row = sqlx::query(
                "SELECT user_id, code_hash, created_at, expires_at \
                 FROM auth_password_recovery_codes WHERE code_hash = ?",
            )
            .bind(code_hash)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal(map_sqlx_busy("auth.recovery.get", error)))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let expires_raw: String = row.get("expires_at");
            let Some(expires_at) = parse_iso(&expires_raw) else {
                return Ok(None);
            };
            if expires_at <= now {
                return Ok(None);
            }
            let created_raw: String = row.get("created_at");
            Ok(Some(RecoveryCode {
                user_id: row.get("user_id"),
                code_hash: row.get("code_hash"),
                created_at: parse_iso(&created_raw).unwrap_or(0),
                expires_at,
            }))
        })
    }

    fn delete_for_user<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((_, lane)) = self.db.live() else {
                return Err(internal("auth recovery store is not wired"));
            };
            let user_id = user_id.to_owned();
            lane.write(Lane::Foreground, "auth.recovery.delete", move |tx| {
                tx.execute(
                    "DELETE FROM auth_password_recovery_codes WHERE user_id = ?",
                    rusqlite::params![user_id],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(internal)
        })
    }
}
