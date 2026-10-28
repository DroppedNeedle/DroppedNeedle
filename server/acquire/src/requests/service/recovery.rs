//! Startup recovery for the request ledger.

use super::{RequestsService, cancel_linked, stored_dispatch};
use crate::dispatch::DispatchOutcome;
use crate::requests::error::RequestsError;
use crate::requests::ledger::RequestStatus;
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
}

impl RequestsService {
    /// Startup recovery for the ledger. A row left `cancelling` finishes
    /// its cancel (the owner asked to stop; cancelling a task twice is
    /// harmless). An authorized `pending` row with no task had its dispatch
    /// or its link cut off: it relinks to the task created for it, or
    /// dispatches under the same idempotency key, so nothing runs twice.
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
                    .update_status(
                        kind,
                        &record.key,
                        RequestStatus::Cancelled,
                        Some(now),
                        Some(record.generation),
                    )
                    .await?
                {
                    report.cancelled += 1;
                }
            }
            for record in self.store.with_status(kind, RequestStatus::Pending).await? {
                if record.task_id.is_some() || !record.dispatch_authorized {
                    continue;
                }
                let owner = record.user_id.clone().unwrap_or_default();
                let found = self
                    .dispatch
                    .find_task_since(&owner, kind.as_str(), &record.key, record.requested_at)
                    .await;
                let task_id = match found {
                    Ok(Some(task_id)) => {
                        report.relinked += 1;
                        task_id
                    }
                    Ok(None) => {
                        let request = stored_dispatch(&record, record.origin, &owner);
                        match self.dispatch.dispatch(&request).await {
                            Ok(DispatchOutcome::Dispatched { task_id }) => {
                                report.redispatched += 1;
                                task_id
                            }
                            Ok(DispatchOutcome::Active { task_id }) => {
                                report.relinked += 1;
                                task_id
                            }
                            Err(error) => {
                                tracing::warn!(key = %record.key, ?error, "recovery dispatch failed");
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(key = %record.key, ?error, "recovery task lookup failed");
                        continue;
                    }
                };
                self.store
                    .link_task(kind, &record.key, &task_id, Some(record.generation))
                    .await?;
            }
        }
        Ok(report)
    }
}
