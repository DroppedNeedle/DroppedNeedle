//! Download queue HTTP: the queue list and task views, the per-task
//! actions (cancel, retry, next source, reimport), the bulk actions
//! (clear, stop all retries, retry all failed), the activity summary, the
//! admin quarantine list, and manual album searches (start, view, pick,
//! dismiss, cancel). The held-import and upgrade routes live in
//! [`super::held_http`] and mount here.
//!
//! Every route sits inside the session gate. Tasks follow v2's ownership:
//! admins see and act on every task, everyone else on their own (another
//! user's task answers 403). Reimport and quarantine are admin only. Live
//! updates come over the shared event stream (`downloads.changed` and
//! `download_progress`), not a per-task stream.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    response::IntoResponse,
    routing,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::held::HeldImports;
use super::held_http::{self, ReviewState};
use super::queue::{DownloadQueue, QueueError, TaskFile, TaskView};
use super::queue_rows::{ListFilter, Viewer};
use super::state::TaskStatus;
use super::store::QuarantineRow;
use super::upgrades::Upgrades;
use crate::acquire::requests::{
    auth::Principal,
    error::RequestsError,
    http::{HttpError, ValidJson, ValidQuery},
};
use crate::acquire::search_jobs::candidates::SearchCandidateView;
use crate::acquire::search_jobs::{AlbumSearch, JobView, SearchJobError, SearchJobs, StartOutcome};
use crate::acquire::worker::{DownloadWorker, ReimportError};

/// Reimport outcome: the task as the import left it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReimportResponse {
    /// Whether the files reached the library (`completed` or `partial`).
    pub success: bool,
    /// Task status after the import (`completed`, `partial`, `failed`).
    pub status: String,
    /// Why the import did not complete, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Why a download sits where it does: one stable code, one plain sentence
/// and what to do about it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadReason {
    /// Stable machine code (`stalled`, `weak_match`, `no_source`, ...).
    pub code: String,
    /// What happened, in one sentence.
    pub text: String,
    /// What the person can do about it.
    pub action: String,
}

/// What the import decided about the task's newest landing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportDecisionView {
    /// `imported`, `partial`, `held`, `rejected` or `deferred`.
    pub outcome: String,
    /// Stable reason code, when the landing was not a clean import.
    pub reason_code: Option<String>,
    /// The reason as a plain sentence.
    pub reason_text: Option<String>,
    /// What to do about it.
    pub reason_action: Option<String>,
    /// Specifics (which file, which marker), never the explanation.
    pub detail: Option<String>,
    pub files_total: i64,
    pub files_imported: i64,
    pub files_held: i64,
    /// Decision time (unix seconds).
    pub decided_at: f64,
}

/// One download task as the queue shows it. Field names match v2's
/// `DownloadTaskResponse`, so the web UI reads them unchanged.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadTaskResponse {
    pub id: String,
    pub user_id: String,
    /// `album` or `track`.
    pub download_type: String,
    /// `soulseek`, `usenet` or `plugin:<key>`.
    pub source: String,
    /// `user`, `retry` or `upgrade`.
    pub origin: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub artist_mbid: Option<String>,
    pub artist_name: String,
    pub album_title: String,
    pub track_title: Option<String>,
    pub year: Option<i64>,
    /// `queued`, `downloading`, `processing`, `completed`, `partial`,
    /// `failed` or `cancelled`.
    pub status: String,
    pub progress_percent: i64,
    pub total_size_bytes: Option<i64>,
    pub downloaded_bytes: i64,
    pub files_total: i64,
    pub files_completed: i64,
    pub files_failed: i64,
    pub source_username: Option<String>,
    pub search_job_id: Option<String>,
    pub candidate_index: Option<i64>,
    pub final_path: Option<String>,
    /// The worker's last outcome as recorded. Show `reason` instead.
    pub error_message: Option<String>,
    pub retry_count: i64,
    pub created_at: f64,
    pub updated_at: f64,
    pub completed_at: Option<f64>,
    /// When the next automatic retry is due, if one will run.
    pub next_retry_at: Option<f64>,
    /// Automatic retries allowed in all (0 when auto-retry is off).
    pub retry_max: i64,
    /// The whole retry backoff schedule in minutes.
    pub retry_ladder_minutes: Vec<i64>,
    /// `not_tracked`, `in_use`, `pending`, `complete`, `preserved` or
    /// `needs_attention`: how far cleanup of the source files got.
    pub acquisition_cleanup_state: String,
    pub quality_format: Option<String>,
    pub quality_bitrate: Option<i64>,
    pub quality_bit_depth: Option<i64>,
    pub quality_sample_rate: Option<i64>,
    pub advertised_queue_depth: Option<i64>,
    pub queue_position_start: Option<i64>,
    pub queue_position_end: Option<i64>,
    /// Waiting in the peer's upload queue with no bytes yet.
    pub remote_queued: bool,
    /// Sources tried so far.
    pub attempt_number: i64,
    /// Sources the task may try in all.
    pub attempt_total: i64,
    /// Whether another source may still be tried.
    pub has_next_source: bool,
    /// Files of this task wait in the held list for a decision.
    pub held_for_review: bool,
    pub wrong_product_verdict_at: Option<f64>,
    pub wrong_product_detail: Option<String>,
    /// Why the task sits where it does, when that needs saying.
    pub reason: Option<DownloadReason>,
    /// The import's decision on the newest landing.
    pub decision: Option<ImportDecisionView>,
}

