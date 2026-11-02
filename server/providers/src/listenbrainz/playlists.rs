//! ListenBrainz recommendation playlists: the "Created for you" list and
//! one playlist's tracks.
//!
//! Both endpoints answer 404 for "nothing here yet" (a user with no
//! recommendations, a playlist that was replaced), which reads as an empty
//! answer, not a failure. Playlists come back as JSPF; the algorithm that
//! made each one sits in the MusicBrainz playlist extension as
//! `source_patch` (`weekly-jams`, `weekly-exploration`, ...). The wire shapes
//! are in [`models`](super::models).

use super::models::{PlaylistAnswer, PlaylistHeader, PlaylistTrack, RecommendationPlaylistsAnswer};
use super::{ListenBrainzClient, ListenBrainzCredentials};
use crate::{ProviderError, RequestPriority};

/// One recommendation playlist made for a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendationPlaylist {
    /// Playlist id (the last segment of its identifier URL).
    pub playlist_id: String,
    /// The algorithm that made it, such as `weekly-jams`.
    pub source_patch: String,
    /// The playlist's full identifier URL on ListenBrainz.
    pub identifier: String,
}

/// One playlist with its header fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistPage {
    /// Playlist title, empty when absent.
    pub title: String,
    /// Creation date as ListenBrainz sends it, empty when absent.
    pub date: String,
    /// Tracks in playlist order.
    pub tracks: Vec<RecommendationTrack>,
}

/// One track of a recommendation playlist. Title and creator are required;
/// a track missing either is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendationTrack {
    /// Track title.
    pub title: String,
    /// Credited artist.
    pub creator: String,
    /// Album title, empty when absent.
    pub album: String,
    /// Recording MBID, when the identifier carries one.
    pub recording_mbid: Option<String>,
    /// Artist MBIDs in credit order.
    pub artist_mbids: Vec<String>,
    /// Release the cover art comes from, when known.
    pub caa_release_mbid: Option<String>,
    /// Track length in milliseconds, when known.
    pub duration_ms: Option<i64>,
}

impl ListenBrainzClient {
    /// The recommendation playlists made for `username`
    /// (`GET /1/user/{user}/playlists/recommendations`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn recommendation_playlists(
        &self,
        username: &str,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<Vec<RecommendationPlaylist>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let request = self.get(
            &["1", "user", username, "playlists", "recommendations"],
            &[],
            creds,
        )?;
        let answer = self
            .read::<RecommendationPlaylistsAnswer>(request, priority)
            .await;
        let Some(answer) = absent_on_404(answer)? else {
            return Ok(Vec::new());
        };
        Ok(answer
            .playlists
            .into_iter()
            .filter_map(|entry| playlist_header(entry.playlist))
            .collect())
    }

    /// One playlist's tracks (`GET /1/playlist/{id}`). A missing playlist
    /// reads as no tracks.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn playlist_tracks(
        &self,
        playlist_id: &str,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<Vec<RecommendationTrack>, ProviderError> {
        Ok(self.playlist(playlist_id, creds, priority).await?.tracks)
    }

    /// One playlist with its title and date (`GET /1/playlist/{id}`). A
    /// missing playlist reads as an empty page.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn playlist(
        &self,
        playlist_id: &str,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<PlaylistPage, ProviderError> {
        if playlist_id.is_empty() {
            return Ok(PlaylistPage::default());
        }
        let request = self.get(&["1", "playlist", playlist_id], &[], creds)?;
        let answer = self.read::<PlaylistAnswer>(request, priority).await;
        let Some(answer) = absent_on_404(answer)? else {
            return Ok(PlaylistPage::default());
        };
        let playlist = answer.playlist;
        Ok(PlaylistPage {
            title: playlist.title.unwrap_or_default(),
            date: playlist.date.unwrap_or_default(),
            tracks: playlist
                .track
                .unwrap_or_default()
                .into_iter()
                .filter_map(recommendation_track)
                .collect(),
        })
    }
}

/// A 404 here means "nothing here yet", the same as an empty answer.
fn absent_on_404<T>(read: Result<Option<T>, ProviderError>) -> Result<Option<T>, ProviderError> {
    match read {
        Err(ProviderError::NotFound { .. }) => Ok(None),
        other => other,
    }
}

/// Id and algorithm of one playlist header. No usable id means skip.
fn playlist_header(playlist: PlaylistHeader) -> Option<RecommendationPlaylist> {
    let identifier = playlist.identifier?;
    let playlist_id = identifier.rsplit('/').next()?.trim().to_owned();
    if playlist_id.is_empty() {
        return None;
    }
    let source_patch = playlist
        .extension
        .and_then(|extension| extension.musicbrainz)
        .and_then(|extension| extension.additional_metadata)
        .and_then(|metadata| metadata.algorithm_metadata)
        .and_then(|algorithm| algorithm.source_patch)
        .unwrap_or_default();
    Some(RecommendationPlaylist {
        playlist_id,
        source_patch,
        identifier,
    })
}

/// One JSPF track.
fn recommendation_track(track: PlaylistTrack) -> Option<RecommendationTrack> {
    let recording_mbid = track
        .identifier
        .iter()
        .find(|identifier| identifier.contains("recording/"))
        .and_then(|identifier| identifier.rsplit('/').next())
        .filter(|mbid| !mbid.is_empty())
        .map(str::to_owned);
    let metadata = track
        .extension
        .and_then(|extension| extension.musicbrainz)
        .and_then(|extension| extension.additional_metadata);
    let (artists, caa_release_mbid) = metadata.map_or((None, None), |metadata| {
        (metadata.artists, metadata.caa_release_mbid)
    });
    Some(RecommendationTrack {
        title: track.title?,
        creator: track.creator?,
        album: track.album.unwrap_or_default(),
        recording_mbid,
        artist_mbids: artists
            .unwrap_or_default()
            .into_iter()
            .filter_map(|artist| artist.artist_mbid)
            .collect(),
        caa_release_mbid,
        duration_ms: track.duration.filter(|ms| *ms > 0),
    })
}
