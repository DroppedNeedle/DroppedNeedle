//! Thin admin handlers. Each answers one route and returns typed errors.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use super::{
    AdminSetup,
    error::{AdminError, ValidJson},
    models::{
        BackupListResponse, BackupRunResponse, CacheClearBody, CacheClearResponse,
        CacheStatsResponse, PrecacheRunResponse, ProviderStatsResponse, QueueStatsResponse,
        QuotaOverrideBody, QuotaResponse, RestoreReport,
    },
};

/// Backup listing, oldest first.
#[utoipa::path(
    get,
    path = "/api/v3/admin/backups",
    responses((status = 200, description = "Backups on disk", body = BackupListResponse))
)]
pub async fn list_backups(
    State(admin): State<AdminSetup>,
) -> Result<Json<BackupListResponse>, AdminError> {
    let backups = admin.backups.as_ref().ok_or_else(unwired_backups)?;
    super::backups::list_backups(backups.backup_dir()).map(Json)
}

/// Run one backup now.
#[utoipa::path(
    post,
    path = "/api/v3/admin/backups",
    responses((status = 201, description = "Backup completed", body = BackupRunResponse))
)]
pub async fn run_backup(
    State(admin): State<AdminSetup>,
) -> Result<(StatusCode, Json<BackupRunResponse>), AdminError> {
    let backups = admin.backups.as_ref().ok_or_else(unwired_backups)?;
    let report = super::backups::run_backup(backups).await?;
    Ok((StatusCode::CREATED, Json(report)))
}

/// Pre-restore verification report for one backup. Read-only.
#[utoipa::path(
    get,
    path = "/api/v3/admin/backups/{name}/restore-report",
    params(("name" = String, Path, description = "Backup file name")),
    responses((status = 200, description = "Verification report", body = RestoreReport))
)]
pub async fn restore_report(
    State(admin): State<AdminSetup>,
    Path(name): Path<String>,
) -> Result<Json<RestoreReport>, AdminError> {
    let backups = admin.backups.as_ref().ok_or_else(unwired_backups)?;
    super::backups::restore_report(backups.backup_dir(), &name).map(Json)
}

/// Provider-cache counters.
#[utoipa::path(
    get,
    path = "/api/v3/admin/cache/stats",
    responses((status = 200, description = "Cache counters", body = CacheStatsResponse))
)]
pub async fn cache_stats(State(admin): State<AdminSetup>) -> Json<CacheStatsResponse> {
    Json(super::cache::cache_stats(&admin.cache).await)
}

/// Clear the provider cache: everything, or one source's keys.
#[utoipa::path(
    post,
    path = "/api/v3/admin/cache/clear",
    request_body = CacheClearBody,
    responses((status = 200, description = "Clear counts", body = CacheClearResponse))
)]
pub async fn clear_cache(
    State(admin): State<AdminSetup>,
    ValidJson(body): ValidJson<CacheClearBody>,
) -> Result<Json<CacheClearResponse>, AdminError> {
    super::cache::clear_cache(&admin.cache, &body)
        .await
        .map(Json)
}

/// Queue demand plus the job registry.
#[utoipa::path(
    get,
    path = "/api/v3/admin/queue-stats",
    responses((status = 200, description = "Queue snapshot", body = QueueStatsResponse))
)]
pub async fn queue_stats(
    State(admin): State<AdminSetup>,
) -> Result<Json<QueueStatsResponse>, AdminError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::queues::queue_stats(db).await.map(Json)
}

/// Provider limiter posture plus slot lanes.
#[utoipa::path(
    get,
    path = "/api/v3/admin/provider-stats",
    responses((status = 200, description = "Provider snapshot", body = ProviderStatsResponse))
)]
pub async fn provider_stats(State(admin): State<AdminSetup>) -> Json<ProviderStatsResponse> {
    Json(super::queues::provider_stats(&admin.providers))
}

/// One user's quota standing.
#[utoipa::path(
    get,
    path = "/api/v3/admin/users/{id}/quota",
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Quota standing", body = QuotaResponse))
)]
pub async fn get_quota(
    State(admin): State<AdminSetup>,
    Path(user_id): Path<String>,
) -> Result<Json<QuotaResponse>, AdminError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::quota::get_quota(&admin.users, db, &admin.quota, &user_id)
        .await
        .map(Json)
}

/// Start one supervised precache run. The run continues in the background
/// under the `precache-library` job name.
#[utoipa::path(
    post,
    path = "/api/v3/admin/precache/run",
    responses(
        (status = 202, description = "Precache run started", body = PrecacheRunResponse),
        (status = 409, description = "A precache run is already live"),
    )
)]
pub async fn run_precache(
    State(admin): State<AdminSetup>,
) -> Result<(StatusCode, Json<PrecacheRunResponse>), AdminError> {
    let trigger = admin.precache.as_ref().ok_or_else(unwired_precache)?;
    match trigger.run().await {
        Ok(_) => Ok((
            StatusCode::ACCEPTED,
            Json(PrecacheRunResponse {
                job: crate::jobs::precache::JOB_NAME.to_owned(),
                status: "started".to_owned(),
            }),
        )),
        Err(_) => Err(AdminError::Conflict {
            message: "A precache run is already live".to_owned(),
        }),
    }
}

/// Set (or, with all-`None`, clear) one user's quota overrides.
#[utoipa::path(
    put,
    path = "/api/v3/admin/users/{id}/quota",
    request_body = QuotaOverrideBody,
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Fresh quota standing", body = QuotaResponse))
)]
pub async fn set_quota(
    State(admin): State<AdminSetup>,
    Path(user_id): Path<String>,
    ValidJson(body): ValidJson<QuotaOverrideBody>,
) -> Result<Json<QuotaResponse>, AdminError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::quota::set_quota(&admin.users, db, &admin.quota, &user_id, &body)
        .await
        .map(Json)
}

/// 503 for backup routes on states without a backup service.
fn unwired_backups() -> AdminError {
    AdminError::Unavailable {
        message: "Backup service is not wired on this state".to_owned(),
    }
}

/// 503 for the precache route on states without the trigger.
fn unwired_precache() -> AdminError {
    AdminError::Unavailable {
        message: "Precache trigger is not wired on this state".to_owned(),
    }
}

/// 503 for database routes on states without database handles.
fn unwired_db() -> AdminError {
    AdminError::Unavailable {
        message: "Admin database handles are not wired on this state".to_owned(),
    }
}
