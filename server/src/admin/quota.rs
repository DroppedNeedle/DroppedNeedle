//! Quota admin: per-user overrides, effective values, and usage.
//!
//! Ported from v2 `quota_service.py` + the `admin/users/{id}/quota` pair.
//! The live ledger stays the enforcement truth (acquisition reads it at
//! submit and dispatch); this module is the admin face that edits and
//! inspects it:
//!
//! - Overrides persist in `user_quotas` (all-`None` deletes the row back to
//!   pure inherit, the v2 `set` rule) and apply to the ledger at once, so
//!   an edited quota bites on the next request. Boot reloads the table into
//!   the ledger ([`reload_overrides`]).
//! - Bounds mirror v2: count 0–1M, days 1–1M, storage 0–1M; 0 means
//!   unlimited.
//! - Usage reads the enforcement view: window counts from the ledger's
//!   recorded asks, storage from landed download bytes by owner.
//!   Scan-discovered files stay unowned (the v2 A5 rule). Storage reads 0
//!   until download tasks record their sizes (a stage-7 follow-up); the
//!   query is live so the bars fill in with no admin change.
//! - `admin`/`trusted` are exempt from per-user quotas (v2 D8).
//!
//! One clean-slate break: unknown users answer 404 on both routes, matching
//! the sibling user-admin routes. v2 answered 200-with-zeros on GET and
//! blew up on the FK on PUT; neither helps an admin editor.

use std::time::{SystemTime, UNIX_EPOCH};

use super::models::{QuotaOverrideView, QuotaResponse};
use super::{AdminDb, error::AdminError, models::QuotaOverrideBody};
use crate::{
    acquire::requests::quota::{QuotaLedger, QuotaOverride},
    auth::users::UsersDeps,
    db::writer::Lane,
};

/// Top of every quota bound (the v2 1M ceiling).
const QUOTA_MAX: u64 = 1_000_000;

/// One raw override row: user, count, days, storage.
type OverrideRow = (String, Option<i64>, Option<i64>, Option<i64>);

/// One user's quota standing for the admin editor.
pub async fn get_quota(
    users: &UsersDeps,
    db: &AdminDb,
    ledger: &QuotaLedger,
    user_id: &str,
) -> Result<QuotaResponse, AdminError> {
    let user = users
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| AdminError::internal(&format_args!("quota user read failed: {error}")))?
        .ok_or(AdminError::NotFound)?;
    let row = read_override(db, user_id).await?;
    let effective = ledger.effective_quota(user_id).map_err(|error| {
        AdminError::internal(&format_args!("effective quota failed: {error:?}"))
    })?;
    let in_window = ledger
        .count_in_window(user_id, effective.request_days, now_epoch())
        .map_err(|error| {
            AdminError::internal(&format_args!("quota window count failed: {error:?}"))
        })?;
    let storage_bytes = user_storage_bytes(db, user_id).await?;
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
/// fresh standing. The table persists first so a failed write never leaves
/// the ledger ahead of the database; the ledger follows, and `check_bounds`
/// already answered 400 above, so the ledger cannot refuse the input.
pub async fn set_quota(
    users: &UsersDeps,
    db: &AdminDb,
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
    check_bounds(body)?;
    let row = QuotaOverride {
        request_count: body.request_quota_count,
        request_days: body.request_quota_days,
        storage_gb: body.storage_quota_gb,
    };
    // Persist first: a failed table write answers before the ledger moves,
    // so the two never diverge. The ledger cannot refuse below — bounds
    // already passed — except an InvalidInput if the two bound sites ever
    // drift apart, which stays 400.
    write_override(db, &user.id, &row).await?;
    if let Err(error) = ledger.set_override(&user.id, row.clone()) {
        if let crate::acquire::requests::error::RequestsError::InvalidInput { message } = error {
            return Err(AdminError::InvalidInput { message });
        }
        return Err(AdminError::internal(&format_args!(
            "quota ledger write failed: {error:?}"
        )));
    }
    get_quota(users, db, ledger, &user.id).await
}

