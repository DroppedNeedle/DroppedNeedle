//! Thin admin handlers. Each answers one route; the one
//! `From<AdminError> for ApiError` here gives admin errors a status.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use droppedneedle_auth::{Admin, Curator, User};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;

use super::{
    AdminSetup,
    error::AdminError,
    models::{
        BackupListResponse, BackupRunResponse, CacheClearBody, CacheClearResponse,
        CacheStatsResponse, CacheSyncCancelResponse, PrecacheRunResponse, ProviderStatsResponse,
        QueueStatsResponse, QuotaOverrideBody, QuotaResponse, RestoreReport,
    },
};

impl From<AdminError> for ApiError {
    fn from(error: AdminError) -> Self {
        match error {
            AdminError::NotFound => ApiError::not_found(),
            AdminError::InvalidInput { message } => ApiError::invalid_input(message),
            AdminError::Conflict { message } => ApiError::conflict(message),
            AdminError::Unavailable { message } => {
                ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, message)
            }
            AdminError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

/// Backup listing, oldest first.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/backups",
    responses((status = 200, description = "Backups on disk", body = BackupListResponse))
)]
pub async fn list_backups(
    State(admin): State<AdminSetup>,
    _caller: Admin,
) -> Result<Json<BackupListResponse>, ApiError> {
    let dir = admin
        .backups
        .as_ref()
        .ok_or_else(unwired_backups)?
        .backup_dir()
        .to_owned();
    blocking(move || super::backups::list_backups(&dir))
        .await
        .map(Json)
}

/// Run one backup now.
#[utoipa::path(
    post,
    tag = "admin",
    path = "/api/v3/admin/backups",
    responses((status = 201, description = "Backup completed", body = BackupRunResponse))
)]
pub async fn run_backup(
    State(admin): State<AdminSetup>,
    _caller: Admin,
) -> Result<(StatusCode, Json<BackupRunResponse>), ApiError> {
    let backups = admin.backups.as_ref().ok_or_else(unwired_backups)?;
    let report = super::backups::run_backup(backups).await?;
    Ok((StatusCode::CREATED, Json(report)))
}

/// Pre-restore verification report for one backup. Read-only.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/backups/{name}/restore-report",
    params(("name" = String, Path, description = "Backup file name")),
    responses((status = 200, description = "Verification report", body = RestoreReport))
)]
pub async fn restore_report(
    State(admin): State<AdminSetup>,
    _caller: Admin,
    Path(name): Path<String>,
) -> Result<Json<RestoreReport>, ApiError> {
    let dir = admin
        .backups
        .as_ref()
        .ok_or_else(unwired_backups)?
        .backup_dir()
        .to_owned();
    blocking(move || super::backups::restore_report(&dir, &name))
        .await
        .map(Json)
}

/// Provider data and image cache counters.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/cache/stats",
    responses((status = 200, description = "Cache counters", body = CacheStatsResponse))
)]
pub async fn cache_stats(
    State(admin): State<AdminSetup>,
    _caller: Admin,
) -> Json<CacheStatsResponse> {
    Json(super::cache::cache_stats(&admin.cache, admin.covers.as_ref()).await)
}

/// Clear provider data, the cached images, or both.
#[utoipa::path(
    post,
    tag = "admin",
    path = "/api/v3/admin/cache/clear",
    request_body = CacheClearBody,
    responses((status = 200, description = "Clear counts", body = CacheClearResponse))
)]
pub async fn clear_cache(
    State(admin): State<AdminSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<CacheClearBody>,
) -> Result<Json<CacheClearResponse>, ApiError> {
    super::cache::clear_cache(
        &admin.cache,
        admin.db.as_ref(),
        admin.covers.as_ref(),
        &body,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

/// Queue demand, the job registry and the latest checkpoint pass.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/queue-stats",
    responses((status = 200, description = "Queue snapshot", body = QueueStatsResponse))
)]
pub async fn queue_stats(
    State(admin): State<AdminSetup>,
    _caller: Admin,
) -> Result<Json<QueueStatsResponse>, ApiError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    Ok(Json(
        super::queues::queue_stats(db, admin.checkpoint.as_ref()).await?,
    ))
}

