//! ListenBrainz recommendation playlists: the "Created for you" list and
//! one playlist's tracks.
//!
//! Both endpoints answer 404 for "nothing here yet" (a user with no
//! recommendations, a playlist that was replaced), which reads as an empty
//! answer, not a failure. Playlists come back as JSPF; the algorithm that
//! made each one sits in the MusicBrainz playlist extension as
//! `source_patch` (`weekly-jams`, `weekly-exploration`, ...).

use super::{ListenBrainzClient, ListenBrainzCredentials};
use crate::{ProviderError, RequestPriority};

/// JSPF extension key for playlist-level MusicBrainz metadata.
const PLAYLIST_EXTENSION: &str = "https://musicbrainz.org/doc/jspf#playlist";
/// JSPF extension key for track-level MusicBrainz metadata.
const TRACK_EXTENSION: &str = "https://musicbrainz.org/doc/jspf#track";

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
        let Some(payload) = absent_on_404(self.read(request, priority).await)? else {
            return Ok(Vec::new());
        };
        let entries = payload
            .get("playlists")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(entries
            .iter()
            .filter_map(|entry| parse_playlist_header(entry.get("playlist")?))
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
        let Some(payload) = absent_on_404(self.read(request, priority).await)? else {
            return Ok(PlaylistPage::default());
        };
        let playlist = payload.get("playlist");
        let field = |key: &str| {
            playlist
                .and_then(|playlist| playlist.get(key))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let tracks = playlist
            .and_then(|playlist| playlist.get("track"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(PlaylistPage {
            title: field("title"),
            date: field("date"),
            tracks: tracks.iter().filter_map(parse_track).collect(),
        })
    }
}

/// A 404 here means "nothing here yet", the same as an empty answer.
fn absent_on_404(
    read: Result<Option<serde_json::Value>, ProviderError>,
) -> Result<Option<serde_json::Value>, ProviderError> {
    match read {
        Err(ProviderError::NotFound { .. }) => Ok(None),
        other => other,
    }
}

/// Id and algorithm of one playlist header. No usable id means skip.
fn parse_playlist_header(playlist: &serde_json::Value) -> Option<RecommendationPlaylist> {
    let identifier = playlist.get("identifier")?.as_str()?;
    let playlist_id = identifier.rsplit('/').next()?.trim();
    if playlist_id.is_empty() {
        return None;
    }
    let source_patch = playlist
        .get("extension")
        .and_then(|ext| ext.get(PLAYLIST_EXTENSION))
        .and_then(|ext| ext.get("additional_metadata"))
        .and_then(|meta| meta.get("algorithm_metadata"))
        .and_then(|algo| algo.get("source_patch"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(RecommendationPlaylist {
        playlist_id: playlist_id.to_owned(),
        source_patch,
        identifier: identifier.to_owned(),
    })
}

/// One JSPF track.
fn parse_track(track: &serde_json::Value) -> Option<RecommendationTrack> {
    let text = |key: &str| {
        track
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let title = text("title")?;
    let creator = text("creator")?;
    let recording_mbid = track
        .get("identifier")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .find(|ident| ident.contains("recording/"))
        .and_then(|ident| ident.rsplit('/').next())
        .filter(|mbid| !mbid.is_empty())
        .map(str::to_owned);
    let meta = track
        .get("extension")
        .and_then(|ext| ext.get(TRACK_EXTENSION))
        .and_then(|ext| ext.get("additional_metadata"));
    let artist_mbids = meta
        .and_then(|meta| meta.get("artists"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|artist| artist.get("artist_mbid")?.as_str())
        .filter(|mbid| !mbid.is_empty())
        .map(str::to_owned)
        .collect();
    let caa_release_mbid = meta
        .and_then(|meta| meta.get("caa_release_mbid"))
        .and_then(serde_json::Value::as_str)
        .filter(|mbid| !mbid.is_empty())
        .map(str::to_owned);
    Some(RecommendationTrack {
        title,
        creator,
        album: text("album").unwrap_or_default(),
        recording_mbid,
        artist_mbids,
        caa_release_mbid,
        duration_ms: track
            .get("duration")
            .and_then(serde_json::Value::as_i64)
            .filter(|ms| *ms > 0),
    })
}