impl From<TaskView> for DownloadTaskResponse {
    fn from(view: TaskView) -> Self {
        let TaskView {
            row,
            facts,
            next_retry_at,
            retry_max,
            retry_ladder_minutes,
            attempt_total,
            reason,
        } = view;
        let (task, extras) = (row.task, row.extras);
        Self {
            id: task.id,
            user_id: task.user_id,
            download_type: task.download_type,
            source: task.source,
            origin: task.origin,
            release_group_mbid: task.release_group_mbid,
            release_mbid: extras.release_mbid,
            release_track_mbid: extras.release_track_mbid,
            recording_mbid: task.recording_mbid,
            artist_mbid: extras.artist_mbid,
            artist_name: task.artist_name,
            album_title: task.album_title,
            track_title: extras.track_title,
            year: extras.year,
            status: task.status.as_str().to_owned(),
            progress_percent: task.progress_percent,
            total_size_bytes: task.total_size_bytes,
            downloaded_bytes: task.downloaded_bytes,
            files_total: extras.files_total,
            files_completed: extras.files_completed,
            files_failed: extras.files_failed,
            source_username: task.source_username,
            search_job_id: task.search_job_id,
            candidate_index: task.candidate_index,
            final_path: extras.final_path,
            error_message: task.error_message,
            retry_count: task.retry_count,
            created_at: task.created_at,
            updated_at: task.updated_at,
            completed_at: task.completed_at,
            next_retry_at,
            retry_max,
            retry_ladder_minutes,
            acquisition_cleanup_state: facts
                .cleanup_state
                .unwrap_or_else(|| "not_tracked".to_owned()),
            quality_format: task.quality_format,
            quality_bitrate: extras.quality_bitrate,
            quality_bit_depth: extras.quality_bit_depth,
            quality_sample_rate: extras.quality_sample_rate,
            advertised_queue_depth: extras.advertised_queue_depth,
            queue_position_start: extras.queue_position_start,
            queue_position_end: extras.queue_position_end,
            remote_queued: extras.remote_queued,
            attempt_number: facts.attempts,
            attempt_total,
            has_next_source: facts.attempts < attempt_total,
            held_for_review: facts.held_for_review,
            wrong_product_verdict_at: extras.wrong_product_verdict_at,
            wrong_product_detail: extras.wrong_product_detail,
            reason: reason.map(|reason| DownloadReason {
                code: reason.code,
                text: reason.text,
                action: reason.action,
            }),
            decision: facts.decision.map(|decision| ImportDecisionView {
                outcome: decision.outcome,
                reason_code: decision.reason_code,
                reason_text: decision.reason_text,
                reason_action: decision.reason_action,
                detail: decision.detail,
                files_total: decision.files_total,
                files_imported: decision.files_imported,
                files_held: decision.files_held,
                decided_at: decision.decided_at,
            }),
        }
    }
}

/// One page of the queue.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadListResponse {
    pub items: Vec<DownloadTaskResponse>,
    pub page: i64,
    pub page_size: i64,
}

/// Queue list filters.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DownloadListQuery {
    /// Only tasks in this status.
    pub status: Option<String>,
    /// Only tasks for this release group.
    pub release_group_mbid: Option<String>,
    /// 1-based page (default 1).
    pub page: Option<i64>,
    /// Tasks per page, 1 to 100 (default 20).
    pub page_size: Option<i64>,
}

