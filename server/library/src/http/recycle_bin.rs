//! The recycle bin page's routes, for admins: what the bin holds, grouped
//! by the removal or change that put it there; what emptying it now would
//! delete; whether it is being emptied; and Empty itself, which queues a
//! run of the bin's expiry with no retention. Put back is the album's
//! removal route.

use crate::manage::Publishing;
use axum::{Json, extract::State, http::StatusCode};
use droppedneedle_auth::Admin;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use crate::publish::PublishError;
use crate::publish::bin_contents::{self, BinGroup, BinKind};
use crate::recycle_bin::{self, Emptied, EmptyingState};
use crate::wiring::LibraryServices;

/// Recycle bin routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .admin(routes!(list_recycle_bin))
        .admin(routes!(recycle_bin_summary))
        .admin(routes!(recycle_bin_status))
        .admin(routes!(empty_recycle_bin))
}

/// The album a group of files came from.
#[derive(Debug, Serialize, ToSchema)]
pub struct BinAlbumView {
    /// The local album id; the album page opens on it.
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    /// The album has a page to open: it has files, or a MusicBrainz
    /// release group.
    pub has_page: bool,
    pub release_group_mbid: Option<String>,
    /// The album has art of its own, served by its artwork route.
    pub cover_available: bool,
}

/// One removal or change whose files are in the recycle bin.
#[derive(Debug, Serialize, ToSchema)]
pub struct BinRemovalView {
    /// The removal's id, which Put back names.
    pub id: String,
    pub kind: BinKind,
    pub album: Option<BinAlbumView>,
    /// When it was made, unix milliseconds.
    pub removed_at: i64,
    /// When the bin's expiry deletes its first copy, unix milliseconds;
    /// absent while the retention can't be read.
    pub expires_at: Option<i64>,
    /// Files in the bin.
    pub files: u32,
    /// Bytes of the copies found.
    pub bytes: u64,
    /// Copies not found where they should be.
    pub not_found: u32,
    /// Copies that can't be checked: in a bin no longer set, behind a
    /// link, or while the bins can't be told.
    pub unchecked: u32,
    /// Files waiting for an admin under "Some file writes need you".
    pub held: u32,
    /// Put back is offered.
    pub can_put_back: bool,
}

/// One page of the recycle bin, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecycleBinPage {
    pub items: Vec<BinRemovalView>,
    /// Pass back as `cursor` for the next page; absent on the last.
    pub next_cursor: Option<String>,
    /// Days a copy stays in the bin; absent while it can't be read.
    pub retention_days: Option<i64>,
    /// The bins can be looked at now.
    pub bins_known: bool,
}

/// Paging for the recycle bin.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RecycleBinQuery {
    /// The previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Items per page, 1 to 100 (50 by default).
    pub limit: Option<u32>,
}

/// What emptying the bin now would delete.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecycleBinSummary {
    /// Files it would delete (one changed in the bin is kept instead).
    pub files: u32,
    /// Their bytes.
    pub bytes: u64,
    /// Files it would leave: held, in use by a put-back, not found, or in
    /// a bin it may not empty.
    pub staying: u32,
    /// Older bin folders the app did not record, which it would empty.
    pub older_folders: u32,
    /// When this was read, unix milliseconds. Empty deletes only what was
    /// in the bin then.
    pub as_of: i64,
}

/// Whether the bin is being emptied, and what the last run did.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecycleBinStatus {
    pub state: EmptyingState,
    pub last_emptied: Option<Emptied>,
}

/// Empty the bin of what it held at `before`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct EmptyRecycleBinBody {
    /// The summary's `as_of`: only copies in the bin by then are deleted.
    pub before: i64,
}

/// Empty was queued.
#[derive(Debug, Serialize, ToSchema)]
pub struct EmptyRecycleBinView {
    /// A run was already waiting; it goes ahead with its own moment.
    pub joined: bool,
}

/// List what the recycle bin holds, grouped by the removal or change
/// that put it there, newest first.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/recycle-bin",
    params(RecycleBinQuery),
    responses(
        (status = 200, description = "A page of the recycle bin", body = RecycleBinPage),
        (status = 400, description = "Bad cursor or limit"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn list_recycle_bin(
    State(state): State<Publishing>,
    _caller: Admin,
    ValidQuery(query): ValidQuery<RecycleBinQuery>,
) -> Result<Json<RecycleBinPage>, ApiError> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::invalid_input("Limit must be 1 to 100"));
    }
    let cursor = match query.cursor.as_deref().filter(|raw| !raw.is_empty()) {
        None => None,
        Some(raw) => Some(
            raw.split_once('|')
                .and_then(|(at, id)| Some((at.parse::<i64>().ok()?, id.to_owned())))
                .ok_or_else(|| {
                    ApiError::invalid_input("That cursor is not valid; start from the first page.")
                })?,
        ),
    };
    let retention_days = state
        .settings
        .download_policy
        .read()
        .map(|policy| policy.recycle_retention_days.max(0));
    let (page, bins_known) = blocking(move || {
        let sandbox = state.expiry_sandbox();
        bin_contents::page(&state.db, sandbox.as_ref(), cursor, limit as usize)
            .map(|page| (page, sandbox.is_some()))
            .map_err(publish_error)
    })
    .await?;
    let next_cursor = page.next.map(|(at, id)| format!("{at}|{id}"));
    Ok(Json(RecycleBinPage {
        items: page
            .items
            .into_iter()
            .map(|group| view(group, retention_days))
            .collect(),
        next_cursor,
        retention_days,
        bins_known,
    }))
}

