//! Decisions on recorded asks: approve, reject, cancel, retry and clear.

use super::{
    Dispatched, RequestsService, cancel_linked, dispatch_fault, record_title, stored_dispatch,
};
use crate::dispatch::{DispatchError, DispatchOrigin, DispatchTarget};
use crate::requests::auth::auto_approves;
use crate::requests::error::RequestsError;
use crate::requests::ledger::{CancelDecision, RequestRecord, RequestStatus, validate_mbid};
use crate::requests::models::{ClearHistoryResponse, RequestKind};
use droppedneedle_auth::{Admin, Principal};

/// How an admin's stop of one row's download ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminStop {
    /// The download could not be stopped; the row is untouched.
    StopFailed,
    /// The row changed meanwhile and was left as it is.
    Changed,
    /// The row reads cancelled.
    Cancelled,
}

/// How a requester's cancel of one row ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequesterCancel {
    /// No row for the id.
    NotFound,
    /// The caller is not on the row; its status at the time.
    Denied(RequestStatus),
    /// A co-requester left the row, which goes on for the others.
    Detached,
    /// A row with no download was cancelled.
    CancelledDirect,
    /// The download could not be stopped; the row is put back.
    StopFailed,
    /// The row changed meanwhile; it is put back.
    Changed,
    /// The download was stopped and the row reads cancelled.
    CancelledTask,
}

/// How a decision on one ask ended. The routes word the reply
/// ([`crate::requests::http::decided_reply`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    /// No row for the id.
    NotFound,
    /// Approve or reject on a row not waiting for approval: its status, or
    /// `changed` when the row went away meanwhile.
    NotWaiting {
        /// The row's status.
        status: String,
    },
    /// Approved and its download started.
    Approved {
        /// The row's title.
        title: String,
    },
    /// Approved, but its download could not start; the row reads failed.
    ApprovedNotStarted {
        /// The row's title.
        title: String,
    },
    /// Approved, but the row moved on before its download was linked.
    ApprovedStale,
    /// Rejected.
    Rejected {
        /// The row's title.
        title: String,
    },
    /// A row with no download was cancelled.
    CancelledRequest {
        /// The row's title.
        title: String,
    },
    /// The row's download was stopped and the row reads cancelled.
    CancelledDownload {
        /// The row's title.
        title: String,
    },
    /// A co-requester left the row, which goes on for the others.
    Detached,
    /// The row's status does not allow a cancel.
    CannotCancel {
        /// The row's status.
        status: RequestStatus,
    },
    /// The download could not be stopped.
    StopFailed,
    /// The row changed during the cancel.
    Changed,
    /// The retry waits for an admin's approval.
    RetryAwaitingApproval,
    /// Retried and its download started.
    Retried {
        /// The row's title.
        title: String,
    },
    /// The retry's download could not start; the row is as it was.
    RetryNotStarted,
    /// The row moved on before the retry's download was linked.
    RetryStale,
    /// The row's status does not allow a retry.
    CannotRetry {
        /// The row's status.
        status: RequestStatus,
    },
}

impl Decided {
    /// Whether the decision applied.
    pub fn applied(&self) -> bool {
        matches!(
            self,
            Self::Approved { .. }
                | Self::Rejected { .. }
                | Self::CancelledRequest { .. }
                | Self::CancelledDownload { .. }
                | Self::Detached
                | Self::RetryAwaitingApproval
                | Self::Retried { .. }
        )
    }
}

/// How a batch cancel ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchCancelled {
    /// Rows cancelled or left.
    pub cancelled: u32,
    /// Ids that could not be cancelled.
    pub failed: u32,
}