/// Queue counters for the nav badge and live refresh.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadActivitySummaryResponse {
    /// Moves whenever the viewer's queue changes structurally (not on
    /// byte progress). Refetch the list when it moves.
    pub revision: i64,
    pub active_count: i64,
    pub held_count: i64,
    pub failed_count: i64,
    /// The 20 albums that landed most recently, newest first.
    pub landed_release_group_mbids: Vec<String>,
}

/// One file of a task's current source.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadFileItem {
    pub filename: String,
    /// Advertised size in bytes, when the source gave one.
    pub size: Option<i64>,
    /// Always empty: sources do not advertise durations.
    pub duration: Option<f64>,
}

/// A task's files and counts.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadFilesResponse {
    pub task_id: String,
    pub status: String,
    pub files_total: i64,
    pub files_completed: i64,
    pub files_failed: i64,
    pub progress_percent: i64,
    pub files: Vec<DownloadFileItem>,
}

/// Plain success.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DownloadActionResponse {
    pub success: bool,
}

/// Next-source request: the source the page showed, so a stale click
/// cannot skip the source that replaced it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NextSourceRequest {
    pub expected_candidate_index: i64,
}

/// Next-source outcome: the task after the switch.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NextSourceResponse {
    pub success: bool,
    pub status: String,
    pub candidate_index: Option<i64>,
}

/// Retry outcome: the new task carrying the retry.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RetryDownloadResponse {
    pub success: bool,
    pub task_id: String,
}

/// Clear outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ClearDownloadsResponse {
    /// Finished tasks removed.
    pub cleared: i64,
}

/// Stop-all-retries outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StopRetriesResponse {
    /// Scheduled retries stopped.
    pub stopped: i64,
}

/// Retry-all-failed outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RetryAllResponse {
    /// Failed downloads started again.
    pub retried: i64,
}

/// One blocklisted release.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuarantineEntry {
    pub id: i64,
    /// `soulseek`, `usenet` or `plugin:<key>`.
    pub source: String,
    /// v2's client label: `slskd`, `sabnzbd`, or the source for plugins.
    pub client_id: String,
    /// Soulseek peer, empty for other sources.
    pub username: String,
    /// The blocked file (soulseek) or job (usenet).
    pub filename: String,
    /// The stored identity, as matched.
    pub identity: String,
    /// Stable reason code.
    pub reason: String,
    /// The reason as a plain sentence.
    pub reason_text: String,
    pub quarantined_at: f64,
    pub release_group_mbid: Option<String>,
}

impl From<QuarantineRow> for QuarantineEntry {
    fn from(row: QuarantineRow) -> Self {
        let (username, filename) = match (row.source.as_str(), row.identity.split_once('/')) {
            ("soulseek", Some((user, file))) => (user.to_owned(), file.to_owned()),
            _ => (String::new(), row.identity.clone()),
        };
        let client_id = match row.source.as_str() {
            "soulseek" => "slskd".to_owned(),
            "usenet" => "sabnzbd".to_owned(),
            other => other.to_owned(),
        };
        Self {
            id: row.id,
            reason_text: quarantine_reason_text(&row.reason).to_owned(),
            source: row.source,
            client_id,
            username,
            filename,
            identity: row.identity,
            reason: row.reason,
            quarantined_at: row.quarantined_at,
            release_group_mbid: row.release_group_mbid,
        }
    }
}

/// The blocklist reason as a sentence.
fn quarantine_reason_text(code: &str) -> &'static str {
    match code {
        "verify_failed" => "The files failed the import checks.",
        "corrupt" => "The files were damaged or not audio.",
        "fingerprint_mismatch" => "The audio was a different recording than named.",
        "duration_mismatch" => "The track lengths did not match the album.",
        "download_failed" => "The download from this source failed.",
        "manual" => "Blocked by an admin.",
        _ => "Blocked after a failed download.",
    }
}

/// One page of the blocklist.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuarantineListResponse {
    pub items: Vec<QuarantineEntry>,
    pub page: i64,
}

/// Quarantine paging.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct QuarantineQuery {
    /// 1-based page (default 1).
    pub page: Option<i64>,
    /// Entries per page, 1 to 200 (default 50).
    pub page_size: Option<i64>,
}

