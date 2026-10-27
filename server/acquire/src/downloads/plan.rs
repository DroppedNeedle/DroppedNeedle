//! A download's plan: what it fetches beyond the task row's own columns.
//!
//! The plan is `download_tasks.plan_json`, written in the transaction
//! that creates the task (dispatch, a retry's successor) and in the one
//! that records the track's album or the release MusicBrainz redirected
//! to. Decoding is strict: an unknown field is an error, so a task whose
//! plan does not read waits instead of running as an empty plan. Every
//! field has a default, so `{}` is the empty plan of a plain download.

use serde::{Deserialize, Serialize};

/// The plan of one download task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPlan {
    /// The release the lists below belong to (after a MusicBrainz
    /// redirect, the id it redirected to). The lists are dropped when the
    /// row's asked release no longer answers to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<String>,
    /// The release's tracklist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_tracks: Vec<ExpectedTrack>,
    /// For an album download: the only positions to fetch ("Get missing
    /// tracks"). Empty fetches the whole album.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub album_wanted: Vec<TrackPosition>,
    /// For an album download: the library copy it was fetched for, which
    /// the files join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_album: Option<PlannedAlbum>,
    /// For a single-track download: the person chose to take the song from
    /// its album, so the album's track imports as the requested song when
    /// it is the right track.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub album_version: bool,
    /// For a single-track download: the album it was resolved to. The
    /// album's release, title and year are on the task row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_album: Option<TrackAlbum>,
    /// A person picked a candidate outside the quality band and confirmed
    /// it: the landing keeps its files whatever their quality, for the
    /// attempt that fetched that candidate only (its handle is `pinned`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quality_waived: bool,
}

impl TaskPlan {
    /// Decode a stored plan, refusing unknown fields.
    pub fn decode(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The stored form.
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// What a retry carries over: the release, the positions it asks for,
    /// the copy it is for and "Get it from the album". The tracklist and a
    /// track's album are worked out again. A quality waiver is never
    /// carried over: it belongs to the pick a person confirmed.
    pub fn for_successor(&self) -> Self {
        Self {
            release_mbid: self.release_mbid.clone(),
            album_wanted: self.album_wanted.clone(),
            planned_album: self.planned_album.clone(),
            album_version: self.album_version,
            ..Self::default()
        }
    }

    /// Whether the plan's lists belong to `release` (the plan's own
    /// release, compared without case).
    pub fn is_for(&self, release: &str) -> bool {
        self.release_mbid
            .as_deref()
            .is_some_and(|held| held.trim().eq_ignore_ascii_case(release.trim()))
    }
}

/// One track from the task's exact edition, keyed by disc/position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedTrack {
    /// Position within the disc (1-based).
    pub track_number: i64,
    /// Disc number (1-based).
    #[serde(default = "default_disc")]
    pub disc_number: i64,
    /// Canonical length in seconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// Recording MBID for corroborating evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
    /// Track title for corroborating evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Release-track MBID; makes attribution edition-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_track_mbid: Option<String>,
}

fn default_disc() -> i64 {
    1
}

/// One (disc, track) position on a release, both 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackPosition {
    /// Disc number.
    pub disc: u32,
    /// Position within the disc.
    pub track: u32,
}

/// The album a single-track download was resolved to. The track is
/// searched for as part of this album, and the landing checks it against
/// the plan's tracklist at `wanted` positions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackAlbum {
    /// Album artist, as searched.
    pub album_artist: String,
    /// Why this edition: `manual_identity`, `library_edition`,
    /// `requested_release`, or `best_official`.
    pub basis: String,
    /// Positions of the requested recording on the release.
    pub wanted: Vec<TrackPosition>,
}

/// The library copy an album download was fetched for, and its release as
/// the copy's identity row named it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedAlbum {
    pub album_id: String,
    pub release_mbid: String,
    /// The copy's files that are another version of a release track, to be
    /// replaced by this edition's version ("Replace tracks you have in
    /// another version"). Kept and dropped with the planned album.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<ReplaceVersion>,
}

/// One file of the copy and the release track whose version replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceVersion {
    pub release_track_mbid: String,
    pub local_track_id: String,
}
