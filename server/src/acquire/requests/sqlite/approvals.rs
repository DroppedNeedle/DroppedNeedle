//! Auto-download approvals and import batches.

use rusqlite::{OptionalExtension, params};

use super::super::error::RequestsError;
use super::super::ledger::{ApprovalBatch, FollowApproval};
use super::{
    APPROVAL_APPROVED, APPROVAL_PENDING, APPROVAL_REVOKED, epoch_from_real, lane_error, read_error,
};
use crate::acquire::db::AcquireDb;

/// Durable auto-download approvals plus import batches over
/// `auto_download_approvals` (v2 `follow_store` approval half). Batch
/// members are approval rows sharing a `batch_id`.
#[derive(Clone)]
pub struct FollowApprovalStore {
    db: AcquireDb,
}

/// One pending approval or batch row as the queries read it.
type ApprovalRow = (
    String,
    Option<String>,
    String,
    String,
    String,
    f64,
    Option<String>,
);

impl FollowApprovalStore {
    /// Approvals over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// File (or refresh) one pending approval (v2 `upsert_approval`).
    pub async fn file_pending(
        &self,
        user_id: &str,
        artist_mbid: &str,
        artist_name: &str,
        now: u64,
    ) -> Result<(), RequestsError> {
        let (user_id, artist_mbid, artist_name) = (
            user_id.to_owned(),
            artist_mbid.to_owned(),
            artist_name.to_owned(),
        );
        self.db
            .write("approvals.file", move |tx| {
                tx.execute(
                    "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
                     artist_mbid_lower, artist_name, state, requested_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                     artist_name = excluded.artist_name, state = excluded.state, \
                     requested_at = excluded.requested_at, reviewed_by_id = NULL, \
                     reviewed_by_name = NULL, reviewed_at = NULL, batch_id = NULL",
                    params![
                        user_id,
                        artist_mbid,
                        artist_mbid.to_lowercase(),
                        artist_name,
                        APPROVAL_PENDING,
                        now as f64
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| lane_error("approvals.file", error))
    }

    /// Open one import batch over several artists (v2
    /// `create_import_approval_batch`). Never downgrades an approved row.
    pub async fn create_batch(
        &self,
        batch_id: &str,
        user_id: &str,
        artists: &[(String, String)],
        source: &str,
        now: u64,
    ) -> Result<(), RequestsError> {
        let (batch_id, user_id, source) =
            (batch_id.to_owned(), user_id.to_owned(), source.to_owned());
        let artists = artists.to_vec();
        self.db
            .write("approvals.batch", move |tx| {
                for (mbid, name) in &artists {
                    tx.execute(
                        "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
                         artist_mbid_lower, artist_name, state, requested_at, batch_id, source) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                         ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
                         artist_name = excluded.artist_name, state = excluded.state, \
                         requested_at = excluded.requested_at, reviewed_by_id = NULL, \
                         reviewed_by_name = NULL, reviewed_at = NULL, \
                         batch_id = excluded.batch_id, source = excluded.source \
                         WHERE auto_download_approvals.state != ?9",
                        params![
                            user_id,
                            mbid,
                            mbid.to_lowercase(),
                            name,
                            APPROVAL_PENDING,
                            now as f64,
                            batch_id,
                            source,
                            APPROVAL_APPROVED
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| lane_error("approvals.batch", error))
    }

    /// Pending single approvals (not batch members), oldest first.
    pub async fn pending(&self) -> Result<Vec<FollowApproval>, RequestsError> {
        let rows = self.pending_rows(false).await?;
        Ok(rows
            .into_iter()
            .map(
                |(user_id, user_name, artist_mbid, artist_name, state, requested_at, _)| {
                    FollowApproval {
                        user_name: user_name.unwrap_or_else(|| user_id.clone()),
                        user_id,
                        artist_mbid,
                        artist_name,
                        state,
                        requested_at: epoch_from_real(requested_at),
                    }
                },
            )
            .collect())
    }

    /// Pending batches grouped by batch and user, oldest first (v2
    /// `list_pending_approval_batches`).
    pub async fn pending_batches(&self) -> Result<Vec<ApprovalBatch>, RequestsError> {
        let rows = self.pending_rows(true).await?;
        let mut batches: Vec<ApprovalBatch> = Vec::new();
        for (user_id, user_name, artist_mbid, artist_name, state, requested_at, batch_id) in rows {
            let batch_id = batch_id.unwrap_or_default();
            let requested_at = epoch_from_real(requested_at);
            if let Some(batch) = batches
                .iter_mut()
                .find(|batch| batch.batch_id == batch_id && batch.user_id == user_id)
            {
                batch.artists.push((artist_mbid, artist_name));
                batch.requested_at = batch.requested_at.min(requested_at);
                continue;
            }
            batches.push(ApprovalBatch {
                batch_id,
                user_name: user_name.unwrap_or_else(|| user_id.clone()),
                user_id,
                artists: vec![(artist_mbid, artist_name)],
                state,
                requested_at,
            });
        }
        batches.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(batches)
    }

    /// Pending units for the badge: single approvals plus one unit per
    /// pending batch (v2 `count_pending_approval_units`).
    pub async fn pending_units(&self) -> Result<u32, RequestsError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM auto_download_approvals \
             WHERE state = 'pending' AND batch_id IS NULL) + \
             (SELECT COUNT(*) FROM (SELECT batch_id, user_id FROM auto_download_approvals \
             WHERE state = 'pending' AND batch_id IS NOT NULL GROUP BY batch_id, user_id))",
        )
        .fetch_one(self.db.pool())
        .await
        .map_err(|error| read_error("approvals.units", error))?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Move one pending approval to `state`, stamping the reviewer. Only a
    /// pending row moves (v2 `set_approval_state` behind the pending read).
    pub async fn decide(
        &self,
        user_id: &str,
        artist_mbid: &str,
        state: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<Option<FollowApproval>, RequestsError> {
        self.transition(
            user_id,
            artist_mbid,
            APPROVAL_PENDING,
            state,
            reviewer,
            now,
            "approvals.decide",
        )
        .await
    }

    /// Revoke a prior grant. Only approved rows revoke.
    pub async fn revoke(
        &self,
        user_id: &str,
        artist_mbid: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<Option<FollowApproval>, RequestsError> {
        self.transition(
            user_id,
            artist_mbid,
            APPROVAL_APPROVED,
            APPROVAL_REVOKED,
            reviewer,
            now,
            "approvals.revoke",
        )
        .await
    }

    /// Withdraw a pending ask (the user turned auto-download back off).
    pub async fn withdraw(&self, user_id: &str, artist_mbid: &str) -> Result<bool, RequestsError> {
        let (user_id, mbid_lower) = (user_id.to_owned(), artist_mbid.to_lowercase());
        self.db
            .write("approvals.withdraw", move |tx| {
                let changed = tx.execute(
                    "DELETE FROM auto_download_approvals WHERE user_id = ?1 \
                     AND artist_mbid_lower = ?2 AND state = ?3",
                    params![user_id, mbid_lower, APPROVAL_PENDING],
                )?;
                Ok(changed > 0)
            })
            .await
            .map_err(|error| lane_error("approvals.withdraw", error))
    }

    /// Decide every still-pending row of one batch; returns the moved rows
    /// as `(user_id, artist_mbid, artist_name)` (v2 `set_batch_approval_state`).
    pub async fn decide_batch(
        &self,
        batch_id: &str,
        state: &str,
        reviewer: (&str, Option<String>),
        now: u64,
    ) -> Result<Vec<(String, String, String)>, RequestsError> {
        let (batch_id, state) = (batch_id.to_owned(), state.to_owned());
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write("approvals.decide_batch", move |tx| {
                let mut statement = tx.prepare(
                    "SELECT user_id, artist_mbid, artist_name FROM auto_download_approvals \
                     WHERE batch_id = ?1 AND state = ?2 ORDER BY artist_name",
                )?;
                let rows = statement
                    .query_map(params![batch_id, APPROVAL_PENDING], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
                drop(statement);
                tx.execute(
                    "UPDATE auto_download_approvals SET state = ?2, reviewed_by_id = ?3, \
                     reviewed_by_name = ?4, reviewed_at = ?5 WHERE batch_id = ?1 AND state = ?6",
                    params![
                        batch_id,
                        state,
                        reviewer_id,
                        reviewer_name,
                        now as f64,
                        APPROVAL_PENDING
                    ],
                )?;
                Ok(rows)
            })
            .await
            .map_err(|error| lane_error("approvals.decide_batch", error))
    }

    /// Guarded state move for one row.
    #[allow(clippy::too_many_arguments)]
    async fn transition(
        &self,
        user_id: &str,
        artist_mbid: &str,
        from: &'static str,
        to: &str,
        reviewer: (&str, Option<String>),
        now: u64,
        operation: &'static str,
    ) -> Result<Option<FollowApproval>, RequestsError> {
        let (user_id, mbid_lower, to) = (
            user_id.to_owned(),
            artist_mbid.to_lowercase(),
            to.to_owned(),
        );
        let (reviewer_id, reviewer_name) = (reviewer.0.to_owned(), reviewer.1);
        self.db
            .write(operation, move |tx| {
                let row: Option<(String, String, Option<String>, f64)> = tx
                    .query_row(
                        "SELECT a.artist_mbid, a.artist_name, u.display_name, a.requested_at \
                         FROM auto_download_approvals a LEFT JOIN auth_users u ON u.id = a.user_id \
                         WHERE a.user_id = ?1 AND a.artist_mbid_lower = ?2 AND a.state = ?3",
                        params![user_id, mbid_lower, from],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .optional()?;
                let Some((artist_mbid, artist_name, user_name, requested_at)) = row else {
                    return Ok(None);
                };
                tx.execute(
                    "UPDATE auto_download_approvals SET state = ?3, reviewed_by_id = ?4, \
                     reviewed_by_name = ?5, reviewed_at = ?6 \
                     WHERE user_id = ?1 AND artist_mbid_lower = ?2",
                    params![
                        user_id,
                        mbid_lower,
                        to,
                        reviewer_id,
                        reviewer_name,
                        now as f64
                    ],
                )?;
                Ok(Some(FollowApproval {
                    user_name: user_name.unwrap_or_else(|| user_id.clone()),
                    user_id,
                    artist_mbid,
                    artist_name,
                    state: to,
                    requested_at: epoch_from_real(requested_at),
                }))
            })
            .await
            .map_err(|error| lane_error(operation, error))
    }

    /// Pending rows, singles or batch members, oldest first.
    async fn pending_rows(&self, batched: bool) -> Result<Vec<ApprovalRow>, RequestsError> {
        sqlx::query_as(
            "SELECT a.user_id, u.display_name, a.artist_mbid, a.artist_name, a.state, \
             a.requested_at, a.batch_id FROM auto_download_approvals a \
             LEFT JOIN auth_users u ON u.id = a.user_id \
             WHERE a.state = 'pending' AND (a.batch_id IS NOT NULL) = ?1 \
             ORDER BY a.requested_at ASC, a.artist_name ASC",
        )
        .bind(batched)
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| read_error("approvals.pending", error))
    }
}