/// Router state: the queue service and the worker it runs on.
#[derive(Clone)]
pub struct DownloadsState {
    queue: DownloadQueue,
    worker: Arc<DownloadWorker>,
}

fn viewer(principal: &Principal) -> Viewer {
    Viewer {
        user_id: principal.user_id.clone(),
        admin: principal.role.is_admin(),
    }
}

impl From<QueueError> for HttpError {
    fn from(error: QueueError) -> Self {
        HttpError(match error {
            QueueError::NotFound => RequestsError::NotFound,
            QueueError::Forbidden(message) => RequestsError::Forbidden {
                message: message.to_owned(),
            },
            QueueError::Conflict(message) => RequestsError::Conflict { message },
            QueueError::Unavailable(cause) => RequestsError::internal(&cause),
        })
    }
}

fn page_of(value: Option<i64>, default: i64, max: i64, name: &str) -> Result<i64, HttpError> {
    let value = value.unwrap_or(default);
    if value < 1 || value > max {
        return Err(HttpError(RequestsError::InvalidInput {
            message: format!("{name} must be between 1 and {max}"),
        }));
    }
    Ok(value)
}

/// The viewer's download queue, newest first. Admins see every user's
/// tasks. `GET /api/v3/downloads/tasks`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/tasks",
    params(DownloadListQuery),
    responses((status = 200, body = DownloadListResponse))
)]
pub async fn list_tasks_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    ValidQuery(query): ValidQuery<DownloadListQuery>,
) -> Result<Json<DownloadListResponse>, HttpError> {
    let status = match query.status.as_deref() {
        None | Some("") => None,
        Some(text) => Some(TaskStatus::parse(text).ok_or_else(|| {
            HttpError(RequestsError::InvalidInput {
                message: format!("Unknown download status '{text}'"),
            })
        })?),
    };
    let filter = ListFilter {
        status,
        release_group_mbid: query.release_group_mbid.filter(|mbid| !mbid.is_empty()),
        page: page_of(query.page, 1, i64::MAX, "page")?,
        page_size: page_of(query.page_size, 20, 100, "page_size")?,
    };
    let items = state.queue.list(&viewer(&principal), &filter).await?;
    Ok(Json(DownloadListResponse {
        items: items.into_iter().map(Into::into).collect(),
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// One download task. `GET /api/v3/downloads/tasks/{task_id}`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/tasks/{task_id}",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = DownloadTaskResponse))
)]
pub async fn get_task_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<DownloadTaskResponse>, HttpError> {
    let view = state.queue.get(&viewer(&principal), &task_id).await?;
    Ok(Json(view.into()))
}

/// The files of a task's current source.
/// `GET /api/v3/downloads/tasks/{task_id}/files`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/tasks/{task_id}/files",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = DownloadFilesResponse))
)]
pub async fn task_files_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<DownloadFilesResponse>, HttpError> {
    let (row, files) = state.queue.files(&viewer(&principal), &task_id).await?;
    Ok(Json(DownloadFilesResponse {
        task_id: row.task.id,
        status: row.task.status.as_str().to_owned(),
        files_total: row.extras.files_total,
        files_completed: row.extras.files_completed,
        files_failed: row.extras.files_failed,
        progress_percent: row.task.progress_percent,
        files: files
            .into_iter()
            .map(|TaskFile { filename, size }| DownloadFileItem {
                filename,
                size,
                duration: None,
            })
            .collect(),
    }))
}

/// Stop a download: a live one is cancelled and its transfer aborted; a
/// failed one stops retrying. The owner's wanted watch on the album stops
/// too. `POST /api/v3/downloads/tasks/{task_id}/cancel`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/tasks/{task_id}/cancel",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = DownloadActionResponse))
)]
pub async fn cancel_task_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<DownloadActionResponse>, HttpError> {
    state.queue.cancel(&viewer(&principal), &task_id).await?;
    Ok(Json(DownloadActionResponse { success: true }))
}

/// Move a transfer that waits in a Soulseek peer's queue (no bytes yet)
/// to the next ranked source.
/// `POST /api/v3/downloads/tasks/{task_id}/next-source`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/tasks/{task_id}/next-source",
    params(("task_id" = String, Path, description = "Download task id")),
    request_body = NextSourceRequest,
    responses((status = 200, body = NextSourceResponse))
)]
pub async fn next_source_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(task_id): Path<String>,
    ValidJson(body): ValidJson<NextSourceRequest>,
) -> Result<Json<NextSourceResponse>, HttpError> {
    let task = state
        .queue
        .next_source(&viewer(&principal), &task_id, body.expected_candidate_index)
        .await?;
    Ok(Json(NextSourceResponse {
        success: true,
        status: task.status.as_str().to_owned(),
        candidate_index: task.candidate_index,
    }))
}