/// Provider limiter posture plus the outbound calls counted since start.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/provider-stats",
    responses((status = 200, description = "Provider snapshot", body = ProviderStatsResponse))
)]
pub async fn provider_stats(
    State(admin): State<AdminSetup>,
    _caller: Admin,
) -> Json<ProviderStatsResponse> {
    Json(super::queues::provider_stats(&admin.providers))
}

/// One user's quota standing.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/users/{id}/quota",
    operation_id = "get_user_quota",
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Quota standing", body = QuotaResponse))
)]
pub async fn get_quota(
    State(admin): State<AdminSetup>,
    _caller: Admin,
    Path(user_id): Path<String>,
) -> Result<Json<QuotaResponse>, ApiError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::quota::get_quota(&admin.users, db, &admin.quota, &user_id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Start one supervised precache run. The run continues in the background
/// under the `precache-library` job name.
#[utoipa::path(
    post,
    tag = "admin",
    path = "/api/v3/admin/precache/run",
    responses(
        (status = 202, description = "Precache run started", body = PrecacheRunResponse),
        (status = 409, description = "A precache run is already live"),
    )
)]
pub async fn run_precache(
    State(admin): State<AdminSetup>,
    caller: Admin,
) -> Result<(StatusCode, Json<PrecacheRunResponse>), ApiError> {
    let trigger = admin.precache.as_ref().ok_or_else(unwired_precache)?;
    match trigger.run(Some(caller.user_id().to_owned())).await {
        Ok(_) => Ok((
            StatusCode::ACCEPTED,
            Json(PrecacheRunResponse {
                job: droppedneedle_kernel::jobs::precache::JOB_NAME.to_owned(),
                status: "started".to_owned(),
            }),
        )),
        Err(_) => Err(AdminError::Conflict {
            message: "A precache run is already live".to_owned(),
        }
        .into()),
    }
}

/// Where the library image precache stands. The same shape streams live
/// as the `cache.sync` event.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/cache/sync/status",
    responses((status = 200, description = "Precache progress", body = droppedneedle_kernel::events::CacheSyncProgress))
)]
pub async fn cache_sync_status(
    State(admin): State<AdminSetup>,
    _caller: User,
) -> Json<droppedneedle_kernel::events::CacheSyncProgress> {
    Json(
        admin
            .precache
            .as_ref()
            .map(|trigger| trigger.status().snapshot())
            .unwrap_or_default(),
    )
}

/// Stop the running library image precache. Images already fetched stay
/// cached, so the next run continues from there. Succeeds when nothing is
/// running too.
#[utoipa::path(
    post,
    tag = "admin",
    path = "/api/v3/cache/sync/cancel",
    responses((status = 200, description = "Precache stopped", body = CacheSyncCancelResponse))
)]
pub async fn cancel_cache_sync(
    State(admin): State<AdminSetup>,
    _caller: Curator,
) -> Json<CacheSyncCancelResponse> {
    if let Some(trigger) = &admin.precache {
        trigger.cancel().await;
    }
    Json(CacheSyncCancelResponse {
        status: "cancelled".to_owned(),
    })
}

/// Set (or, with all-`None`, clear) one user's quota overrides.
#[utoipa::path(
    put,
    tag = "admin",
    path = "/api/v3/admin/users/{id}/quota",
    request_body = QuotaOverrideBody,
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Fresh quota standing", body = QuotaResponse))
)]
pub async fn set_quota(
    State(admin): State<AdminSetup>,
    _caller: Admin,
    Path(user_id): Path<String>,
    ValidJson(body): ValidJson<QuotaOverrideBody>,
) -> Result<Json<QuotaResponse>, ApiError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::quota::set_quota(&admin.users, db, &admin.quota, &user_id, &body)
        .await
        .map(Json)
        .map_err(ApiError::from)
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

/// Run blocking backup-directory work on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AdminError> + Send + 'static,
) -> Result<T, ApiError> {
    let outcome = tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| AdminError::internal(&format_args!("backup task failed: {cause}")))?;
    Ok(outcome?)
}