/// The outcome of a requester's cancel.
fn requester_decided(
    outcome: RequesterCancel,
    record: &RequestRecord,
) -> Result<Decided, RequestsError> {
    Ok(match outcome {
        RequesterCancel::NotFound => Decided::NotFound,
        RequesterCancel::Denied(prior_status) => {
            if prior_status.is_cancellable() || prior_status == RequestStatus::AwaitingApproval {
                return Err(RequestsError::Forbidden {
                    message: "Cannot cancel another user's request".to_owned(),
                });
            }
            Decided::CannotCancel {
                status: prior_status,
            }
        }
        RequesterCancel::Detached => Decided::Detached,
        RequesterCancel::CancelledDirect => Decided::CancelledRequest {
            title: record_title(record),
        },
        RequesterCancel::StopFailed => Decided::StopFailed,
        RequesterCancel::Changed => Decided::Changed,
        RequesterCancel::CancelledTask => Decided::CancelledDownload {
            title: record_title(record),
        },
    })
}

impl RequestsService {
    /// Cancel a batch of asks. Non-admins detach from shared rows and
    /// cancel their own; only an explicit admin cancels anything (a
    /// missing id must never mint admin rights).
    pub async fn cancel_batch(
        &self,
        principal: &Principal,
        ids: &[String],
        kind: RequestKind,
    ) -> Result<BatchCancelled, RequestsError> {
        let is_admin = principal.is_admin();
        let now = self.now_epoch();
        let mut cancelled = 0_u32;
        let mut failed = 0_u32;
        let mut seen = std::collections::HashSet::new();
        for raw in ids {
            if !seen.insert(raw.clone()) {
                continue;
            }
            let Ok(mbid) = validate_mbid(raw) else {
                failed = failed.saturating_add(1);
                continue;
            };
            let outcome = if is_admin {
                self.admin_cancel_one(kind, &mbid, now).await?
            } else {
                matches!(
                    self.requester_cancel(kind, &mbid, principal.user_id(), now)
                        .await?,
                    RequesterCancel::Detached
                        | RequesterCancel::CancelledDirect
                        | RequesterCancel::CancelledTask
                )
            };
            if outcome {
                cancelled = cancelled.saturating_add(1);
            } else {
                failed = failed.saturating_add(1);
            }
        }
        Ok(BatchCancelled { cancelled, failed })
    }

