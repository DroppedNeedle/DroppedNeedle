//! App-password rows over `connect_app_passwords`.

use rusqlite::OptionalExtension as _;
use rusqlite::Transaction;

use super::sql;
use super::{AuthDb, WriteError, is_write_conflict, op_error, store_error};
use crate::times::{parse_iso, to_iso};
use crate::users::models::AppPasswordRecord;
use crate::users::stores::StoreError;
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::db::statement::Statement;

/// App-password rows over `connect_app_passwords`.
///
/// Verification stays decrypt-free: `secret_sha256` resolves the row and the
/// touch stamps last use. `secret_encrypted` round-trips untouched so exports
/// can re-encrypt it under a fresh key.
#[derive(Clone, Debug)]
pub struct AppPasswords {
    db: AuthDb,
}

/// Map one row read with [`sql::APP_PASSWORD_COLUMNS`].
fn map_app_password(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppPasswordRecord> {
    let created_raw: String = row.get(5)?;
    let used_raw: Option<String> = row.get(6)?;
    Ok(AppPasswordRecord {
        id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        secret_sha256: row.get(3)?,
        secret_encrypted: row.get(4)?,
        created_at: parse_iso(&created_raw).unwrap_or(0),
        last_used_at: used_raw.as_deref().and_then(parse_iso),
        last_client: row.get(7)?,
    })
}

impl AppPasswords {
    /// App passwords over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }

    /// Insert a row unless the owner already holds `max_active` live rows;
    /// the count and the insert share one transaction. Returns false when
    /// the cap refused it. Conflict when the id or secret hash is taken.
    pub async fn insert_capped(
        &self,
        row: AppPasswordRecord,
        max_active: u64,
    ) -> Result<bool, StoreError> {
        let cap = i64::try_from(max_active).unwrap_or(i64::MAX);
        let outcome = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.app_passwords.insert", move |tx| {
                let active: i64 = tx
                    .query_row(
                        sql::ACTIVE_APP_PASSWORD_COUNT.text(),
                        rusqlite::params![row.user_id],
                        |found| found.get(0),
                    )
                    .map_err(op_error)?;
                if active >= cap {
                    return Ok(false);
                }
                Self::insert_in(tx, &row, false)?;
                Ok(true)
            })
            .await;
        match outcome {
            Ok(inserted) => Ok(inserted),
            Err(error) if is_write_conflict(&error) => Err(StoreError::Conflict),
            Err(error) => Err(store_error(error)),
        }
    }

    /// Insert one row inside the caller's transaction, with no cap. A taken
    /// id or secret hash is [`WriteError::Conflict`].
    pub fn insert_in(
        tx: &Transaction<'_>,
        row: &AppPasswordRecord,
        revoked: bool,
    ) -> Result<(), WriteError> {
        tx.execute(
            sql::INSERT_APP_PASSWORD.text(),
            rusqlite::params![
                row.id,
                row.user_id,
                row.name,
                row.secret_sha256,
                row.secret_encrypted,
                to_iso(row.created_at),
                row.last_used_at.map(to_iso),
                row.last_client,
                revoked,
            ],
        )
        .map_err(op_error)?;
        Ok(())
    }

    /// One row read by a statement taking one text value.
    async fn one(
        &self,
        name: &'static str,
        statement: Statement,
        value: &str,
    ) -> Result<Option<AppPasswordRecord>, StoreError> {
        let value = value.to_owned();
        self.db
            .read(name, move |conn| {
                conn.prepare_cached(statement.text())?
                    .query_row([value], map_app_password)
                    .optional()
            })
            .await
    }

    /// Fetch one row by id, regardless of revocation.
    pub async fn get_by_id(&self, id: &str) -> Result<Option<AppPasswordRecord>, StoreError> {
        self.one("auth.app_passwords.get", sql::APP_PASSWORD_BY_ID, id)
            .await
    }

    /// Fetch the live row for a secret hash. None when unknown or revoked.
    pub async fn get_active_by_sha256(
        &self,
        secret_sha256: &str,
    ) -> Result<Option<AppPasswordRecord>, StoreError> {
        self.one(
            "auth.app_passwords.get",
            sql::ACTIVE_APP_PASSWORD_BY_SHA256,
            secret_sha256,
        )
        .await
    }

    /// Live rows for one user: oldest first, id tiebreak.
    pub async fn list_active_by_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<AppPasswordRecord>, StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .read("auth.app_passwords.list", move |conn| {
                conn.prepare_cached(sql::ACTIVE_APP_PASSWORDS_OF_USER.text())?
                    .query_map([user_id], map_app_password)?
                    .collect()
            })
            .await
    }

    /// Every live row across users: owner, then age, then id (admin oversight).
    pub async fn list_all_active(&self) -> Result<Vec<AppPasswordRecord>, StoreError> {
        self.db
            .read("auth.app_passwords.list", |conn| {
                conn.prepare_cached(sql::ACTIVE_APP_PASSWORDS.text())?
                    .query_map([], map_app_password)?
                    .collect()
            })
            .await
    }

    /// Soft-revoke. Returns false for unknown or already-revoked ids.
    pub async fn revoke(&self, id: &str) -> Result<bool, StoreError> {
        let id = id.to_owned();
        let changed: usize = self
            .db
            .lane()
            .write(Lane::Foreground, "auth.app_passwords.revoke", move |tx| {
                tx.execute(sql::REVOKE_APP_PASSWORD.text(), rusqlite::params![id])
                    .map_err(op_error)
            })
            .await
            .map_err(store_error)?;
        Ok(changed > 0)
    }

    /// Stamp last use after a verification.
    pub async fn touch(
        &self,
        secret_sha256: &str,
        last_used_at: i64,
        last_client: Option<&str>,
    ) -> Result<(), StoreError> {
        let (secret_sha256, last_client) =
            (secret_sha256.to_owned(), last_client.map(str::to_owned));
        self.db
            .lane()
            .write(Lane::Foreground, "auth.app_passwords.touch", move |tx| {
                tx.execute(
                    sql::TOUCH_APP_PASSWORD.text(),
                    rusqlite::params![to_iso(last_used_at), last_client, secret_sha256],
                )
                .map_err(op_error)?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }
}
