//! Request the songs a playlist names and the library lacks.
//!
//! The playlist's owner presses one button; each album with an entry that
//! has no library file is resolved to a MusicBrainz release group by its
//! title and artist, then filed as a request. A playlist can request,
//! never retire, replace or pin.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use droppedneedle_auth::User;
use droppedneedle_catalog::collections::store::playlists::PlaylistStore;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use crate::imports::spotify_albums::AlbumResolver;
use crate::requests::error::RequestsError;
use crate::requests::ledger::ApprovalState;
use crate::requests::models::{AlbumIntake, RequestKind};
use crate::requests::service::RequestsService;
use crate::requests::state::RequestsState;

/// Most albums one press files. The Weekly Mix caps the same way, so one
/// press on a large import never turns into a long MusicBrainz walk.
const MAX_REQUESTS_PER_PRESS: usize = 10;

/// Every dependency the route needs.
#[derive(Clone)]
pub struct PlaylistRequestDeps {
    /// Request intake, with the library handle and the quota ledger.
    pub requests: RequestsState,
    /// Playlists, read as the caller.
    pub playlists: PlaylistStore,
    /// Finds each album's release group.
    pub resolver: Arc<AlbumResolver>,
}

/// The route.
pub fn routes() -> Routes<PlaylistRequestDeps> {
    Routes::new().user(routes!(request_missing_handler))
}

/// What one press filed.
#[derive(Debug, Serialize, ToSchema)]
pub struct RequestMissingResponse {
    /// Albums a request was filed for.
    pub requested: usize,
    /// Albums passed over: the entry names nothing, the album already has a
    /// live request, or the press reached its cap.
    pub skipped: usize,
    /// Albums the resolver or the intake could not place.
    pub failed: usize,
}

/// Request the albums a playlist names that the library lacks.
/// `POST /api/v3/playlists/{playlist_id}/request-missing`. Owner only.
#[utoipa::path(
    post,
    tag = "playlists",
    path = "/api/v3/playlists/{playlist_id}/request-missing",
    params(("playlist_id" = String, Path, description = "Playlist id")),
    responses((status = 202, body = RequestMissingResponse))
)]
pub async fn request_missing_handler(
    State(deps): State<PlaylistRequestDeps>,
    User(principal): User,
    Path(playlist_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let owned = deps.playlists.owned_by(principal.user_id());
    let found = owned.get(&playlist_id).await.map_err(ApiError::internal)?;
    if found.is_none() {
        return Err(ApiError::not_found());
    }
    let tracks = owned
        .tracks(&playlist_id)
        .await
        .map_err(ApiError::internal)?;
    // One album, one request: group the entries without a library file.
    let mut groups: Vec<(String, String)> = Vec::new();
    let mut skipped = 0_usize;
    for track in &tracks {
        if track.library_file_id.is_some() {
            continue;
        }
        let album = track.album_name.trim().to_owned();
        let artist = track.artist_name.trim().to_owned();
        let title = track.track_name.trim().to_owned();
        if album.is_empty() || artist.is_empty() || title.is_empty() {
            // A row-only file entry has no album to ask for.
            skipped += 1;
            continue;
        }
        if !groups.contains(&(album.clone(), artist.clone())) {
            groups.push((album, artist));
        }
    }
    // The Weekly Mix grant decides whether the press waits for an admin:
    // a curator or an approved row files at once, everyone else through the
    // normal approval queue.
    let granted = principal.role().is_curator()
        || matches!(
            deps.requests.mixes.state(principal.user_id()).await,
            Ok(Some(ApprovalState::Approved))
        );
    let service = RequestsService::new(&deps.requests);
    let mut requested = 0_usize;
    let mut failed = 0_usize;
    for (album, artist) in groups {
        if requested >= MAX_REQUESTS_PER_PRESS {
            skipped += 1;
            continue;
        }
        let group = match deps.resolver.resolve_names(&album, &artist).await {
            Ok(Some(group)) => group,
            Ok(None) => {
                tracing::info!(album = %album, artist = %artist, "playlist request: no release group proved");
                failed += 1;
                continue;
            }
            Err(error) => {
                tracing::warn!(album = %album, artist = %artist, ?error, "playlist request: musicbrainz read failed");
                failed += 1;
                continue;
            }
        };
        if matches!(
            deps.requests.store.get(RequestKind::Album, &group).await,
            Ok(Some(row)) if row.status.is_active()
        ) {
            skipped += 1;
            continue;
        }
        let intake = AlbumIntake {
            musicbrainz_id: group,
            artist: Some(artist),
            album: Some(album),
            year: None,
            artist_mbid: None,
            release_mbid: None,
            monitor_artist: false,
            auto_download_artist: false,
        };
        let outcome = if granted {
            service.request_album_granted(&principal, &intake).await
        } else {
            service.request_album(&principal, &intake).await
        };
        match outcome {
            Ok(response) if response.task_id.is_some() => requested += 1,
            Ok(_) => skipped += 1,
            Err(error) => {
                tracing::warn!(?error, "playlist request: album request failed");
                if matches!(
                    error,
                    RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. }
                ) {
                    break;
                }
                failed += 1;
            }
        }
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(RequestMissingResponse {
            requested,
            skipped,
            failed,
        }),
    ))
}
