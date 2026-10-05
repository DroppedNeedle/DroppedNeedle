//! Personal-mix approvals.

use std::collections::HashSet;
use std::sync::Mutex;

use rusqlite::params;

use super::super::error::RequestsError;
use super::super::ledger::MixApproval;
use super::{
    APPROVAL_APPROVED, APPROVAL_PENDING, APPROVAL_REVOKED, epoch_from_real, lane_error, read_error,
};
use crate::acquire::db::AcquireDb;

/// Durable personal-mix approvals over `personal_mix_approvals`, plus the
/// in-process refresh guard (a build running in this process).
pub struct PersonalMixStore {
    db: AcquireDb,
    refresh_running: Mutex<HashSet<String>>,
    #[cfg(any(test, feature = "test-support"))]
    unlinked: std::sync::RwLock<HashSet<String>>,
}

impl PersonalMixStore {
    /// Mix approvals over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self {
            db,
            refresh_running: Mutex::new(HashSet::new()),
            #[cfg(any(test, feature = "test-support"))]
            unlinked: std::sync::RwLock::new(HashSet::new()),
        }
    }

    /// File (or refresh) one pending approval.
    pub async fn file_pending(&self, user_id: &str, now: u64) -> Result<(), RequestsError> {
        let user_id = user_id.to_owned();
        self.db
            .write("mix.file", move |tx| {
                tx.execute(
                    "INSERT INTO personal_mix_approvals (user_id, state, requested_at) \
                     VALUES (?1, ?2, ?3) ON CONFLICT (user_id) DO UPDATE SET \
                     state = excluded.state, requested_at = excluded.requested_at, \
                     reviewed_by_id = NULL, reviewed_by_name = NULL, reviewed_at = NULL",
                    params![user_id, APPROVAL_PENDING, now as f64],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("mix.file", error))
    }

    /// Mark one user unlinked (fixtures only).
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_unlinked(&self, user_id: &str) {
        if let Ok(mut unlinked) = self.unlinked.write() {
            unlinked.insert(user_id.to_owned());
        }
    }

    /// Whether one user may build a mix. Linking lives with the
    /// ListenBrainz connection; every user reads as linked until that port
    /// arrives.
    pub fn is_linked(&self, user_id: &str) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if let Ok(unlinked) = self.unlinked.read()
            && unlinked.contains(user_id)
        {
            return false;
        }
        let _ = user_id;
        true
    }

    /// Pending approvals, oldest first.
    pub async fn pending(&self) -> Result<Vec<MixApproval>, RequestsError> {
        let rows: Vec<(String, Option<String>, String, f64)> = sqlx::query_as(
            "SELECT m.user_id, u.display_name, m.state, m.requested_at \
             FROM personal_mix_approvals m LEFT JOIN auth_users u ON u.id = m.user_id \
             WHERE m.state = 'pending' ORDER BY m.requested_at ASC, m.user_id",
        )
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_error("mix.pending", error))?;
        Ok(rows
            .into_iter()
            .map(|(user_id, user_name, state, requested_at)| MixApproval {
                user_name: user_name.unwrap_or_else(|| user_id.clone()),
                user_id,
                state,
                requested_at: epoch_from_real(requested_at),
            })
            .collect())
    }

    /// Pending approval count for the badge.
    pub async fn pending_count(&self) -> Result<u32, RequestsError> {
        Ok(u32::try_from(self.pending().await?.len()).unwrap_or(u32::MAX))
    }

    /// Move one pending approval to `state`.
    pub async fn decide(
        &self,
        user_id: &str,
        state: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        self.transition(user_id, APPROVAL_PENDING, state, reviewer, now)
            .await
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub async fn revoke(
        &self,
        user_id: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        self.transition(user_id, APPROVAL_APPROVED, APPROVAL_REVOKED, reviewer, now)
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
        from: &'static str,
        to: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<bool, RequestsError> {
        let (user_id, to) = (user_id.to_owned(), to.to_owned());
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write("mix.transition", move |tx| {
                let changed = tx.execute(
                    "UPDATE personal_mix_approvals SET state = ?2, reviewed_by_id = ?3, \
                     reviewed_by_name = ?4, reviewed_at = ?5 WHERE user_id = ?1 AND state = ?6",
                    params![user_id, to, reviewer_id, reviewer_name, now as f64, from],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("mix.transition", error))
    }
}
