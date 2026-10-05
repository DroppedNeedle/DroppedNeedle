//! Auto-download approval reads. Admin only.
//!
//! These are the read paths over pending auto-download requests filed through
//! the follow toggles. Approve, reject, and revoke mutations live in
//! `acquire::requests`.

use axum::{Json, extract::State};

use super::{
    auth::Principal,
    error::CollectionsError,
    models::{
        ApprovalBatchItem, ApprovalBatchListResponse, AutoDownloadApprovalItem,
        AutoDownloadApprovalListResponse,
    },
    state::{AutoDownloadState, CollectionsState, read_store},
};

/// What produced a batch of follow-sourced requests.
const FOLLOW_SOURCE: &str = "follow_request";
/// How many artist names a batch card previews.
const BATCH_SAMPLE_COUNT: usize = 3;

/// List pending auto-download requests across users. Admin only. When
/// wiring connects the acquire approval store, reads share it with the
/// approval mutations; otherwise this reads the follow rows.
pub async fn list_approvals(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<AutoDownloadApprovalListResponse, CollectionsError> {
    state.check_injection()?;
    caller.require_admin()?;
    if let Some(source) = &state.acquire_approvals {
        let items = source
            .pending_approvals()
            .await
            .map_err(|cause| CollectionsError::internal(&cause))?
            .into_iter()
            .map(|row| AutoDownloadApprovalItem {
                user_id: row.user_id,
                user_name: Some(row.user_name),
                artist_mbid: row.artist_mbid,
                artist_name: row.artist_name,
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        let count = items.len();
        return Ok(AutoDownloadApprovalListResponse { items, count });
    }
    let rows = read_store(&state.follows.follows, "follow")?;
    let mut items = rows
        .values()
        .filter(|row| row.auto_download_state == AutoDownloadState::Pending)
        .map(|row| AutoDownloadApprovalItem {
            user_id: row.user_id.clone(),
            user_name: Some(row.user_name.clone()),
            artist_mbid: row.artist_mbid.clone(),
            artist_name: row.artist_name.clone(),
            requested_at: row.requested_at.unwrap_or(row.followed_at),
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        a.requested_at
            .cmp(&b.requested_at)
            .then(a.user_id.cmp(&b.user_id))
            .then(a.artist_mbid.cmp(&b.artist_mbid))
    });
    let count = items.len();
    Ok(AutoDownloadApprovalListResponse { items, count })
}

/// What produced an acquire-owned batch card.
const IMPORT_SOURCE: &str = "import";

/// List pending requests grouped per user as batch cards. Admin only. When
/// wiring connects the acquire approval store, this lists the acquire
/// batches (every card actionable through the batch mutations); otherwise
/// it groups the follow rows per user as before.
pub async fn list_approval_batches(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<ApprovalBatchListResponse, CollectionsError> {
    state.check_injection()?;
    caller.require_admin()?;
    if let Some(source) = &state.acquire_approvals {
        let mut batches = source
            .pending_batches()
            .await
            .map_err(|cause| CollectionsError::internal(&cause))?
            .into_iter()
            .map(|batch| ApprovalBatchItem {
                batch_id: batch.batch_id,
                user_id: batch.user_id,
                user_name: Some(batch.user_name),
                artist_count: batch.artists.len(),
                sample_names: batch
                    .artists
                    .iter()
                    .take(BATCH_SAMPLE_COUNT)
                    .map(|(_, name)| name.clone())
                    .collect(),
                requested_at: batch.requested_at,
                source: IMPORT_SOURCE.to_owned(),
            })
            .collect::<Vec<_>>();
        batches.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then(a.user_id.cmp(&b.user_id))
        });
        let count = batches.len();
        return Ok(ApprovalBatchListResponse { batches, count });
    }
    let rows = read_store(&state.follows.follows, "follow")?;
    let mut by_user: std::collections::HashMap<String, Vec<AutoDownloadApprovalItem>> =
        std::collections::HashMap::new();
    for row in rows
        .values()
        .filter(|row| row.auto_download_state == AutoDownloadState::Pending)
    {
        by_user
            .entry(row.user_id.clone())
            .or_default()
            .push(AutoDownloadApprovalItem {
                user_id: row.user_id.clone(),
                user_name: Some(row.user_name.clone()),
                artist_mbid: row.artist_mbid.clone(),
                artist_name: row.artist_name.clone(),
                requested_at: row.requested_at.unwrap_or(row.followed_at),
            });
    }
    let mut batches = by_user
        .into_iter()
        .map(|(user_id, mut items)| {
            items.sort_by(|a, b| {
                a.requested_at
                    .cmp(&b.requested_at)
                    .then(a.artist_mbid.cmp(&b.artist_mbid))
            });
            let user_name = items.first().and_then(|item| item.user_name.clone());
            let requested_at = items.first().map(|item| item.requested_at).unwrap_or(0);
            let sample_names = items
                .iter()
                .take(BATCH_SAMPLE_COUNT)
                .map(|item| item.artist_name.clone())
                .collect::<Vec<_>>();
            ApprovalBatchItem {
                batch_id: format!("follow:{user_id}"),
                user_id,
                user_name,
                artist_count: items.len(),
                sample_names,
                requested_at,
                source: FOLLOW_SOURCE.to_owned(),
            }
        })
        .collect::<Vec<_>>();
    batches.sort_by(|a, b| {
        a.requested_at
            .cmp(&b.requested_at)
            .then(a.user_id.cmp(&b.user_id))
    });
    let count = batches.len();
    Ok(ApprovalBatchListResponse { batches, count })
}

/// List pending auto-download requests.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approvals",
    responses(
        (status = 200, description = "Pending approvals", body = AutoDownloadApprovalListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_approvals_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<AutoDownloadApprovalListResponse>, CollectionsError> {
    list_approvals(&state, &caller).await.map(Json)
}

/// List pending requests grouped per user.
#[utoipa::path(
    get,
    path = "/api/v3/requests/auto-download-approval-batches",
    responses(
        (status = 200, description = "Approval batches", body = ApprovalBatchListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_approval_batches_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<ApprovalBatchListResponse>, CollectionsError> {
    list_approval_batches(&state, &caller).await.map(Json)
}
