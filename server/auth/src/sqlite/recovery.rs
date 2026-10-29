//! Password-recovery codes over `auth_password_recovery_codes`.

use rusqlite::OptionalExtension as _;

use super::sql;
use super::{AuthDb, op_error, store_error};
use crate::times::{ms_to_secs, secs_to_ms};
use crate::users::models::RecoveryCode;
use crate::users::stores::StoreError;
use droppedneedle_kernel::db::Lane;

/// Recovery codes over `auth_password_recovery_codes`: one live code per
/// user, and minting replaces the last one.
#[derive(Clone, Debug)]
pub struct RecoveryCodes {
    db: AuthDb,
}

impl RecoveryCodes {
    /// Recovery codes over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }

    /// Store (replacing any prior) one user's code.
    pub async fn store(&self, code: RecoveryCode) -> Result<(), StoreError> {
        let now = secs_to_ms(self.db.now_unix());
        self.db
            .lane()
            .write(Lane::Foreground, "auth.recovery.store", move |tx| {
                // Cheap sweep in the same tx (v2 parity): expired rows never
                // accumulate, and no background loop owns this table.
                tx.execute(
                    sql::DELETE_EXPIRED_RECOVERY_CODES.text(),
                    rusqlite::params![now],
                )
                .map_err(op_error)?;
                tx.execute(
                    sql::STORE_RECOVERY_CODE.text(),
                    rusqlite::params![
                        code.user_id,
                        code.code_hash,
                        secs_to_ms(code.created_at),
                        secs_to_ms(code.expires_at),
                    ],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    /// Find the live row for a code hash. None when unknown or expired.
    pub async fn find_live_by_hash(
        &self,
        code_hash: &str,
        now: i64,
    ) -> Result<Option<RecoveryCode>, StoreError> {
        let code_hash = code_hash.to_owned();
        let row: Option<(String, String, i64, i64)> = self
            .db
            .read("auth.recovery.get", move |conn| {
                conn.prepare_cached(sql::RECOVERY_CODE_BY_HASH.text())?
                    .query_row([code_hash], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .optional()
            })
            .await?;
        let Some((user_id, code_hash, created_ms, expires_ms)) = row else {
            return Ok(None);
        };
        let expires_at = ms_to_secs(expires_ms);
        if expires_at <= now {
            return Ok(None);
        }
        Ok(Some(RecoveryCode {
            user_id,
            code_hash,
            created_at: ms_to_secs(created_ms),
            expires_at,
        }))
    }

    /// Delete one user's code. Used after a reset consumes it.
    pub async fn delete_for_user(&self, user_id: &str) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .lane()
            .write(Lane::Foreground, "auth.recovery.delete", move |tx| {
                tx.execute(
                    sql::DELETE_USER_RECOVERY_CODE.text(),
                    rusqlite::params![user_id],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }
}
