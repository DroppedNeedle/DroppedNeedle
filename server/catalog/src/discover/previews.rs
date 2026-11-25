//! 30-second previews from Deezer, then iTunes, for tracks and albums. Both
//! services are keyless. Preview URLs expire, so
//! they are resolved for every request and never cached here.

use std::collections::HashSet;

use crate::discover::{
    models::{
        AlbumPreviewMode, PreviewRole, PreviewSource, PreviewTrackItem, TrackPreviewResponse,
    },
    ports::{AlbumSamples, BoxFuture, ProviderFailure},
};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::preview::{PreviewClient, PreviewProvider, PreviewTrack};

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

/// Most tracks an audition reads from an album's track list: the whole list
/// for any ordinary album, in the one tracks call the lookup makes.
const AUDITION_ROWS: usize = 100;

fn item(track: PreviewTrack, source: PreviewSource, role: Option<PreviewRole>) -> PreviewTrackItem {
    PreviewTrackItem {
        title: track.title,
        artist_name: track.artist_name,
        preview_url: track.preview_url,
        duration_s: track.duration_s.map(i64::from),
        position: track.position,
        source,
        role,
    }
}

fn source_of(provider: PreviewProvider) -> PreviewSource {
    match provider {
        PreviewProvider::Deezer => PreviewSource::Deezer,
        PreviewProvider::Itunes => PreviewSource::Itunes,
    }
}

/// Up to three clips that show an album, from its tracks with a preview in
/// album order (disc, then position on it; no disc reads as disc 1): the
/// opener, the best-known other track (the highest rank, else the second
/// track), then the track nearest two thirds of the way through that is
/// neither (the earlier one on a tie). A place on a disc already seen is
/// another edition's copy of a song (iTunes' search mixes in the deluxe
/// one), so only its first track counts.
fn audition(mut tracks: Vec<PreviewTrack>) -> Vec<(PreviewTrack, PreviewRole)> {
    let place = |track: &PreviewTrack| (track.disc.unwrap_or(1), track.position);
    tracks.sort_by_key(|track| {
        let (disc, position) = place(track);
        (disc, position.unwrap_or(i64::MAX))
    });
    let mut places = HashSet::new();
    tracks.retain(|track| {
        let (disc, position) = place(track);
        position.is_none_or(|position| places.insert((disc, position)))
    });
    let count = tracks.len();
    if count == 0 {
        return Vec::new();
    }
    let ranked = tracks[1..].iter().any(|track| track.rank.is_some());
    let best_known = (1..count).max_by_key(|&index| {
        (
            if ranked {
                tracks[index].rank.unwrap_or(i64::MIN)
            } else {
                0
            },
            std::cmp::Reverse(index),
        )
    });
    let target = ((count - 1) * 2 + 1) / 3;
    let deep_cut = (1..count)
        .filter(|&index| Some(index) != best_known)
        .min_by_key(|&index| (index.abs_diff(target), index));
    let mut picked = vec![(0, PreviewRole::Opener)];
    picked.extend(best_known.map(|index| (index, PreviewRole::BestKnown)));
    picked.extend(deep_cut.map(|index| (index, PreviewRole::DeepCut)));
    let mut slots: Vec<Option<PreviewTrack>> = tracks.into_iter().map(Some).collect();
    picked
        .into_iter()
        .filter_map(|(index, role)| slots[index].take().map(|track| (track, role)))
        .collect()
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

    /// Album samples plus the serving provider: the first `count` tracks in
    /// album order, or the audition's three clips from the whole track list.
    pub fn album_preview<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        count: i64,
        mode: AlbumPreviewMode,
    ) -> BoxFuture<'a, Result<AlbumSamples, ProviderFailure>> {
        Box::pin(async move {
            let limit = match mode {
                AlbumPreviewMode::First => usize::try_from(count).unwrap_or(0),
                AlbumPreviewMode::Audition => AUDITION_ROWS,
            };
            let (tracks, provider) = self
                .client
                .get_album_preview_tracks(artist, album, limit, RequestPriority::UserInitiated)
                .await;
            let Some(provider) = provider else {
                return Ok((Vec::new(), None));
            };
            let source = source_of(provider);
            let items = match mode {
                AlbumPreviewMode::First => tracks
                    .into_iter()
                    .map(|track| item(track, source, None))
                    .collect(),
                AlbumPreviewMode::Audition => audition(tracks)
                    .into_iter()
                    .map(|(track, role)| item(track, source, Some(role)))
                    .collect(),
            };
            Ok((items, Some(source)))
        })
    }
}
