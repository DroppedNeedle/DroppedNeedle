//! ListenBrainz wire shapes for the reads the core client makes.
//!
//! Tolerant of unknown fields (serde skips them) and strict on identity: an
//! answer missing the field a caller keys on fails decoding, so a changed
//! answer reads as a payload error, never as "no listens" or "invalid".
//! Counts accept an integer or an integral float. A field read with `loose_*`
//! and no `default` must be there but may be null: that row alone is skipped.

use std::collections::HashMap;

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};

/// `GET /1/user/{user}/listens`.
///
/// Verified against api.listenbrainz.org/1/user/{user}/listens on 2026-11-02: tests/fixtures/listenbrainz/listens_unknown_user.json (the 404 only).
/// Not verified: the 200 answer needs an account, recorded later.
#[derive(Debug, Clone, Deserialize)]
pub struct ListensAnswer {
    /// The envelope.
    pub payload: ListensPayload,
}

/// The listens envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ListensPayload {
    /// The listens, newest first. Required: an answer without the list is
    /// not "no listens".
    pub listens: Vec<ListenItem>,
}

/// One listen.
#[derive(Debug, Clone, Deserialize)]
pub struct ListenItem {
    /// Listen time, unix seconds: required identity of the listen.
    #[serde(deserialize_with = "required_count")]
    pub listened_at: i64,
    /// What was played.
    pub track_metadata: TrackMetadata,
}

/// A listen's track metadata. The names are what the client submitted;
/// `additional_info` is free-form, so its fields are read loosely.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackMetadata {
    /// Track title.
    #[serde(default)]
    pub track_name: Option<String>,
    /// Artist name.
    #[serde(default)]
    pub artist_name: Option<String>,
    /// Release title.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_name: Option<String>,
    /// Submitter-supplied extras.
    #[serde(default)]
    pub additional_info: Option<AdditionalInfo>,
    /// ListenBrainz's own MusicBrainz mapping, when it has one.
    #[serde(default)]
    pub mbid_mapping: Option<MbidMapping>,
}

/// The submitter's MBIDs. Free-form upstream, so a value that is not text
/// reads as absent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AdditionalInfo {
    /// Recording MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub recording_mbid: Option<String>,
    /// Release MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_mbid: Option<String>,
}

/// ListenBrainz's MusicBrainz mapping of a listen.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MbidMapping {
    /// Recording MBID.
    #[serde(default)]
    pub recording_mbid: Option<String>,
    /// Release MBID.
    #[serde(default)]
    pub release_mbid: Option<String>,
    /// Artist MBIDs.
    #[serde(default)]
    pub artist_mbids: Option<Vec<String>>,
}

/// `POST /1/metadata/recording/` with `inc=release`: recording MBID to its
/// metadata.
///
/// Verified against api.listenbrainz.org/1/metadata/recording/ on 2026-11-02: tests/fixtures/listenbrainz/recording_metadata_post.json.
pub type RecordingMetadataAnswer = HashMap<String, RecordingMetadata>;

/// One recording's metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingMetadata {
    /// The release the recording resolves to, under `inc=release`.
    #[serde(default)]
    pub release: Option<RecordingRelease>,
}

/// The release a recording resolves to.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingRelease {
    /// Release-group MBID: the identity the lookup exists for.
    pub release_group_mbid: String,
}

/// `GET /1/metadata/release_group/` with `inc=artist tag release`:
/// release-group MBID to its metadata. An unknown MBID is left out, so an
/// answer for only unknown ids is `{}`.
///
/// Verified against api.listenbrainz.org/1/metadata/release_group/ on 2026-11-02: tests/fixtures/listenbrainz/release_group_metadata.json.
/// Verified against api.listenbrainz.org/1/metadata/release_group/ on 2026-11-02: tests/fixtures/listenbrainz/release_group_metadata_unknown.json.
pub type ReleaseGroupMetadataAnswer = HashMap<String, ReleaseGroupEntry>;

/// One release group's metadata. Only the tags are read.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseGroupEntry {
    /// Artist and release-group tags.
    #[serde(default)]
    pub tag: Option<TagGroups>,
}

/// The tag lists of one release group.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TagGroups {
    /// Tags on the credited artists.
    #[serde(default)]
    pub artist: Vec<TagItem>,
    /// Tags on the release group.
    #[serde(default)]
    pub release_group: Vec<TagItem>,
}

/// One tag. Curated genres carry `genre_mbid`; folksonomy tags omit it.
#[derive(Debug, Clone, Deserialize)]
pub struct TagItem {
    /// Tag text: required identity.
    pub tag: String,
    /// How many people applied it.
    #[serde(deserialize_with = "required_count")]
    pub count: i64,
    /// Curated genre MBID.
    #[serde(default)]
    pub genre_mbid: Option<String>,
}

/// `POST /1/popularity/release-group`: one row per asked MBID.
///
/// Verified against api.listenbrainz.org/1/popularity/release-group on 2026-11-02: tests/fixtures/listenbrainz/popularity_release_group_post.json.
/// Not verified: the row for an MBID ListenBrainz does not know, so its
/// count stays optional.
pub type ReleaseGroupPopularityAnswer = Vec<ReleaseGroupPopularity>;

/// One release group's listen count.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseGroupPopularity {
    /// Release-group MBID: required identity.
    pub release_group_mbid: String,
    /// Total listens; absent or not a whole number when ListenBrainz has
    /// no count, and the row is skipped.
    #[serde(default, deserialize_with = "loose_count")]
    pub total_listen_count: Option<i64>,
}