/// Reload every stored override into the ledger. Boot calls this after
/// migrations so durable edits survive restarts. Rows that no longer pass
/// bounds (hand-edited behind the API's back) are skipped with a warning,
/// never fatal — one bad row must not brick the boot.
pub async fn reload_overrides(
    pool: &sqlx::SqlitePool,
    ledger: &QuotaLedger,
) -> Result<usize, crate::db::error::DbError> {
    let rows: Vec<OverrideRow> =
        sqlx::query_as("SELECT user_id, request_quota_count, request_quota_days, storage_quota_gb FROM user_quotas")
            .fetch_all(pool)
            .await?;
    let mut loaded = 0;
    for (user_id, count, days, storage) in rows {
        let row = QuotaOverride {
            request_count: count.and_then(|value| u32::try_from(value).ok()),
            request_days: days.and_then(|value| u32::try_from(value).ok()),
            storage_gb: storage.and_then(|value| u64::try_from(value).ok()),
        };
        if ledger.set_override(&user_id, row).is_err() {
            tracing::warn!(user_id, "stored quota override fails bounds; skipping");
            continue;
        }
        loaded += 1;
    }
    Ok(loaded)
}

/// Stored override row for one user, or `None` when the user inherits fully.
async fn read_override(db: &AdminDb, user_id: &str) -> Result<Option<QuotaOverride>, AdminError> {
    let row: Option<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT request_quota_count, request_quota_days, storage_quota_gb
         FROM user_quotas WHERE user_id = ?1",
    )
    .bind(user_id)
    .fetch_optional(&db.pool)
    .await
    .map_err(|error| AdminError::internal(&format_args!("quota override read failed: {error}")))?;
    Ok(row.map(|(count, days, storage)| QuotaOverride {
        request_count: count.and_then(|value| u32::try_from(value).ok()),
        request_days: days.and_then(|value| u32::try_from(value).ok()),
        storage_gb: storage.and_then(|value| u64::try_from(value).ok()),
    }))
}

/// Persist one override row through the writer lane. All-`None` deletes the
/// row back to pure inherit.
async fn write_override(
    db: &AdminDb,
    user_id: &str,
    row: &QuotaOverride,
) -> Result<(), AdminError> {
    let user_id = user_id.to_owned();
    let row = row.clone();
    db.lane
        .write(Lane::Foreground, "admin-quota-set", move |tx| {
            if row.request_count.is_none() && row.request_days.is_none() && row.storage_gb.is_none()
            {
                tx.execute(
                    "DELETE FROM user_quotas WHERE user_id = ?1",
                    rusqlite::params![user_id],
                )?;
                return Ok(());
            }
            tx.execute(
                "INSERT INTO user_quotas
                     (user_id, request_quota_count, request_quota_days, storage_quota_gb)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (user_id) DO UPDATE SET
                     request_quota_count = excluded.request_quota_count,
                     request_quota_days = excluded.request_quota_days,
                     storage_quota_gb = excluded.storage_quota_gb",
                rusqlite::params![
                    user_id,
                    row.request_count.map(i64::from),
                    row.request_days.map(i64::from),
                    row.storage_gb.and_then(|value| i64::try_from(value).ok()),
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| match error {
            crate::db::error::DbError::WriteFailed { cause, .. }
                if cause.contains("FOREIGN KEY") =>
            {
                AdminError::NotFound
            }
            other => AdminError::internal(&format_args!("quota override write failed: {other}")),
        })?;
    Ok(())
}

/// Landed download bytes owned by one user. Completed tasks only; a failed
/// or cancelled pull never lands bytes. `NULL` sizes (tasks recorded
/// before sizes land) simply do not add up.
async fn user_storage_bytes(db: &AdminDb, user_id: &str) -> Result<u64, AdminError> {
    let total: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(total_size_bytes), 0) FROM download_tasks
         WHERE user_id = ?1 AND status = 'completed'",
    )
    .bind(user_id)
    .fetch_one(&db.pool)
    .await
    .map_err(|error| AdminError::internal(&format_args!("quota storage read failed: {error}")))?;
    Ok(total.unwrap_or(0).max(0) as u64)
}

/// v2 `set_override` bounds: count 0–1M, days 1–1M, storage 0–1M.
fn check_bounds(body: &QuotaOverrideBody) -> Result<(), AdminError> {
    let bounds = [
        (
            "request_quota_count",
            body.request_quota_count.map(u64::from),
            0_u64,
        ),
        (
            "request_quota_days",
            body.request_quota_days.map(u64::from),
            1_u64,
        ),
        ("storage_quota_gb", body.storage_quota_gb, 0_u64),
    ];
    for (name, value, low) in bounds {
        if let Some(value) = value
            && (value < low || value > QUOTA_MAX)
        {
            return Err(AdminError::InvalidInput {
                message: format!("{name} must be between {low} and {QUOTA_MAX}"),
            });
        }
    }
    Ok(())
}

/// Wall-clock now as unix seconds for the trailing-window math.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}
