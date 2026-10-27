//! 30-second previews from Deezer, then iTunes (v2 `/track-preview` and
//! `/album-preview`). Both services are keyless. Preview URLs expire, so
//! they are resolved for every request and never cached here.

use crate::reads::discover::{
    models::{PreviewTrackItem, TrackPreviewResponse},
    ports::{AlbumSamples, BoxFuture, ProviderFailure},
};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::preview::{PreviewClient, PreviewTrack};

/// Live previews over the one preview client. A person pressed play, so
/// every lookup goes at user priority.
pub struct LivePreviews {
    client: PreviewClient,
}

impl LivePreviews {
    /// Previews through `client`.
    pub fn new(client: PreviewClient) -> Self {
        Self { client }
    }
}

fn item(track: PreviewTrack) -> PreviewTrackItem {
    PreviewTrackItem {
        title: track.title,
        artist_name: track.artist_name,
        preview_url: track.preview_url,
        duration_s: track.duration_s.map(i64::from),
        position: track.position,
    }
}

impl LivePreviews {
    /// Preview behind one track. `None` fields mean no provider had one.
    pub fn track_preview<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<TrackPreviewResponse, ProviderFailure>> {
        Box::pin(async move {
            let (found, provider) = self
                .client
                .get_track_preview(artist, track, RequestPriority::UserInitiated)
                .await;
            Ok(match found {
                Some(found) => TrackPreviewResponse {
                    preview_url: Some(found.preview_url),
                    title: Some(found.title),
                    duration_s: found.duration_s.map(i64::from),
                    provider: provider.map(str::to_owned),
                },
                None => TrackPreviewResponse {
                    preview_url: None,
                    title: None,
                    duration_s: None,
                    provider: None,
                },
            })
        })
    }

    /// Ordered album samples plus the serving provider.
    pub fn album_preview<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        count: i64,
    ) -> BoxFuture<'a, Result<AlbumSamples, ProviderFailure>> {
        Box::pin(async move {
            let limit = usize::try_from(count).unwrap_or(0);
            let (tracks, provider) = self
                .client
                .get_album_preview_tracks(artist, album, limit, RequestPriority::UserInitiated)
                .await;
            Ok((
                tracks.into_iter().map(item).collect(),
                provider.map(str::to_owned),
            ))
        })
    }
}