/// Retry a failed, cancelled or partial download now. Answers the new
/// task. An album retry clears the album's blocklist first.
/// `POST /api/v3/downloads/tasks/{task_id}/retry`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/tasks/{task_id}/retry",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = RetryDownloadResponse))
)]
pub async fn retry_task_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<Json<RetryDownloadResponse>, HttpError> {
    let task_id = state.queue.retry(&viewer(&principal), &task_id).await?;
    Ok(Json(RetryDownloadResponse {
        success: true,
        task_id,
    }))
}

/// Remove finished (completed and cancelled) downloads from the queue.
/// Admins clear every user's. `POST /api/v3/downloads/clear`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/clear",
    responses((status = 200, body = ClearDownloadsResponse))
)]
pub async fn clear_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
) -> Result<Json<ClearDownloadsResponse>, HttpError> {
    let cleared = state.queue.clear(&viewer(&principal)).await?;
    Ok(Json(ClearDownloadsResponse { cleared }))
}

/// Stop every scheduled automatic retry.
/// `POST /api/v3/downloads/stop-all-retries`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/stop-all-retries",
    responses((status = 200, body = StopRetriesResponse))
)]
pub async fn stop_all_retries_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
) -> Result<Json<StopRetriesResponse>, HttpError> {
    let stopped = state.queue.stop_all_retries(&viewer(&principal)).await?;
    Ok(Json(StopRetriesResponse { stopped }))
}

/// Retry every failed download that will not retry by itself, once per
/// album or track. `POST /api/v3/downloads/retry-all-failed`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/retry-all-failed",
    responses((status = 200, body = RetryAllResponse))
)]
pub async fn retry_all_failed_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
) -> Result<Json<RetryAllResponse>, HttpError> {
    let retried = state.queue.retry_all_failed(&viewer(&principal)).await?;
    Ok(Json(RetryAllResponse { retried }))
}

/// Queue counters and the revision that tells the page to refetch.
/// `GET /api/v3/downloads/activity-summary`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/activity-summary",
    responses((status = 200, body = DownloadActivitySummaryResponse))
)]
pub async fn activity_summary_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
) -> Result<Json<DownloadActivitySummaryResponse>, HttpError> {
    let summary = state.queue.activity(&viewer(&principal)).await?;
    Ok(Json(DownloadActivitySummaryResponse {
        revision: summary.revision,
        active_count: summary.active_count,
        held_count: summary.held_count,
        failed_count: summary.failed_count,
        landed_release_group_mbids: summary.landed_release_group_mbids,
    }))
}

/// The release blocklist, newest first. Admin only.
/// `GET /api/v3/downloads/quarantine`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/quarantine",
    params(QuarantineQuery),
    responses((status = 200, body = QuarantineListResponse))
)]
pub async fn list_quarantine_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    ValidQuery(query): ValidQuery<QuarantineQuery>,
) -> Result<Json<QuarantineListResponse>, HttpError> {
    principal.require_admin()?;
    let page = page_of(query.page, 1, i64::MAX, "page")?;
    let page_size = page_of(query.page_size, 50, 200, "page_size")?;
    let rows = state.queue.quarantine(page, page_size).await?;
    Ok(Json(QuarantineListResponse {
        items: rows.into_iter().map(Into::into).collect(),
        page,
    }))
}

/// Remove one blocklist entry so its release may be tried again. Admin
/// only. `DELETE /api/v3/downloads/quarantine/{quarantine_id}`.
#[utoipa::path(
    delete,
    path = "/api/v3/downloads/quarantine/{quarantine_id}",
    params(("quarantine_id" = i64, Path, description = "Quarantine entry id")),
    responses((status = 200, body = DownloadActionResponse))
)]
pub async fn delete_quarantine_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(quarantine_id): Path<String>,
) -> Result<Json<DownloadActionResponse>, HttpError> {
    // The role check comes before the id parse, so a non-admin always
    // hears 403 whatever the id.
    principal.require_admin()?;
    let id = quarantine_id
        .parse::<i64>()
        .map_err(|_| HttpError(RequestsError::NotFound))?;
    state.queue.delete_quarantine(id).await?;
    Ok(Json(DownloadActionResponse { success: true }))
}

