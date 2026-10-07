//! ListenBrainz recommendation playlists: the "Created for you" list and
//! one playlist's tracks.
//!
//! Ports v2's `get_recommendation_playlists` and `get_playlist_tracks`. Both
//! answer 404 for "nothing here yet" (a user with no recommendations, a
//! playlist that was replaced), which reads as an empty answer, not a
//! failure. Playlists come back as JSPF; the algorithm that made each one
//! sits in the MusicBrainz playlist extension as `source_patch`
//! (`weekly-jams`, `weekly-exploration`, ...).

use super::{
    Body, ListenBrainzClient, ListenBrainzCredentials, Outcome, RequestFailure, path_segment,
};
use crate::providers::{DegradationSink, Pacer};

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
}

impl<P: Pacer, S: DegradationSink> ListenBrainzClient<P, S> {
    /// The recommendation playlists made for `username`
    /// (`GET /1/user/{user}/playlists/recommendations`).
    pub async fn recommendation_playlists(
        &self,
        username: &str,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Vec<RecommendationPlaylist>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!(
            "/1/user/{}/playlists/recommendations",
            path_segment(username)
        );
        let payload = match self.get(&endpoint, &[], creds, false, &[404]).await {
            Ok(Body::Json(payload)) => payload,
            Ok(Body::NoContent | Body::InvalidJson) | Err(RequestFailure::Accepted(_)) => {
                return Outcome::Found(Vec::new());
            }
            Err(RequestFailure::Outcome(outcome)) => return outcome,
        };
        let entries = payload
            .get("playlists")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(
            entries
                .iter()
                .filter_map(|entry| parse_playlist_header(entry.get("playlist")?))
                .collect(),
        )
    }

    /// One playlist's tracks (`GET /1/playlist/{id}`). A missing playlist
    /// reads as no tracks.
    pub async fn playlist_tracks(
        &self,
        playlist_id: &str,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Vec<RecommendationTrack>> {
        if playlist_id.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/playlist/{}", path_segment(playlist_id));
        let payload = match self.get(&endpoint, &[], creds, false, &[404]).await {
            Ok(Body::Json(payload)) => payload,
            Ok(Body::NoContent | Body::InvalidJson) | Err(RequestFailure::Accepted(_)) => {
                return Outcome::Found(Vec::new());
            }
            Err(RequestFailure::Outcome(outcome)) => return outcome,
        };
        let tracks = payload
            .get("playlist")
            .and_then(|playlist| playlist.get("track"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(tracks.iter().filter_map(parse_track).collect())
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
    })
}

/// One JSPF track (v2 `parse_recommendation_track`).
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
    })
}