    /// Approve one waiting ask and dispatch it. The
    /// claim moves the row out of the queue before dispatch so a second
    /// approval loses loudly instead of double-dispatching.
    pub async fn approve_request(
        &self,
        admin: &Admin,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<Decided, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(Decided::NotFound);
        };
        if record.status != RequestStatus::AwaitingApproval {
            return Ok(Decided::NotWaiting {
                status: record.status.as_str().to_owned(),
            });
        }
        let claimed = self
            .store
            .claim_approval(
                kind,
                &mbid,
                Some(admin.username_display().to_owned()),
                now,
                record.generation,
            )
            .await?;
        let Some(claimed) = claimed else {
            let status = self
                .store
                .get(kind, &mbid)
                .await?
                .map(|row| row.status.as_str().to_owned())
                .unwrap_or_else(|| "changed".to_owned());
            return Ok(Decided::NotWaiting { status });
        };
        // Shared approvals keep the immutable primary owner; only ownerless
        // legacy rows fall back to the acting admin.
        let owner = claimed
            .user_id
            .clone()
            .unwrap_or_else(|| admin.user_id().to_owned());
        let request = stored_dispatch(&claimed, DispatchOrigin::Approval, &owner);
        match self
            .dispatch_and_link(kind, &mbid, &request, claimed.generation)
            .await?
        {
            Dispatched::Failed(DispatchError::Busy { operation }) => {
                self.store
                    .restore_status(
                        kind,
                        &mbid,
                        RequestStatus::AwaitingApproval,
                        RequestStatus::Pending,
                        claimed.generation,
                    )
                    .await?;
                Err(RequestsError::Busy { operation })
            }
            Dispatched::Failed(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "approved dispatch failed");
                self.store
                    .update_status(
                        kind,
                        &mbid,
                        RequestStatus::Failed,
                        Some(now),
                        Some(claimed.generation),
                    )
                    .await?;
                Ok(Decided::ApprovedNotStarted {
                    title: record_title(&record),
                })
            }
            Dispatched::Stale => Ok(Decided::ApprovedStale),
            Dispatched::Linked(_) => Ok(Decided::Approved {
                title: record_title(&record),
            }),
        }
    }

    /// Reject one waiting ask.
    pub async fn reject_request(
        &self,
        admin: &Admin,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<Decided, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(Decided::NotFound);
        };
        if record.status != RequestStatus::AwaitingApproval {
            return Ok(Decided::NotWaiting {
                status: record.status.as_str().to_owned(),
            });
        }
        let claimed = self
            .store
            .claim_rejection(
                kind,
                &mbid,
                Some(admin.username_display().to_owned()),
                now,
                record.generation,
            )
            .await?;
        if !claimed {
            let status = self
                .store
                .get(kind, &mbid)
                .await?
                .map(|row| row.status.as_str().to_owned())
                .unwrap_or_else(|| "changed".to_owned());
            return Ok(Decided::NotWaiting { status });
        }
        Ok(Decided::Rejected {
            title: record_title(&record),
        })
    }

    /// Cancel one ask.
    pub async fn cancel_one(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<Decided, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(Decided::NotFound);
        };
        if !principal.is_admin() {
            let outcome = self
                .requester_cancel(kind, &mbid, principal.user_id(), now)
                .await?;
            return requester_decided(outcome, &record);
        }
        if record.status == RequestStatus::AwaitingApproval {
            if !self
                .store
                .update_status(
                    kind,
                    &mbid,
                    RequestStatus::Cancelled,
                    Some(now),
                    Some(record.generation),
                )
                .await?
            {
                return Ok(Decided::Changed);
            }
            // Approval cancellation revokes the persisted capability after
            // winning the generation CAS.
            self.store
                .set_dispatch_authorized(kind, &mbid, false)
                .await?;
            return Ok(Decided::CancelledRequest {
                title: record_title(&record),
            });
        }
        if !record.status.is_cancellable() {
            return Ok(Decided::CannotCancel {
                status: record.status,
            });
        }
        Ok(
            match self.admin_stop(kind, &mbid, &record, now, false).await? {
                AdminStop::StopFailed => Decided::StopFailed,
                AdminStop::Changed => Decided::Changed,
                AdminStop::Cancelled => Decided::CancelledDownload {
                    title: record_title(&record),
                },
            },
        )
    }

    /// After an admin stopped a row's download, make the row read
    /// cancelled. Stopping the download already settles a linked live row
    /// as cancelled in the same write; a row it did not reach is cancelled
    /// here at its current generation, if it may still be (`waiting` lets
    /// a row awaiting approval through) and still follows the download that
    /// was stopped (`stopped`). Answers whether the row reads cancelled.
    async fn land_admin_cancel(
        &self,
        kind: RequestKind,
        mbid: &str,
        stopped: Option<&str>,
        now: u64,
        waiting: bool,
    ) -> Result<bool, RequestsError> {
        let Some(current) = self.store.get(kind, mbid).await? else {
            return Ok(false);
        };
        if current.status == RequestStatus::Cancelled {
            return Ok(true);
        }
        // Linked to a new download meanwhile: that one keeps running.
        if current.task_id.as_deref() != stopped {
            return Ok(false);
        }
        let open = current.status.is_cancellable()
            || (waiting && current.status == RequestStatus::AwaitingApproval);
        if !open {
            return Ok(false);
        }
        self.store
            .update_status(
                kind,
                mbid,
                RequestStatus::Cancelled,
                Some(now),
                Some(current.generation),
            )
            .await
    }

    /// Retry one terminal ask. An ordinary user may only
    /// dispatch a generation that already carries approval provenance;
    /// anything else rejoins the queue.
    pub async fn retry_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<Decided, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        self.retry_with(principal, &mbid, kind, false).await
    }

    /// Take a single from its album: a failed track request whose download
    /// found only the album's version of the song is retried, and this
    /// time the album's track imports as the song when it is the right
    /// track. The person's click is the choice; the retry keeps the
    /// request's approval, like any retry.
    pub async fn take_album_version(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<Decided, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let kind = RequestKind::Track;
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Err(RequestsError::NotFound);
        };
        if !principal.is_admin()
            && !self
                .store
                .is_requester(kind, &mbid, principal.user_id())
                .await?
        {
            return Err(RequestsError::Forbidden {
                message: "Cannot change another user's request".to_owned(),
            });
        }
        let offered = match record.task_id.as_deref() {
            Some(task_id) if record.status == RequestStatus::Failed => self
                .dispatch
                .album_version_only(task_id)
                .await
                .map_err(dispatch_fault)?,
            _ => false,
        };
        if !offered {
            return Err(RequestsError::Conflict {
                message: "This request has no album version to take".to_owned(),
            });
        }
        // Waiting for approval would drop the choice: the retry would be
        // dispatched later as a plain one.
        if !record.dispatch_authorized && !auto_approves(principal.role()) {
            return Err(RequestsError::Conflict {
                message: "This request needs approval first".to_owned(),
            });
        }
        self.retry_with(principal, &mbid, kind, true).await
    }

    /// Retry one terminal ask; `album_version` takes a single from its
    /// album (see [`Self::take_album_version`]).
    async fn retry_with(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
        album_version: bool,
    ) -> Result<Decided, RequestsError> {
        let mbid = mbid.to_owned();
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(Decided::NotFound);
        };
        if !record.status.is_retryable() {
            return Ok(Decided::CannotRetry {
                status: record.status,
            });
        }
        let prior_status = record.status;
        let can_dispatch = record.dispatch_authorized || auto_approves(principal.role());
        let target = if can_dispatch {
            RequestStatus::Pending
        } else {
            RequestStatus::AwaitingApproval
        };
        let claim = self
            .store
            .claim_retry(
                kind,
                &mbid,
                principal.user_id(),
                target,
                can_dispatch,
                !principal.is_admin(),
                now,
                record.generation,
            )
            .await?;
        let generation = match claim {
            crate::requests::ledger::RetryClaim::Claimed { generation, .. } => generation,
            crate::requests::ledger::RetryClaim::Lost => {
                let current = self.store.get(kind, &mbid).await?;
                if !principal.is_admin()
                    && current
                        .as_ref()
                        .is_some_and(|row| row.status.is_retryable())
                    && !self
                        .store
                        .is_requester(kind, &mbid, principal.user_id())
                        .await?
                {
                    return Err(RequestsError::Forbidden {
                        message: "Cannot retry another user's request".to_owned(),
                    });
                }
                return Ok(match current {
                    None => Decided::NotFound,
                    Some(row) => Decided::CannotRetry { status: row.status },
                });
            }
        };
        if target == RequestStatus::AwaitingApproval {
            return Ok(Decided::RetryAwaitingApproval);
        }
        let owner = record
            .user_id
            .clone()
            .unwrap_or_else(|| principal.user_id().to_owned());
        let mut request = stored_dispatch(&record, DispatchOrigin::Retry, &owner);
        if let DispatchTarget::Track {
            album_version: from_album,
            ..
        } = &mut request.target
        {
            *from_album = album_version;
        }
        match self
            .dispatch_and_link(kind, &mbid, &request, generation)
            .await?
        {
            Dispatched::Failed(DispatchError::Busy { operation }) => {
                self.store
                    .update_status(kind, &mbid, prior_status, None, Some(generation))
                    .await?;
                Err(RequestsError::Busy { operation })
            }
            Dispatched::Failed(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "retry dispatch failed");
                self.store
                    .update_status(kind, &mbid, prior_status, None, Some(generation))
                    .await?;
                Ok(Decided::RetryNotStarted)
            }
            Dispatched::Stale => Ok(Decided::RetryStale),
            Dispatched::Linked(_) => Ok(Decided::Retried {
                title: record_title(&record),
            }),
        }
    }

    /// Clear one history row. Ownership reads before clearability so a
    /// non-owner gets 403 rather than a misleading miss; admins
    /// delete the row while users dismiss it from their own view.
    pub async fn clear_history_item(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ClearHistoryResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ClearHistoryResponse { success: false });
        };
        if !principal.is_admin()
            && !self
                .store
                .is_requester(kind, &mbid, principal.user_id())
                .await?
        {
            return Err(RequestsError::Forbidden {
                message: "Cannot clear another user's request".to_owned(),
            });
        }
        if !record.status.is_clearable() {
            return Ok(ClearHistoryResponse { success: false });
        }
        let cleared = if principal.is_admin() {
            self.store.delete(kind, &mbid).await?
        } else {
            self.store
                .dismiss(kind, &mbid, principal.user_id(), self.now_epoch())
                .await?
        };
        Ok(ClearHistoryResponse { success: cleared })
    }

    /// Quiet admin cancel of one row for batch-cancel: only live rows
    /// cancel, and approval cancels revoke the persisted capability.
    async fn admin_cancel_one(
        &self,
        kind: RequestKind,
        mbid: &str,
        now: u64,
    ) -> Result<bool, RequestsError> {
        let Some(record) = self.store.get(kind, mbid).await? else {
            return Ok(false);
        };
        if !record.status.is_active() {
            return Ok(false);
        }
        if self.admin_stop(kind, mbid, &record, now, true).await? != AdminStop::Cancelled {
            return Ok(false);
        }
        if record.status == RequestStatus::AwaitingApproval {
            self.store
                .set_dispatch_authorized(kind, mbid, false)
                .await?;
        }
        Ok(true)
    }

    /// Stop a row's linked download for an admin, then make the row read
    /// cancelled ([`Self::land_admin_cancel`]).
    async fn admin_stop(
        &self,
        kind: RequestKind,
        mbid: &str,
        record: &RequestRecord,
        now: u64,
        waiting: bool,
    ) -> Result<AdminStop, RequestsError> {
        if let Some(task_id) = record.task_id.as_deref()
            && !cancel_linked(&self.dispatch, task_id).await
        {
            return Ok(AdminStop::StopFailed);
        }
        if !self
            .land_admin_cancel(kind, mbid, record.task_id.as_deref(), now, waiting)
            .await?
        {
            return Ok(AdminStop::Changed);
        }
        Ok(AdminStop::Cancelled)
    }

    /// A requester's cancel of one row. The task cancels under the
    /// immutable primary owner, never the actor (a co-requester
    /// can never replace the attribution). A row whose download could not
    /// be stopped, or that changed meanwhile, is put back as it was.
    async fn requester_cancel(
        &self,
        kind: RequestKind,
        mbid: &str,
        user_id: &str,
        now: u64,
    ) -> Result<RequesterCancel, RequestsError> {
        let decision = self
            .store
            .prepare_requester_cancel(kind, mbid, user_id, now)
            .await?;
        let Some(decision) = decision else {
            return Ok(RequesterCancel::NotFound);
        };
        let (prior_status, task_id, generation) = match decision {
            CancelDecision::Denied { prior_status } => {
                return Ok(RequesterCancel::Denied(prior_status));
            }
            CancelDecision::Detached => return Ok(RequesterCancel::Detached),
            CancelDecision::CancelledDirect => return Ok(RequesterCancel::CancelledDirect),
            CancelDecision::CancelTask {
                prior_status,
                task_id,
                generation,
                ..
            } => (prior_status, task_id, generation),
        };
        let outcome = if let Some(task_id) = task_id.as_deref()
            && !cancel_linked(&self.dispatch, task_id).await
        {
            RequesterCancel::StopFailed
        } else if !self
            .store
            .update_status(
                kind,
                mbid,
                RequestStatus::Cancelled,
                Some(now),
                Some(generation),
            )
            .await?
        {
            RequesterCancel::Changed
        } else {
            return Ok(RequesterCancel::CancelledTask);
        };
        self.store
            .restore_status(
                kind,
                mbid,
                prior_status,
                RequestStatus::Cancelling,
                generation,
            )
            .await?;
        Ok(outcome)
    }
}
