//! Auto-download approvals and import batches over
//! `auto_download_approvals`. Batch members are approval rows sharing a
//! `batch_id`. The writes here are steps inside one [`Follows`]
//! transaction; the reads go through the pool.

use rusqlite::{OptionalExtension, Transaction, params};

use super::{Follows, sql};
use crate::requests::error::RequestsError;
use crate::requests::ledger::{ApprovalBatch, ApprovalState, FollowApproval};
use crate::requests::sqlite::{read, secs_from_ms};

/// One pending approval or batch row as the queries read it.
type ApprovalRow = (String, Option<String>, String, String, i64, Option<String>);

/// Who decided, as stamped on the row: user id and display name.
pub(super) type Reviewer = (String, Option<String>);

/// File (or refresh) one pending ask. An approved row stays approved: an
/// admin's yes lasts until revoked.
pub(super) fn file_pending(
    tx: &Transaction,
    user_id: &str,
    artist_mbid: &str,
    artist_name: &str,
    now_ms: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::FILE_APPROVAL.text(),
        params![
            user_id,
            artist_mbid,
            artist_mbid.to_lowercase(),
            artist_name,
            ApprovalState::Pending,
            now_ms,
            ApprovalState::Approved
        ],
    )?;
    Ok(())
}

/// File one batch member. Never downgrades an approved row.
pub(super) fn file_batch_member(
    tx: &Transaction,
    batch_id: &str,
    user_id: &str,
    (artist_mbid, artist_name): (&str, &str),
    source: &str,
    now_ms: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::FILE_BATCH_MEMBER.text(),
        params![
            user_id,
            artist_mbid,
            artist_mbid.to_lowercase(),
            artist_name,
            ApprovalState::Pending,
            now_ms,
            batch_id,
            source,
            ApprovalState::Approved
        ],
    )?;
    Ok(())
}

/// Withdraw a pending ask (the user turned auto-download back off).
pub(super) fn withdraw_pending(
    tx: &Transaction,
    user_id: &str,
    mbid_lower: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::WITHDRAW_APPROVAL.text(),
        params![user_id, mbid_lower, ApprovalState::Pending],
    )?;
    Ok(())
}

/// Move one row from `from` to `to`, stamping the reviewer. A row in any
/// other state stays as it is and answers `None`.
pub(super) fn transition(
    tx: &Transaction,
    user_id: &str,
    mbid_lower: &str,
    (from, to): (ApprovalState, ApprovalState),
    reviewer: &Reviewer,
    now_ms: i64,
) -> rusqlite::Result<Option<FollowApproval>> {
    let row: Option<(String, String, Option<String>, i64)> = tx
        .query_row(
            sql::APPROVAL_IN_STATE.text(),
            params![user_id, mbid_lower, from],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((artist_mbid, artist_name, user_name, requested_at)) = row else {
        return Ok(None);
    };
    tx.execute(
        sql::DECIDE_APPROVAL.text(),
        params![user_id, mbid_lower, to, reviewer.0, reviewer.1, now_ms],
    )?;
    Ok(Some(FollowApproval {
        user_name: user_name.unwrap_or_else(|| user_id.to_owned()),
        user_id: user_id.to_owned(),
        artist_mbid,
        artist_name,
        state: to,
        requested_at: secs_from_ms(requested_at),
    }))
}

/// Decide every still-pending row of one batch; returns the moved rows as
/// `(user_id, artist_mbid, artist_name)`.
pub(super) fn decide_batch_rows(
    tx: &Transaction,
    batch_id: &str,
    state: ApprovalState,
    reviewer: &Reviewer,
    now_ms: i64,
) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut statement = tx.prepare_cached(sql::BATCH_ROWS.text())?;
    let rows = statement
        .query_map(params![batch_id, ApprovalState::Pending], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    drop(statement);
    tx.execute(
        sql::DECIDE_BATCH.text(),
        params![
            batch_id,
            state,
            reviewer.0,
            reviewer.1,
            now_ms,
            ApprovalState::Pending
        ],
    )?;
    Ok(rows)
}

impl Follows {
    /// Pending single approvals (not batch members), oldest first.
    pub async fn pending(&self) -> Result<Vec<FollowApproval>, RequestsError> {
        let rows = self.pending_rows(false).await?;
        Ok(rows
            .into_iter()
            .map(
                |(user_id, user_name, artist_mbid, artist_name, requested_at, _)| FollowApproval {
                    user_name: user_name.unwrap_or_else(|| user_id.clone()),
                    user_id,
                    artist_mbid,
                    artist_name,
                    state: ApprovalState::Pending,
                    requested_at: secs_from_ms(requested_at),
                },
            )
            .collect())
    }

    /// Pending batches grouped by batch and user, oldest first.
    pub async fn pending_batches(&self) -> Result<Vec<ApprovalBatch>, RequestsError> {
        let rows = self.pending_rows(true).await?;
        let mut batches: Vec<ApprovalBatch> = Vec::new();
        for (user_id, user_name, artist_mbid, artist_name, requested_at, batch_id) in rows {
            let batch_id = batch_id.unwrap_or_default();
            let requested_at = secs_from_ms(requested_at);
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
                state: ApprovalState::Pending,
                requested_at,
            });
        }
        batches.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        Ok(batches)
    }

    /// Pending units for the badge: single approvals plus one unit per
    /// pending batch.
    pub async fn pending_units(&self) -> Result<u32, RequestsError> {
        let count: i64 = read(&self.db, "approvals.units", |conn| {
            conn.query_row(sql::PENDING_UNITS.text(), [], |row| row.get(0))
        })
        .await?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Pending rows, singles or batch members, oldest first.
    async fn pending_rows(&self, batched: bool) -> Result<Vec<ApprovalRow>, RequestsError> {
        read(&self.db, "approvals.pending", move |conn| {
            let mut statement = conn.prepare_cached(sql::PENDING_APPROVALS.text())?;
            let rows = statement.query_map(params![batched], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?;
            rows.collect()
        })
        .await
    }
}
