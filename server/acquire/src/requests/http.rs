//! The requests routes' one mapping onto [`ApiError`], and the words of
//! their replies to a decision.

use axum::http::StatusCode;
use droppedneedle_kernel::error::{ApiError, Unavailable};

use super::error::{QUOTA_EXCEEDED, RequestsError, STORAGE_FULL};
use super::models::{ActionResponse, BatchCancelResponse};
use super::service::{BatchCancelled, Decided, MixDecided};

/// Status, code and details for each requests error. Quota and storage
/// refusals keep their details for the Requests page, a busy database is
/// `SERVER_BUSY`, and a fault's cause goes to the log only.
impl From<RequestsError> for ApiError {
    fn from(error: RequestsError) -> Self {
        match error {
            RequestsError::Unauthorized { message } => ApiError::unauthorized(message),
            RequestsError::Forbidden { message } => ApiError::forbidden(message),
            RequestsError::NotFound => ApiError::not_found(),
            RequestsError::InvalidInput { message } => ApiError::invalid_input(message),
            RequestsError::Conflict { message } => ApiError::conflict(message),
            RequestsError::QuotaExceeded { message, details } => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, QUOTA_EXCEEDED, message)
                    .with_details(details)
            }
            RequestsError::StorageFull { message, details } => {
                ApiError::new(StatusCode::FORBIDDEN, STORAGE_FULL, message).with_details(details)
            }
            RequestsError::Busy { operation } => ApiError::unavailable(
                Unavailable::ServerBusy,
                format_args!("database busy: {operation}"),
            ),
            RequestsError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

/// The reply to a decision on one ask.
pub(crate) fn decided_reply(outcome: Decided) -> ActionResponse {
    let success = outcome.applied();
    let message = match outcome {
        Decided::NotFound => "Request not found".to_owned(),
        Decided::NotWaiting { status } => {
            format!("Request is not awaiting approval (status: {status})")
        }
        Decided::Approved { title } => format!("Approved: {title}"),
        Decided::ApprovedNotStarted { title } => format!("Approved but failed to start: {title}"),
        Decided::ApprovedStale => "Approved request became stale".to_owned(),
        Decided::Rejected { title } => format!("Rejected: {title}"),
        Decided::CancelledRequest { title } => format!("Cancelled request for {title}"),
        Decided::CancelledDownload { title } => format!("Cancelled download of {title}"),
        Decided::Detached => {
            "Removed from your requests. The shared server request continues for another listener."
                .to_owned()
        }
        Decided::CannotCancel { status } => format!("Cannot cancel request with status '{status}'"),
        Decided::StopFailed => "Could not stop the download; try again".to_owned(),
        Decided::Changed => "Request changed while cancelling".to_owned(),
        Decided::RetryAwaitingApproval => "Retry submitted, awaiting admin approval".to_owned(),
        Decided::Retried { title } => format!("Re-requested {title}"),
        Decided::RetryNotStarted => "Retry failed to start download".to_owned(),
        Decided::RetryStale => "Retry request became stale".to_owned(),
        Decided::CannotRetry { status } => format!("Cannot retry request with status '{status}'"),
    };
    ActionResponse { success, message }
}

/// The reply to a decision on a personal-mix grant.
pub(crate) fn mix_reply(outcome: MixDecided) -> ActionResponse {
    let (success, message) = match outcome {
        MixDecided::NoMatch => (false, "No matching approval found"),
        MixDecided::Approved => (true, "Weekly Mix auto-request approved"),
        MixDecided::Rejected => (true, "Weekly Mix auto-request rejected"),
        MixDecided::Revoked => (true, "Weekly Mix auto-request revoked"),
    };
    ActionResponse {
        success,
        message: message.to_owned(),
    }
}

/// The reply to a batch cancel.
pub(crate) fn batch_cancel_reply(outcome: BatchCancelled) -> BatchCancelResponse {
    let BatchCancelled { cancelled, failed } = outcome;
    let mut message = format!("Cancelled {cancelled} requests");
    if failed > 0 {
        message.push_str(&format!(", {failed} failed"));
    }
    BatchCancelResponse {
        success: cancelled > 0,
        cancelled,
        failed,
        message,
    }
}
