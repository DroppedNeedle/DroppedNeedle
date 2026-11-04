//! Quota admin: per-user overrides, effective values, and usage.
//!
//! The live ledger stays the enforcement truth (acquisition reads it at
//! submit and dispatch); this module is the admin face that edits and
//! inspects it:
//!
//! - Overrides are saved by the ledger, which owns `user_quotas`
//!   (all-`None` deletes the row back to pure inherit, the v2 `set` rule),
//!   and apply at once, so an edited quota bites on the next request. Boot
//!   reloads them into the ledger.
//! - Bounds mirror v2: count 0-1M, days 1-1M, storage 0-1M; 0 means
//!   unlimited.
//! - Usage reads the enforcement view: window counts from the ledger's
//!   recorded asks, storage from the ledger's own figure, the indexed
//!   bytes of the files the user's downloads landed. Scan-discovered files
//!   stay unowned (as in v2).
//! - `admin`/`trusted` are exempt from per-user quotas (as in v2).
//!
//! One clean-slate break: unknown users answer 404 on both routes, matching
//! the user-admin routes. v2 answered 200-with-zeros on GET and
//! blew up on the FK on PUT; neither helps an admin editor.

use super::error::AdminError;
use super::models::{QuotaOverrideBody, QuotaOverrideView, QuotaResponse};
use droppedneedle_acquire::requests::error::RequestsError;
use droppedneedle_acquire::requests::quota::{QuotaLedger, QuotaOverride};
use droppedneedle_auth::users::UsersDeps;

/// One user's quota standing for the admin editor.
pub async fn get_quota(
    users: &UsersDeps,
    ledger: &QuotaLedger,
    user_id: &str,
) -> Result<QuotaResponse, AdminError> {
    let user = users
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| AdminError::internal(&format_args!("quota user read failed: {error}")))?
        .ok_or(AdminError::NotFound)?;
    let row = ledger
        .saved_override(user_id)
        .await
        .map_err(|error| ledger_error("quota override read", error))?;
    let effective = ledger.effective_quota(user_id).map_err(|error| {
        AdminError::internal(&format_args!("effective quota failed: {error:?}"))
    })?;
    let in_window = ledger
        .count_in_window(
            user_id,
            effective.request_days,
            u64::try_from(users.clock.now_unix()).unwrap_or(0),
        )
        .await
        .map_err(|error| {
            AdminError::internal(&format_args!("quota window count failed: {error:?}"))
        })?;
    let storage_bytes = ledger
        .user_bytes(user_id)
        .await
        .map_err(|error| ledger_error("quota storage read", error))?;
    Ok(QuotaResponse {
        user_id: user.id,
        quota_override: QuotaOverrideView {
            request_quota_count: row.as_ref().and_then(|found| found.request_count),
            request_quota_days: row.as_ref().and_then(|found| found.request_days),
            storage_quota_gb: row.as_ref().and_then(|found| found.storage_gb),
        },
        effective_request_quota_count: effective.request_count,
        effective_request_quota_days: effective.request_days,
        effective_storage_quota_gb: effective.storage_gb,
        requests_in_window: in_window,
        storage_bytes,
        exempt: user.role.is_curator(),
    })
}

/// Set (or, with all-`None`, clear) one user's overrides, then return the
/// fresh standing. The ledger refuses values out of bounds (a 400) before
/// it writes anything, and writes the row before it applies it, so the two
/// never diverge.
pub async fn set_quota(
    users: &UsersDeps,
    ledger: &QuotaLedger,
    user_id: &str,
    body: &QuotaOverrideBody,
) -> Result<QuotaResponse, AdminError> {
    let user = users
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| AdminError::internal(&format_args!("quota user read failed: {error}")))?
        .ok_or(AdminError::NotFound)?;
    let row = QuotaOverride {
        request_count: body.request_quota_count,
        request_days: body.request_quota_days,
        storage_gb: body.storage_quota_gb,
    };
    ledger
        .save_override(&user.id, row)
        .await
        .map_err(|error| ledger_error("quota override write", error))?;
    get_quota(users, ledger, &user.id).await
}

/// A ledger failure as the admin routes answer it: an unknown user is a
/// 404, input the bounds refuse a 400, and the rest a server fault.
fn ledger_error(operation: &str, error: RequestsError) -> AdminError {
    match error {
        RequestsError::NotFound => AdminError::NotFound,
        RequestsError::InvalidInput { message } => AdminError::InvalidInput { message },
        other => AdminError::internal(&format_args!("{operation} failed: {other:?}")),
    }
}
