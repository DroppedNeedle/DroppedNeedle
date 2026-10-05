//! OIDC PKCE states over `auth_oidc_states`.

use rusqlite::OptionalExtension as _;

use super::{AuthDb, op_error, store_unavailable};
use crate::auth::federated::FederatedError;
use crate::auth::federated::oidc::{OidcStateStore, STATE_TTL_SECS};
use crate::auth::times::{parse_iso, to_iso};
use crate::db::Lane;

/// Short-lived OIDC PKCE states over `auth_oidc_states`.
///
/// Consumption is single-use and atomic: the row is deleted in the same
/// transaction that reads it, and expired rows consume as absent.
#[derive(Clone, Debug)]
pub struct SqliteOidcStateStore {
    db: AuthDb,
}

impl SqliteOidcStateStore {
    /// Adapter over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }
}

impl OidcStateStore for SqliteOidcStateStore {
    async fn store_state(&self, state: &str, code_verifier: &str) -> Result<(), FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth OIDC store is not wired"));
        };
        let (state, code_verifier) = (state.to_owned(), code_verifier.to_owned());
        lane.write(Lane::Foreground, "auth.oidc.store", move |tx| {
            let now = AuthDb::now_unix();
            tx.execute(
                "INSERT OR REPLACE INTO auth_oidc_states \
                 (state, created_at, expires_at, code_verifier) VALUES (?, ?, ?, ?)",
                rusqlite::params![
                    state,
                    to_iso(now),
                    to_iso(now + STATE_TTL_SECS as i64),
                    code_verifier,
                ],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)
    }

    async fn consume_state(&self, state: &str) -> Result<Option<String>, FederatedError> {
        let Some((_, lane)) = self.db.live() else {
            return Err(store_unavailable("auth OIDC store is not wired"));
        };
        let state = state.to_owned();
        lane.write(Lane::Foreground, "auth.oidc.consume", move |tx| {
            let found: Option<(Option<String>, String)> = tx
                .query_row(
                    "SELECT code_verifier, expires_at FROM auth_oidc_states WHERE state = ?",
                    rusqlite::params![state],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(op_error)?;
            tx.execute(
                "DELETE FROM auth_oidc_states WHERE state = ?",
                rusqlite::params![state],
            )
            .map_err(op_error)?;
            let Some((verifier, expires_raw)) = found else {
                return Ok(None);
            };
            if parse_iso(&expires_raw).is_none_or(|at| at <= AuthDb::now_unix()) {
                return Ok(None);
            }
            Ok(verifier)
        })
        .await
        .map_err(store_unavailable)
    }
}