/// `GET /1/popularity/top-release-groups-for-artist/{mbid}`, most played
/// first.
///
/// Verified against api.listenbrainz.org/1/popularity/top-release-groups-for-artist/{mbid} on 2026-11-02: tests/fixtures/listenbrainz/top_release_groups_radiohead.json.
pub type TopReleaseGroupsAnswer = Vec<TopReleaseGroupRow>;

/// One of an artist's top release groups.
#[derive(Debug, Clone, Deserialize)]
pub struct TopReleaseGroupRow {
    /// Release-group MBID: required identity.
    pub release_group_mbid: String,
    /// Total listens: on every recorded row. The row is skipped if null.
    #[serde(deserialize_with = "loose_count")]
    pub total_listen_count: Option<i64>,
    /// The release group's display fields.
    #[serde(default)]
    pub release_group: Option<Named>,
    /// The credited artist's display fields.
    #[serde(default)]
    pub artist: Option<Named>,
}

/// A nested object read only for its name.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Named {
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// `GET /1/popularity/top-recordings-for-artist/{mbid}`, most played first.
///
/// Verified against api.listenbrainz.org/1/popularity/top-recordings-for-artist/{mbid} on 2026-11-02: tests/fixtures/listenbrainz/top_recordings_radiohead.json.
pub type TopRecordingsAnswer = Vec<TopRecordingRow>;

/// One of an artist's top recordings.
#[derive(Debug, Clone, Deserialize)]
pub struct TopRecordingRow {
    /// Recording title, which keys the row: on every recorded row. The row
    /// is skipped if it is null or blank.
    #[serde(deserialize_with = "loose_text")]
    pub recording_name: Option<String>,
    /// Credited artist name.
    #[serde(default)]
    pub artist_name: Option<String>,
    /// Total listens: on every recorded row. The row is skipped if null.
    #[serde(deserialize_with = "loose_count")]
    pub total_listen_count: Option<i64>,
    /// Recording MBID.
    #[serde(default)]
    pub recording_mbid: Option<String>,
    /// Release title.
    #[serde(default)]
    pub release_name: Option<String>,
    /// Release MBID.
    #[serde(default)]
    pub release_mbid: Option<String>,
}

/// `GET /1/lb-radio/artist/{mbid}`: similar-artist MBID to a sample of its
/// recordings.
///
/// Verified against api.listenbrainz.org/1/lb-radio/artist/{mbid} on 2026-11-02: tests/fixtures/listenbrainz/lb_radio_artist.json.
pub type RadioArtistAnswer = HashMap<String, Vec<RadioRecording>>;

/// One sampled recording of a similar artist.
#[derive(Debug, Clone, Deserialize)]
pub struct RadioRecording {
    /// The similar artist's name.
    #[serde(default)]
    pub similar_artist_name: Option<String>,
    /// Total listens of the recording: on every recorded row. A null adds
    /// nothing to the artist's sum.
    #[serde(deserialize_with = "loose_count")]
    pub total_listen_count: Option<i64>,
}

/// `GET /1/validate-token`.
///
/// Verified against api.listenbrainz.org/1/validate-token on 2026-11-02: tests/fixtures/listenbrainz/validate_token_invalid.json.
/// A made-up token gets a 200 with `valid: false`, not a 401.
/// Not verified: the answer to a good token, recorded later.
#[derive(Debug, Clone, Deserialize)]
pub struct ValidateTokenAnswer {
    /// The verdict: required, so a changed answer is never "invalid".
    pub valid: bool,
    /// The account the token belongs to, on a valid token.
    #[serde(default)]
    pub user_name: Option<String>,
}

/// `GET /1/user/{user}/listen-count`.
///
/// Verified against api.listenbrainz.org/1/user/{user}/listen-count on 2026-11-02: tests/fixtures/listenbrainz/listen_count_unknown_user.json (the 404 only).
/// Not verified: the 200 answer needs an account, recorded later.
#[derive(Debug, Clone, Deserialize)]
pub struct ListenCountAnswer {
    /// The envelope.
    pub payload: ListenCountPayload,
}

/// The listen-count envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ListenCountPayload {
    /// The user's listen count: required, so a changed answer is never
    /// "0 listens".
    #[serde(deserialize_with = "required_count")]
    pub count: i64,
}

/// A count as ListenBrainz writes it: an integer, or a float with no
/// fraction.
#[derive(Deserialize)]
#[serde(untagged)]
enum Count {
    Int(i64),
    Float(f64),
    Other(IgnoredAny),
}

impl Count {
    fn value(self) -> Option<i64> {
        match self {
            Self::Int(count) => Some(count),
            Self::Float(float) if float.is_finite() && float.fract() == 0.0 => Some(float as i64),
            Self::Float(_) | Self::Other(_) => None,
        }
    }
}

/// A count that must be there and be a whole number.
fn required_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    Count::deserialize(deserializer)?
        .value()
        .ok_or_else(|| serde::de::Error::custom("expected a whole-number count"))
}

/// A count where anything but a whole number reads as absent.
fn loose_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    Ok(Count::deserialize(deserializer)?.value())
}

/// Text where anything but a non-blank string reads as absent.
fn loose_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Text(String),
        Other(IgnoredAny),
    }
    Ok(match Loose::deserialize(deserializer)? {
        Loose::Text(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Loose::Text(_) | Loose::Other(_) => None,
    })
}
