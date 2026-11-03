//! OIDC PKCE states over `auth_oidc_states`.

use rusqlite::OptionalExtension as _;

use super::{AuthDb, op_error, sql, store_unavailable};
use crate::federated::FederatedError;
use crate::federated::oidc::STATE_TTL_SECS;
use crate::times::{ms_to_secs, secs_to_ms};
use droppedneedle_kernel::db::Lane;

/// Short-lived OIDC PKCE states over `auth_oidc_states`.
///
/// Consumption is single-use and atomic: the row is deleted in the same
/// transaction that reads it, and expired rows consume as absent. Storing
/// a state sweeps the expired ones.
#[derive(Clone, Debug)]
pub struct OidcStates {
    db: AuthDb,
}

impl OidcStates {
    /// The states over one handle.
    pub fn new(db: &AuthDb) -> Self {
        Self { db: db.clone() }
    }

    /// Persist `state` with its verifier for [`STATE_TTL_SECS`].
    pub async fn store_state(
        &self,
        state: &str,
        code_verifier: &str,
    ) -> Result<(), FederatedError> {
        let lane = self.db.lane();
        let (state, code_verifier) = (state.to_owned(), code_verifier.to_owned());
        let db = self.db.clone();
        lane.write(Lane::Foreground, "auth.oidc.store", move |tx| {
            // Read here: the write may have waited its turn on the lane.
            let now = db.now_unix();
            // Abandoned logins leave states behind; sweep the expired ones
            // in the same transaction so the table stays small.
            tx.execute(
                sql::DELETE_EXPIRED_OIDC_STATES.text(),
                rusqlite::params![secs_to_ms(now)],
            )
            .map_err(op_error)?;
            tx.execute(
                sql::STORE_OIDC_STATE.text(),
                rusqlite::params![
                    state,
                    secs_to_ms(now),
                    secs_to_ms(now + STATE_TTL_SECS as i64),
                    code_verifier,
                ],
            )
            .map_err(op_error)?;
            Ok(())
        })
        .await
        .map_err(store_unavailable)
    }

    /// Take and delete `state`; `None` means unknown or expired.
    pub async fn consume_state(&self, state: &str) -> Result<Option<String>, FederatedError> {
        let lane = self.db.lane();
        let state = state.to_owned();
        let db = self.db.clone();
        lane.write(Lane::Foreground, "auth.oidc.consume", move |tx| {
            // Read here: the write may have waited its turn on the lane.
            let now = db.now_unix();
            let found = tx
                .query_row(
                    sql::OIDC_STATE.text(),
                    rusqlite::params![state],
                    sql::OidcStateRow::read,
                )
                .optional()
                .map_err(op_error)?;
            tx.execute(sql::DELETE_OIDC_STATE.text(), rusqlite::params![state])
                .map_err(op_error)?;
            let Some(found) = found else {
                return Ok(None);
            };
            if ms_to_secs(found.expires_at.0) <= now {
                return Ok(None);
            }
            Ok(found.code_verifier)
        })
        .await
        .map_err(store_unavailable)
    }
}
