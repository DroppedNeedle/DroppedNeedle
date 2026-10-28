//! Personal-mix approvals.

use std::collections::HashSet;
use std::sync::Mutex;

use rusqlite::{OptionalExtension, params};

use super::super::error::RequestsError;
use super::super::ledger::{ApprovalState, MixApproval};
use super::super::sql;
use super::{lane_error, ms_from_secs, read, secs_from_ms, unknown_value};
use crate::db::AcquireDb;

/// Decode a stored approval state; one outside the CHECK fails the read.
fn approval_state(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<ApprovalState> {
    let text: String = row.get(index)?;
    ApprovalState::parse(&text).ok_or_else(|| unknown_value(index, "state", &text))
}

/// Durable personal-mix approvals over `personal_mix_approvals`, plus the
/// in-process refresh guard (a build running in this process).
pub struct PersonalMixStore {
    db: AcquireDb,
    refresh_running: Mutex<HashSet<String>>,
}

impl PersonalMixStore {
    /// Mix approvals over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self {
            db,
            refresh_running: Mutex::new(HashSet::new()),
        }
    }

    /// File (or refresh) one pending approval.
    pub async fn file_pending(&self, user_id: &str, now: u64) -> Result<(), RequestsError> {
        let user_id = user_id.to_owned();
        self.db
            .write("mix.file", move |tx| {
                tx.execute(
                    sql::FILE_MIX_APPROVAL.text(),
                    params![user_id, ApprovalState::Pending.as_str(), ms_from_secs(now)],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("mix.file", error))
    }

    /// One user's approval state, if a row exists.
    pub async fn state(&self, user_id: &str) -> Result<Option<String>, RequestsError> {
        let user_id = user_id.to_owned();
        read(&self.db, "mix.state", move |conn| {
            conn.query_row(sql::MIX_APPROVAL_STATE.text(), params![user_id], |row| {
                Ok(approval_state(row, 0)?.as_str().to_owned())
            })
            .optional()
        })
        .await
    }

    /// Pending approvals, oldest first.
    pub async fn pending(&self) -> Result<Vec<MixApproval>, RequestsError> {
        read(&self.db, "mix.pending", |conn| {
            let mut statement = conn.prepare_cached(sql::PENDING_MIX_APPROVALS.text())?;
            let rows = statement.query_map([], |row| {
                let user_id: String = row.get(0)?;
                let user_name: Option<String> = row.get(1)?;
                Ok(MixApproval {
                    user_name: user_name.unwrap_or_else(|| user_id.clone()),
                    user_id,
                    state: approval_state(row, 2)?.as_str().to_owned(),
                    requested_at: secs_from_ms(row.get(3)?),
                })
            })?;
            rows.collect()
        })
        .await
    }

    /// Pending approval count for the badge.
    pub async fn pending_count(&self) -> Result<u32, RequestsError> {
        Ok(u32::try_from(self.pending().await?.len()).unwrap_or(u32::MAX))
    }

    /// Move one pending approval to `state`.
    pub async fn decide(
        &self,
        user_id: &str,
        state: ApprovalState,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        self.transition(user_id, ApprovalState::Pending, state, reviewer, now)
            .await
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub async fn revoke(
        &self,
        user_id: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        self.transition(
            user_id,
            ApprovalState::Approved,
            ApprovalState::Revoked,
            reviewer,
            now,
        )
        .await
    }

    /// Claim the refresh key. False means a build already holds it.
    pub fn refresh_start(&self, user_id: &str) -> Result<bool, RequestsError> {
        let mut running = self.refresh_running.lock().map_err(|cause| {
            RequestsError::internal(&format_args!("mix refresh lock failed: {cause}"))
        })?;
        Ok(running.insert(user_id.to_owned()))
    }

    /// Release the refresh key once the build lands.
    pub fn refresh_finish(&self, user_id: &str) -> Result<(), RequestsError> {
        let mut running = self.refresh_running.lock().map_err(|cause| {
            RequestsError::internal(&format_args!("mix refresh lock failed: {cause}"))
        })?;
        running.remove(user_id);
        Ok(())
    }

    async fn transition(
        &self,
        user_id: &str,
        from: ApprovalState,
        to: ApprovalState,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        let user_id = user_id.to_owned();
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write("mix.transition", move |tx| {
                let changed = tx.execute(
                    sql::MIX_APPROVAL_TRANSITION.text(),
                    params![
                        user_id,
                        to.as_str(),
                        reviewer_id,
                        reviewer_name,
                        ms_from_secs(now),
                        from.as_str()
                    ],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("mix.transition", error))
    }
}
