//! Wire shapes for scrobble forwarding to ListenBrainz. Last.fm takes form
//! parameters, and its answers are read by its client.
//!
//! ListenBrainz takes a JSON body on `POST /1/submit-listens`; the fields
//! and the `playing_now`/`single` listen types follow v2's repository and
//! the submission format at
//! https://listenbrainz.readthedocs.io/en/latest/users/json.html, read
//! 2026-10-21: `listened_at` is the play's start in unix seconds and is
//! left out for `playing_now`; `additional_info` takes `duration_ms` and
//! `recording_mbid` ("A MusicBrainz Recording ID of the recording that was
//! played"), among others. The server refuses an id that is not a UUID
//! with a 400 ("recording_mbid MBID format invalid."), so only a UUID is
//! sent.

use serde::Serialize;

/// `POST /1/submit-listens` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubmitListens {
    /// `playing_now` or `single`.
    pub listen_type: &'static str,
    /// Exactly one listen.
    pub payload: Vec<Listen>,
}

/// One listen.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listen {
    /// Play time, unix seconds. Absent for `playing_now`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listened_at: Option<i64>,
    /// What was played.
    pub track_metadata: TrackMetadata,
}

/// Track names plus optional extras.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackMetadata {
    /// Artist name.
    pub artist_name: String,
    /// Track title.
    pub track_name: String,
    /// Album title, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
    /// Length and recording, when either is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<AdditionalInfo>,
}

/// Extra listen fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdditionalInfo {
    /// Track length in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// The MusicBrainz recording played.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
}

impl AdditionalInfo {
    /// The extras worth sending: a positive length and a recording id that
    /// is a UUID. `None` when neither is.
    pub fn of(duration_ms: i64, recording_mbid: Option<&str>) -> Option<Self> {
        let info = Self {
            duration_ms: (duration_ms > 0).then_some(duration_ms),
            recording_mbid: recording_mbid
                .and_then(|mbid| uuid::Uuid::parse_str(mbid.trim()).ok())
                .map(|mbid| mbid.hyphenated().to_string()),
        };
        (info.duration_ms.is_some() || info.recording_mbid.is_some()).then_some(info)
    }
}
