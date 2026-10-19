//! Album history route.
//!
//! - `GET /library/albums/{id}/history`: what the app did by itself to the
//!   album's tracks and to the albums it is made of, newest first. Open to
//!   every signed-in user, as the album is catalog data.

use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use super::operations::blocking;
use crate::operations::history::{HistoryActor, HistoryEntry, HistoryKind};
use crate::operations::models::OperationError;
use crate::operations::service::Operations;
use crate::wiring::LibrarySetup;
use droppedneedle_auth::User;

/// Album history routes.
pub fn routes() -> Routes<LibrarySetup> {
    Routes::new().user(routes!(get_album_history))
}

/// What one history entry tells.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlbumHistoryKindView {
    /// Tracks have new files, and what people made moved to them.
    TracksContinued,
    /// Tracks were replaced by the edition's version, and what people
    /// made moved to the new files.
    TracksReplaced,
    /// An album retagged outside DroppedNeedle was joined into this one.
    RetagJoined,
    /// Left unmanaged like the album whose files were retagged as this one.
    RetagHalfLeftAlone,
    /// A leftover album of an earlier version was folded into this one.
    LeftoverJoined,
    /// Other music took the paths of tracks, which came in as new tracks;
    /// what people made stayed with the old tracks, now missing.
    TracksOtherMusic,
    /// Tracks' files were replaced by new copies of the same recordings.
    TracksNewCopy,
}

/// Who asked for a join.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlbumHistoryByView {
    /// The caller.
    You,
    /// Another person.
    Person,
}

/// One line of an album's history.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AlbumHistoryEntryView {
    pub kind: AlbumHistoryKindView,
    /// Unix milliseconds.
    pub at: i64,
    /// Who asked, for `retag_joined` only; null when the app did it.
    pub by: Option<AlbumHistoryByView>,
    /// How many tracks the entry is about; 0 for album entries.
    pub track_count: usize,
    /// The first five of those tracks' titles, in album order.
    pub track_titles: Vec<String>,
    /// The other album's title, null when its row is gone.
    pub other_album_title: Option<String>,
}

/// An album's history, newest first: at most ten entries, none older than
/// 180 days.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AlbumHistoryView {
    pub items: Vec<AlbumHistoryEntryView>,
}

impl From<HistoryEntry> for AlbumHistoryEntryView {
    fn from(entry: HistoryEntry) -> Self {
        Self {
            kind: match entry.kind {
                HistoryKind::TracksContinued => AlbumHistoryKindView::TracksContinued,
                HistoryKind::TracksReplaced => AlbumHistoryKindView::TracksReplaced,
                HistoryKind::RetagJoined => AlbumHistoryKindView::RetagJoined,
                HistoryKind::RetagHalfLeftAlone => AlbumHistoryKindView::RetagHalfLeftAlone,
                HistoryKind::LeftoverJoined => AlbumHistoryKindView::LeftoverJoined,
                HistoryKind::TracksOtherMusic => AlbumHistoryKindView::TracksOtherMusic,
                HistoryKind::TracksNewCopy => AlbumHistoryKindView::TracksNewCopy,
            },
            at: entry.at,
            by: entry.by.map(|by| match by {
                HistoryActor::You => AlbumHistoryByView::You,
                HistoryActor::Person => AlbumHistoryByView::Person,
            }),
            track_count: entry.track_count,
            track_titles: entry.track_titles,
            other_album_title: entry.other_album_title,
        }
    }
}

/// The album's history as `caller_user_id` sees it. Blocking.
pub fn album_history_view(
    ops: &Operations,
    album_id: &str,
    caller_user_id: &str,
) -> Result<AlbumHistoryView, OperationError> {
    Ok(AlbumHistoryView {
        items: ops
            .album_history(album_id, caller_user_id)?
            .into_iter()
            .map(AlbumHistoryEntryView::from)
            .collect(),
    })
}

/// What the app did by itself to the album's tracks and to the albums it
/// is made of.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/albums/{album_id}/history",
    params(("album_id" = String, Path, description = "Local album id")),
    responses(
        (status = 200, description = "The album's history", body = AlbumHistoryView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown album"),
    )
)]
pub async fn get_album_history(
    State(state): State<LibrarySetup>,
    User(caller): User,
    Path(album_id): Path<String>,
) -> Result<Json<AlbumHistoryView>, ApiError> {
    let ops = Operations::new(&state);
    let view = blocking(move || album_history_view(&ops, &album_id, caller.user_id())).await?;
    Ok(Json(view))
}