fn view(group: BinGroup, retention_days: Option<i64>) -> BinRemovalView {
    BinRemovalView {
        expires_at: retention_days
            .zip(group.first_binned_at)
            .map(|(days, first)| first.saturating_add(days.saturating_mul(86_400_000))),
        id: group.id,
        kind: group.kind,
        album: group.album.map(|album| BinAlbumView {
            id: album.id,
            title: album.title,
            artist: album.artist,
            has_page: album.has_page,
            release_group_mbid: album.release_group_mbid,
            cover_available: album.cover_available,
        }),
        removed_at: group.removed_at,
        files: group.files,
        bytes: group.bytes,
        not_found: group.not_found,
        unchecked: group.unchecked,
        held: group.held,
        can_put_back: group.can_put_back,
    }
}

/// Count what emptying the recycle bin now would delete, for its
/// confirm.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/recycle-bin/summary",
    responses(
        (status = 200, description = "What Empty would delete", body = RecycleBinSummary),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn recycle_bin_summary(
    State(state): State<Publishing>,
    _caller: Admin,
) -> Result<Json<RecycleBinSummary>, ApiError> {
    let as_of = state.core.clock.now_ms();
    let summary = blocking(move || {
        let sandbox = state.expiry_sandbox();
        bin_contents::summary(&state.db, sandbox.as_ref(), as_of).map_err(publish_error)
    })
    .await?;
    Ok(Json(RecycleBinSummary {
        files: summary.files,
        bytes: summary.bytes,
        staying: summary.staying,
        older_folders: summary.older_folders,
        as_of,
    }))
}

/// Whether the recycle bin is being emptied, and what the last run did.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/recycle-bin/status",
    responses(
        (status = 200, description = "Emptying state", body = RecycleBinStatus),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn recycle_bin_status(
    State(state): State<Publishing>,
    _caller: Admin,
) -> Result<Json<RecycleBinStatus>, ApiError> {
    let emptying = recycle_bin::state(&state).await.map_err(db_error)?;
    let last_emptied =
        blocking(move || recycle_bin::last_emptied(&state).map_err(db_error)).await?;
    Ok(Json(RecycleBinStatus {
        state: emptying,
        last_emptied,
    }))
}

/// Empty the recycle bin of what it held at `before`, under the rules
/// its expiry keeps: a copy that changed in the bin, one a put-back is
/// using, and one in a bin that is or holds a library or download folder
/// stay. Runs in the background.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/recycle-bin/empty",
    request_body = EmptyRecycleBinBody,
    responses(
        (status = 202, description = "Emptying started", body = EmptyRecycleBinView),
        (status = 400, description = "Bad body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 503, description = "The server is busy; try again"),
    )
)]
pub async fn empty_recycle_bin(
    State(state): State<Publishing>,
    caller: Admin,
    ValidJson(body): ValidJson<EmptyRecycleBinBody>,
) -> Result<(StatusCode, Json<EmptyRecycleBinView>), ApiError> {
    let actor = caller.user_id().to_owned();
    let enqueued = blocking(move || {
        recycle_bin::request_empty(&state, body.before, &actor).map_err(|error| {
            if error.is_busy() {
                ApiError::unavailable(Unavailable::ServerBusy, error)
            } else {
                ApiError::internal(error)
            }
        })
    })
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(EmptyRecycleBinView {
            joined: enqueued.joined,
        }),
    ))
}

fn db_error(error: droppedneedle_kernel::db::DbError) -> ApiError {
    use droppedneedle_kernel::db::DbError;
    match error {
        DbError::Busy { .. } | DbError::LaneClosed => {
            ApiError::unavailable(Unavailable::ServerBusy, error)
        }
        other => ApiError::internal(other),
    }
}

fn publish_error(error: PublishError) -> ApiError {
    if error.wrote_nothing() {
        ApiError::unavailable(Unavailable::ServerBusy, error)
    } else {
        ApiError::internal(error)
    }
}
