//! Wire shapes for scrobble forwarding to ListenBrainz and Last.fm.
//!
//! ListenBrainz takes a JSON body on `POST /1/submit-listens`; the fields
//! and the `playing_now`/`single` listen types follow v2's repository and
//! the ListenBrainz API docs. Last.fm takes form parameters on
//! `POST /2.0/` and reports application errors as HTTP 200 bodies with an
//! integer `error` code, so the error body is the one shape we decode.

use serde::{Deserialize, Serialize};

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
    /// Duration, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_info: Option<AdditionalInfo>,
}

/// Extra listen fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdditionalInfo {
    /// Track length in milliseconds.
    pub duration_ms: i64,
}

/// A Last.fm application error. Success bodies carry no `error` field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LastFmError {
    /// Last.fm error code.
    pub error: i64,
    /// Human message.
    #[serde(default)]
    pub message: String,
}
