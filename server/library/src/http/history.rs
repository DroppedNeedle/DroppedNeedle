//! Album history routes.
//!
//! - `GET /library/albums/{id}/history`: what the app did by itself to the
//!   album's tracks and to the albums it is made of, newest first. Open to
//!   every signed-in user, as the album is catalog data.
//! - `POST /library/folds/{fold_id}/undo`: undo the fold of a leftover
//!   album, by the `undo_id` its history entry carries. For curators.
//! - `POST /library/artist-repairs/{repair_id}/undo`: undo the repair that
//!   gave an album on "Unknown Artist" its tracks' artist, by its history
//!   entry's `undo_id`. For curators.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use super::blocking;
use crate::operations::history::{HistoryActor, HistoryEntry, HistoryKind};
use crate::operations::models::OperationError;
use crate::operations::service::Operations;
use crate::wiring::LibraryServices;
use droppedneedle_auth::{Curator, User};

/// Album history routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .user(routes!(get_album_history))
        .curator(routes!(undo_album_fold))
        .curator(routes!(undo_artist_repair))
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
    /// The album took its artist from its tracks instead of the "Unknown
    /// Artist" placeholder.
    ArtistFromTracks,
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
    /// For a `leftover_joined` or `artist_from_tracks` entry that can be
    /// undone now, the id to undo it by; null otherwise.
    pub undo_id: Option<String>,
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
                HistoryKind::ArtistFromTracks => AlbumHistoryKindView::ArtistFromTracks,
            },
            at: entry.at,
            by: entry.by.map(|by| match by {
                HistoryActor::You => AlbumHistoryByView::You,
                HistoryActor::Person => AlbumHistoryByView::Person,
            }),
            track_count: entry.track_count,
            track_titles: entry.track_titles,
            other_album_title: entry.other_album_title,
            undo_id: entry.undo_id,
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
    State(ops): State<Operations>,
    User(caller): User,
    Path(album_id): Path<String>,
) -> Result<Json<AlbumHistoryView>, ApiError> {
    let view = blocking(move || album_history_view(&ops, &album_id, caller.user_id())).await?;
    Ok(Json(view))
}

/// Undo the fold of a leftover album: the album, its files and its earlier
/// match come back. Plays, favorites and playlist entries moved to the
/// album that kept the files stay there.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/folds/{fold_id}/undo",
    params(("fold_id" = String, Path, description = "The history entry's undo_id")),
    responses(
        (status = 204, description = "The album is back"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "No fold that can be undone"),
        (status = 409, description = "The albums changed since the fold"),
    )
)]
pub async fn undo_album_fold(
    State(ops): State<Operations>,
    caller: Curator,
    Path(fold_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = caller.user_id().to_owned();
    blocking(move || ops.undo_fold(&fold_id, &user)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Undo the repair that gave an album left on "Unknown Artist" the artist
/// its tracks are credited to: the album's earlier artist and credits come
/// back, and the cleanup leaves it on them.
#[utoipa::path(
    post,
    tag = "library management",
    path = "/api/v3/library/artist-repairs/{repair_id}/undo",
    params(("repair_id" = String, Path, description = "The history entry's undo_id")),
    responses(
        (status = 204, description = "The album's earlier artist is back"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
        (status = 404, description = "No artist repair that can be undone"),
        (status = 409, description = "The album's artist changed since the repair"),
    )
)]
pub async fn undo_artist_repair(
    State(ops): State<Operations>,
    caller: Curator,
    Path(repair_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = caller.user_id().to_owned();
    blocking(move || ops.undo_artist_repair(&repair_id, &user)).await?;
    Ok(StatusCode::NO_CONTENT)
}
