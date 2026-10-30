//! Startup recovery for the request ledger.

use super::{RequestsService, cancel_linked, cancel_started, stored_dispatch};
use crate::dispatch::DispatchOutcome;
use crate::requests::error::RequestsError;
use crate::requests::ledger::{RequestRecord, RequestStatus};
use crate::requests::models::RequestKind;

/// What startup recovery did to the request ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestRecovery {
    /// Interrupted cancels finished.
    pub cancelled: usize,
    /// Rows relinked to the task created for them.
    pub relinked: usize,
    /// Rows dispatched again under their original key.
    pub redispatched: usize,
    /// Pending Weekly Mix asks of admins and trusted users removed: their
    /// grant never waits for an admin.
    pub mix_asks_cleared: u64,
}

/// How a pending row with no task got one.
pub(super) enum Revived {
    /// The row follows the task already created for it.
    Relinked(String),
    /// The row's download was queued again under its original key.
    Redispatched(String),
}

impl RequestsService {
    /// Startup recovery for the ledger. A row left `cancelling` finishes
    /// its cancel (the owner asked to stop; cancelling a task twice is
    /// harmless). An authorized `pending` row with no task had its dispatch
    /// or its link cut off: it relinks to the task created for it, or
    /// dispatches under the same idempotency key, so nothing runs twice.
    /// Pending Weekly Mix asks of admins and trusted users are removed.
    pub async fn recover(&self) -> Result<RequestRecovery, RequestsError> {
        let now = self.now_epoch();
        let mut report = RequestRecovery::default();
        for kind in [RequestKind::Album, RequestKind::Track] {
            for record in self
                .store
                .with_status(kind, RequestStatus::Cancelling)
                .await?
            {
                if let Some(task_id) = record.task_id.as_deref()
                    && !cancel_linked(&self.dispatch, task_id).await
                {
                    continue;
                }
                if self
                    .store
                    .cancel(kind, &record.key, now, Some(record.generation))
                    .await?
                {
                    report.cancelled += 1;
                }
            }
            for record in self.store.with_status(kind, RequestStatus::Pending).await? {
                if record.task_id.is_some() || !record.dispatch_authorized {
                    continue;
                }
                match self.revive(&record).await? {
                    Some(Revived::Relinked(_)) => report.relinked += 1,
                    Some(Revived::Redispatched(_)) => report.redispatched += 1,
                    None => {}
                }
            }
        }
        report.mix_asks_cleared = self.mixes.clear_curator_pending().await?;
        Ok(report)
    }
}

impl RequestsService {
    /// Give an authorized `pending` row with no task the task it lost:
    /// the one created for it since it was asked, else a dispatch under
    /// its original idempotency key. A row that moved on meanwhile is left
    /// alone, and a download started for it is cancelled. Answers what
    /// happened, or `None` when the row still has no task.
    pub(super) async fn revive(
        &self,
        record: &RequestRecord,
    ) -> Result<Option<Revived>, RequestsError> {
        let kind = record.kind;
        let owner = record.user_id.clone().unwrap_or_default();
        let found = self
            .dispatch
            .find_task_since(&owner, kind.as_str(), &record.key, record.requested_at)
            .await;
        let (revived, started) = match found {
            Ok(Some(task_id)) => (Revived::Relinked(task_id), None),
            Ok(None) => {
                let request = stored_dispatch(record, record.origin, &owner);
                match self.dispatch.dispatch(&request).await {
                    Ok(outcome @ DispatchOutcome::Dispatched { .. }) => (
                        Revived::Redispatched(outcome.task_id().to_owned()),
                        Some(outcome),
                    ),
                    Ok(DispatchOutcome::Active { task_id }) => (Revived::Relinked(task_id), None),
                    Err(error) => {
                        tracing::warn!(key = %record.key, ?error, "request dispatch failed");
                        return Ok(None);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(key = %record.key, ?error, "request task lookup failed");
                return Ok(None);
            }
        };
        let task_id = match &revived {
            Revived::Relinked(task_id) | Revived::Redispatched(task_id) => task_id,
        };
        if self
            .store
            .link_task(kind, &record.key, task_id, Some(record.generation))
            .await?
            || self.follows_task(kind, &record.key, task_id).await?
        {
            return Ok(Some(revived));
        }
        if let Some(outcome) = started {
            cancel_started(&self.dispatch, &outcome).await;
        }
        Ok(None)
    }
}