/// Import a failed or short-landed task's files again, right away, from
/// the files its last download left on disk (no new search or download).
/// `POST /api/v3/downloads/tasks/{task_id}/reimport`. Admin only. Missing
/// tasks and tasks with no files to reimport answer 404; a download client
/// that is no longer configured answers 409.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/tasks/{task_id}/reimport",
    params(("task_id" = String, Path, description = "Download task id")),
    responses((status = 200, body = ReimportResponse))
)]
pub async fn reimport_task_handler(
    State(state): State<DownloadsState>,
    principal: Principal,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    principal.require_admin()?;
    let row = state
        .worker
        .reimport(&task_id)
        .await
        .map_err(|error| match error {
            ReimportError::Unavailable(message) => RequestsError::Conflict { message },
            ReimportError::Journal(cause) => RequestsError::internal(&cause),
        })?;
    let Some(row) = row else {
        return Err(RequestsError::NotFound.into());
    };
    let status = row.status.as_str().to_owned();
    Ok(Json(ReimportResponse {
        success: matches!(status.as_str(), "completed" | "partial"),
        status,
        error_message: row.error_message,
    }))
}

/// Queue routes without an auth layer. The app mounts this inside the
/// session gate under the shared principal-translation layer.
pub fn downloads_core_routes(worker: Arc<DownloadWorker>, upgrades: Upgrades) -> Router {
    let review = ReviewState {
        held: HeldImports::new(worker.clone()),
        upgrades,
    };
    let state = DownloadsState {
        queue: DownloadQueue::new(worker.clone()),
        worker,
    };
    let review = Router::new()
        .route(
            "/downloads/held",
            routing::get(held_http::list_held_handler),
        )
        .route(
            "/downloads/held/reverify",
            routing::post(held_http::reverify_bulk_handler),
        )
        .route(
            "/downloads/held/{held_id}/import",
            routing::post(held_http::import_held_handler),
        )
        .route(
            "/downloads/held/{held_id}/discard",
            routing::post(held_http::discard_held_handler),
        )
        .route(
            "/downloads/held/{held_id}/reverify",
            routing::post(held_http::reverify_held_handler),
        )
        .route(
            "/downloads/held/{held_id}/audio",
            routing::get(held_http::held_audio_handler),
        )
        .route(
            "/downloads/held/management/{source_task_id}/retry",
            routing::post(held_http::retry_local_handler),
        )
        .route(
            "/downloads/held/management/{source_task_id}/discard",
            routing::post(held_http::discard_local_handler),
        )
        .route(
            "/downloads/held/verdict/{source_task_id}/discard",
            routing::post(held_http::discard_verdict_handler),
        )
        .route(
            "/downloads/cutoff-unmet",
            routing::get(held_http::cutoff_unmet_handler),
        )
        .route(
            "/downloads/upgrade/album",
            routing::post(held_http::upgrade_album_handler),
        )
        .route(
            "/downloads/upgrade/track",
            routing::post(held_http::upgrade_track_handler),
        )
        .with_state(review);
    Router::new()
        .route("/downloads/tasks", routing::get(list_tasks_handler))
        .route("/downloads/tasks/{task_id}", routing::get(get_task_handler))
        .route(
            "/downloads/tasks/{task_id}/files",
            routing::get(task_files_handler),
        )
        .route(
            "/downloads/tasks/{task_id}/cancel",
            routing::post(cancel_task_handler),
        )
        .route(
            "/downloads/tasks/{task_id}/next-source",
            routing::post(next_source_handler),
        )
        .route(
            "/downloads/tasks/{task_id}/retry",
            routing::post(retry_task_handler),
        )
        .route(
            "/downloads/tasks/{task_id}/reimport",
            routing::post(reimport_task_handler),
        )
        .route("/downloads/clear", routing::post(clear_handler))
        .route(
            "/downloads/stop-all-retries",
            routing::post(stop_all_retries_handler),
        )
        .route(
            "/downloads/retry-all-failed",
            routing::post(retry_all_failed_handler),
        )
        .route(
            "/downloads/activity-summary",
            routing::get(activity_summary_handler),
        )
        .route(
            "/downloads/quarantine",
            routing::get(list_quarantine_handler),
        )
        .route(
            "/downloads/quarantine/{quarantine_id}",
            routing::delete(delete_quarantine_handler),
        )
        .with_state(state)
        .merge(review)
}

