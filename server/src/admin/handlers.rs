//! Thin admin handlers. Each answers one route; [`AdminHttpError`] is the
//! one place admin errors get a status and envelope.

use axum::{
    Json,
    extract::{FromRequest, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

use crate::error::{
    CONFLICT, FIXED_INTERNAL_MESSAGE, FORBIDDEN, INTERNAL_ERROR, INVALID_INPUT, NOT_FOUND,
    NOT_FOUND_MESSAGE, UPSTREAM_ERROR, envelope_response, fault_response, unauthorized_response,
};

use super::{
    AdminSetup,
    error::AdminError,
    models::{
        BackupListResponse, BackupRunResponse, CacheClearBody, CacheClearResponse,
        CacheStatsResponse, PrecacheRunResponse, ProviderStatsResponse, QueueStatsResponse,
        QuotaOverrideBody, QuotaResponse, RestoreReport,
    },
};

/// An admin failure on its way to the wire.
#[derive(Debug)]
pub struct AdminHttpError(pub AdminError);

impl From<AdminError> for AdminHttpError {
    fn from(error: AdminError) -> Self {
        Self(error)
    }
}

impl IntoResponse for AdminHttpError {
    fn into_response(self) -> Response {
        match self.0 {
            AdminError::Unauthorized { message } => unauthorized_response(message),
            AdminError::Forbidden { message } => {
                envelope_response(StatusCode::FORBIDDEN, FORBIDDEN, message, None)
            }
            AdminError::NotFound => {
                envelope_response(StatusCode::NOT_FOUND, NOT_FOUND, NOT_FOUND_MESSAGE, None)
            }
            AdminError::InvalidInput { message } => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            AdminError::Conflict { message } => {
                envelope_response(StatusCode::CONFLICT, CONFLICT, message, None)
            }
            AdminError::Unavailable { message } => envelope_response(
                StatusCode::SERVICE_UNAVAILABLE,
                UPSTREAM_ERROR,
                message,
                None,
            ),
            AdminError::Internal { error_id } => fault_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR,
                FIXED_INTERNAL_MESSAGE,
                &error_id,
            ),
        }
    }
}

/// JSON body extractor that keeps malformed input inside the shared envelope
/// instead of Axum's default plain-text 400.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = AdminHttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| {
                AdminHttpError(AdminError::InvalidInput {
                    message: format!("Invalid request body: {cause}"),
                })
            })
    }
}

/// Backup listing, oldest first.
#[utoipa::path(
    get,
    path = "/api/v3/admin/backups",
    responses((status = 200, description = "Backups on disk", body = BackupListResponse))
)]
pub async fn list_backups(
    State(admin): State<AdminSetup>,
) -> Result<Json<BackupListResponse>, AdminHttpError> {
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
    path = "/api/v3/admin/backups",
    responses((status = 201, description = "Backup completed", body = BackupRunResponse))
)]
pub async fn run_backup(
    State(admin): State<AdminSetup>,
) -> Result<(StatusCode, Json<BackupRunResponse>), AdminHttpError> {
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
) -> Result<Json<RestoreReport>, AdminHttpError> {
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
) -> Result<Json<CacheClearResponse>, AdminHttpError> {
    super::cache::clear_cache(&admin.cache, &body)
        .await
        .map(Json)
        .map_err(AdminHttpError::from)
}

/// Queue demand plus the job registry.
#[utoipa::path(
    get,
    path = "/api/v3/admin/queue-stats",
    responses((status = 200, description = "Queue snapshot", body = QueueStatsResponse))
)]
pub async fn queue_stats(
    State(admin): State<AdminSetup>,
) -> Result<Json<QueueStatsResponse>, AdminHttpError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    Ok(Json(super::queues::queue_stats(db).await?))
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
) -> Result<Json<QuotaResponse>, AdminHttpError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::quota::get_quota(&admin.users, db, &admin.quota, &user_id)
        .await
        .map(Json)
        .map_err(AdminHttpError::from)
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
) -> Result<(StatusCode, Json<PrecacheRunResponse>), AdminHttpError> {
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
        }
        .into()),
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
) -> Result<Json<QuotaResponse>, AdminHttpError> {
    let db = admin.db.as_ref().ok_or_else(unwired_db)?;
    super::quota::set_quota(&admin.users, db, &admin.quota, &user_id, &body)
        .await
        .map(Json)
        .map_err(AdminHttpError::from)
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
) -> Result<T, AdminHttpError> {
    let outcome = tokio::task::spawn_blocking(work)
        .await
        .map_err(|cause| AdminError::internal(&format_args!("backup task failed: {cause}")))?;
    Ok(outcome?)
}
