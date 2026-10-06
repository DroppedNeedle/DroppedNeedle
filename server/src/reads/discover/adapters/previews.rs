//! 30-second previews from Deezer, then iTunes (v2 `/track-preview` and
//! `/album-preview`). Both services are keyless. Preview URLs expire, so
//! they are resolved for every request and never cached here.

use crate::providers::adapters::ReqwestGet;
use crate::providers::preview::{PreviewClient, PreviewTrack};
use crate::reads::discover::{
    models::{PreviewTrackItem, TrackPreviewResponse},
    ports::{AlbumSamples, BoxFuture, PreviewSource, ProviderFailure},
};

/// Live previews over the shared HTTP client.
pub struct LivePreviews {
    http: ReqwestGet,
}

impl LivePreviews {
    /// Previews over one GET transport.
    pub fn new(http: ReqwestGet) -> Self {
        Self { http }
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

impl PreviewSource for LivePreviews {
    fn track_preview<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<TrackPreviewResponse, ProviderFailure>> {
        Box::pin(async move {
            let client = PreviewClient::new(&self.http);
            let (found, provider) = client.get_track_preview(artist, track).await;
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

    fn album_preview<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        count: i64,
    ) -> BoxFuture<'a, Result<AlbumSamples, ProviderFailure>> {
        Box::pin(async move {
            let client = PreviewClient::new(&self.http);
            let limit = usize::try_from(count).unwrap_or(0);
            let (tracks, provider) = client.get_album_preview_tracks(artist, album, limit).await;
            Ok((
                tracks.into_iter().map(item).collect(),
                provider.map(str::to_owned),
            ))
        })
    }
}