/// Queue routes behind the header test gate, with upgrades switched off.
#[cfg(any(test, feature = "test-support"))]
pub fn downloads_router(worker: Arc<DownloadWorker>) -> Router {
    let upgrades = Upgrades::new(
        crate::acquire::flows::stores::UpgradeWorklist::new(worker.journal().db().clone()),
        Arc::new(crate::acquire::flows::stores::UpgradePolicy::default),
        Arc::new(crate::acquire::flows::seams::ScriptedDownloads::new()),
    );
    downloads_core_routes(worker, upgrades).layer(axum::middleware::from_fn(
        crate::acquire::requests::auth::gate,
    ))
}

// Manual album searches.

/// Start a manual search for one album.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SearchAlbumRequest {
    /// Album artist.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Release year, when known.
    #[serde(default)]
    pub year: Option<i32>,
    /// Release group. An album the library already holds is not searched.
    #[serde(default)]
    pub release_group_mbid: Option<String>,
    /// The edition to rank folders against (its tracklist).
    #[serde(default)]
    pub release_mbid: Option<String>,
}

/// What starting a search did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchAlbumResponse {
    /// `searching`, or `already_in_library` when nothing was searched.
    pub status: String,
    /// The job to follow, while searching.
    pub job_id: Option<String>,
}

/// One manual search and what it found.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchJobResponse {
    pub job_id: String,
    /// `searching`, `completed`, `failed`, `matched` (a download started)
    /// or `cancelled`.
    pub status: String,
    pub artist_name: String,
    pub album_title: String,
    pub year: Option<i32>,
    pub release_group_mbid: Option<String>,
    /// The edition the candidates were ranked against.
    pub release_mbid: Option<String>,
    /// Tracks on that edition, when its tracklist could be read.
    pub tracks_total: Option<usize>,
    pub candidate_count: usize,
    /// Best first within each source; sources in the configured order.
    pub candidates: Vec<SearchCandidateView>,
    /// Why the search failed or found nothing, and what to do.
    pub reason: Option<DownloadReason>,
    /// The download a pick started.
    pub task_id: Option<String>,
}

/// Pick one candidate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PickRequest {
    /// `candidate_index` of the candidate to download.
    pub candidate_index: usize,
}

/// The download a pick started.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PickResponse {
    pub task_id: String,
}

/// "None of these": the album is on the watchlist.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DismissSearchResponse {
    pub success: bool,
    /// The album's watch state (`watching` normally).
    pub state: String,
}

impl From<SearchJobError> for HttpError {
    fn from(error: SearchJobError) -> Self {
        HttpError(match error {
            SearchJobError::NotFound => RequestsError::NotFound,
            SearchJobError::Forbidden => RequestsError::Forbidden {
                message: "You can only use your own searches.".to_owned(),
            },
            SearchJobError::Invalid(message) => RequestsError::InvalidInput { message },
            SearchJobError::Conflict(message) => RequestsError::Conflict { message },
            SearchJobError::Refused(error) => error,
            SearchJobError::Internal(cause) => RequestsError::internal(&cause),
        })
    }
}

impl From<JobView> for SearchJobResponse {
    fn from(view: JobView) -> Self {
        let reason = view.reason().map(|reason| DownloadReason {
            code: reason.code().to_owned(),
            text: reason.text().to_owned(),
            action: reason.action().to_owned(),
        });
        let payload = view.payload.unwrap_or_default();
        let candidates: Vec<SearchCandidateView> = payload
            .candidates
            .into_iter()
            .map(|candidate| candidate.view)
            .collect();
        Self {
            job_id: view.row.id,
            status: view.row.status,
            artist_name: view.row.artist_name,
            album_title: view.row.album_title,
            year: view.row.year,
            release_group_mbid: view.row.release_group_mbid,
            release_mbid: payload.release_mbid,
            tracks_total: payload.tracks_total,
            candidate_count: candidates.len(),
            candidates,
            reason,
            task_id: view.task_id,
        }
    }
}

/// Search every download source for one album. The search runs in the
/// background; follow it with `search_job_updated` events or by reading
/// the job. `POST /api/v3/downloads/search/album`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/search/album",
    request_body = SearchAlbumRequest,
    responses((status = 200, body = SearchAlbumResponse))
)]
pub async fn search_album_handler(
    State(search): State<Arc<SearchJobs>>,
    principal: Principal,
    ValidJson(body): ValidJson<SearchAlbumRequest>,
) -> Result<Json<SearchAlbumResponse>, HttpError> {
    let ask = AlbumSearch {
        artist_name: body.artist_name,
        album_title: body.album_title,
        year: body.year,
        release_group_mbid: body.release_group_mbid,
        release_mbid: body.release_mbid,
    };
    Ok(Json(match search.start(&principal, ask).await? {
        StartOutcome::Searching(job_id) => SearchAlbumResponse {
            status: "searching".to_owned(),
            job_id: Some(job_id),
        },
        StartOutcome::AlreadyInLibrary => SearchAlbumResponse {
            status: "already_in_library".to_owned(),
            job_id: None,
        },
    }))
}

/// One of your manual searches and its candidates.
/// `GET /api/v3/downloads/search/{job_id}`.
#[utoipa::path(
    get,
    path = "/api/v3/downloads/search/{job_id}",
    params(("job_id" = String, Path, description = "Search job id")),
    responses((status = 200, body = SearchJobResponse))
)]
pub async fn search_job_handler(
    State(search): State<Arc<SearchJobs>>,
    principal: Principal,
    Path(job_id): Path<String>,
) -> Result<Json<SearchJobResponse>, HttpError> {
    Ok(Json(search.get(&principal, &job_id).await?.into()))
}

/// Download one candidate. The download fetches exactly that candidate
/// first. `POST /api/v3/downloads/search/{job_id}/pick`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/search/{job_id}/pick",
    params(("job_id" = String, Path, description = "Search job id")),
    request_body = PickRequest,
    responses((status = 200, body = PickResponse))
)]
pub async fn pick_candidate_handler(
    State(search): State<Arc<SearchJobs>>,
    principal: Principal,
    Path(job_id): Path<String>,
    ValidJson(body): ValidJson<PickRequest>,
) -> Result<Json<PickResponse>, HttpError> {
    let task_id = search
        .pick(&principal, &job_id, body.candidate_index)
        .await?;
    Ok(Json(PickResponse { task_id }))
}

/// "None of these, keep watching": close the search and put the album on
/// the wanted watchlist. `POST /api/v3/downloads/search/{job_id}/dismiss`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/search/{job_id}/dismiss",
    params(("job_id" = String, Path, description = "Search job id")),
    responses((status = 200, body = DismissSearchResponse))
)]
pub async fn dismiss_search_handler(
    State(search): State<Arc<SearchJobs>>,
    principal: Principal,
    Path(job_id): Path<String>,
) -> Result<Json<DismissSearchResponse>, HttpError> {
    let state = search.dismiss(&principal, &job_id).await?;
    Ok(Json(DismissSearchResponse {
        success: true,
        state,
    }))
}

/// Close a search without downloading anything.
/// `POST /api/v3/downloads/search/{job_id}/cancel`.
#[utoipa::path(
    post,
    path = "/api/v3/downloads/search/{job_id}/cancel",
    params(("job_id" = String, Path, description = "Search job id")),
    responses((status = 200, body = DownloadActionResponse))
)]
pub async fn cancel_search_handler(
    State(search): State<Arc<SearchJobs>>,
    principal: Principal,
    Path(job_id): Path<String>,
) -> Result<Json<DownloadActionResponse>, HttpError> {
    search.cancel(&principal, &job_id).await?;
    Ok(Json(DownloadActionResponse { success: true }))
}

/// Manual search routes without an auth layer, mounted beside the queue
/// routes inside the session gate.
pub fn search_core_routes(search: Arc<SearchJobs>) -> Router {
    Router::new()
        .route(
            "/downloads/search/album",
            routing::post(search_album_handler),
        )
        .route(
            "/downloads/search/{job_id}",
            routing::get(search_job_handler),
        )
        .route(
            "/downloads/search/{job_id}/pick",
            routing::post(pick_candidate_handler),
        )
        .route(
            "/downloads/search/{job_id}/dismiss",
            routing::post(dismiss_search_handler),
        )
        .route(
            "/downloads/search/{job_id}/cancel",
            routing::post(cancel_search_handler),
        )
        .with_state(search)
}
