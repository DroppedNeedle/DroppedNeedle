//! ListenBrainz wire shapes for every read the client makes.
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
/// Verified against api.listenbrainz.org/1/user/{user}/listens on 2026-11-03: tests/fixtures/listenbrainz/listens_account.json.
/// Verified against api.listenbrainz.org/1/user/{user}/listens on 2026-11-02: tests/fixtures/listenbrainz/listens_unknown_user.json (the 404 only).
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
/// Verified against api.listenbrainz.org/1/user/{user}/listen-count on 2026-11-03: tests/fixtures/listenbrainz/listen_count_account.json.
/// Verified against api.listenbrainz.org/1/user/{user}/listen-count on 2026-11-02: tests/fixtures/listenbrainz/listen_count_unknown_user.json (the 404 only).
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

/// `GET /1/stats/sitewide/artists` and `GET /1/stats/user/{user}/artists`.
///
/// Verified against api.listenbrainz.org/1/stats/sitewide/artists on 2026-11-02: tests/fixtures/listenbrainz/sitewide_artists.json.
/// The account answered 204 on 2026-11-03 (`count=5`), which reads as an
/// empty list. Not verified: the 200 answer of /1/stats/user/{user}/artists.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtistStatsAnswer {
    /// The envelope.
    pub payload: ArtistStatsPayload,
}

/// The artist stats envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtistStatsPayload {
    /// The ranked artists. Required: an answer without the list is not "no
    /// artists". A list not computed yet is a 204.
    pub artists: Vec<ArtistStatRow>,
}

/// One ranked artist. Name and count are on every recorded row; the row is
/// skipped if either is null.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtistStatRow {
    /// Artist name, which keys the row.
    #[serde(deserialize_with = "loose_text")]
    pub artist_name: Option<String>,
    /// Listens in the range.
    #[serde(deserialize_with = "loose_count")]
    pub listen_count: Option<i64>,
    /// The one MBID the sitewide list sends (recorded 2026-11-02).
    #[serde(default, deserialize_with = "loose_text")]
    pub artist_mbid: Option<String>,
    /// The MBID list the user list is expected to send. Not verified: kept
    /// until the user answer is recorded.
    #[serde(default)]
    pub artist_mbids: Option<Vec<String>>,
}

/// `GET /1/stats/sitewide/release-groups` and
/// `GET /1/stats/user/{user}/release-groups`.
///
/// Verified against api.listenbrainz.org/1/stats/sitewide/release-groups on 2026-11-02: tests/fixtures/listenbrainz/sitewide_release_groups.json.
/// The account answered 204 on 2026-11-03 (`count=5`), which reads as an
/// empty list. Not verified: the 200 answer of /1/stats/user/{user}/release-groups.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseGroupStatsAnswer {
    /// The envelope.
    pub payload: ReleaseGroupStatsPayload,
}

/// The release-group stats envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseGroupStatsPayload {
    /// The ranked release groups. Required, as for the artist list.
    pub release_groups: Vec<ReleaseGroupStatRow>,
}

/// One ranked release group. Title, artist and count are on every recorded
/// row; the row is skipped if any is null.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseGroupStatRow {
    /// Release group title.
    #[serde(deserialize_with = "loose_text")]
    pub release_group_name: Option<String>,
    /// Credited artist.
    #[serde(deserialize_with = "loose_text")]
    pub artist_name: Option<String>,
    /// Listens in the range.
    #[serde(deserialize_with = "loose_count")]
    pub listen_count: Option<i64>,
    /// Release group MBID, when mapped.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_group_mbid: Option<String>,
    /// Artist MBIDs, when mapped.
    #[serde(default)]
    pub artist_mbids: Option<Vec<String>>,
}

/// `GET /1/stats/user/{user}/recordings`.
///
/// The account answered 204 on 2026-11-03 (`count=5`), which reads as an
/// empty list. Not verified: the 200 answer.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingStatsAnswer {
    /// The envelope.
    pub payload: RecordingStatsPayload,
}

/// The recording stats envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingStatsPayload {
    /// The ranked recordings. Required, as for the artist list.
    pub recordings: Vec<RecordingStatRow>,
}

/// One ranked recording. The row is skipped if a name or the count is null.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingStatRow {
    /// Track title.
    #[serde(deserialize_with = "loose_text")]
    pub track_name: Option<String>,
    /// Credited artist.
    #[serde(deserialize_with = "loose_text")]
    pub artist_name: Option<String>,
    /// Listens in the range.
    #[serde(deserialize_with = "loose_count")]
    pub listen_count: Option<i64>,
    /// Recording MBID, when mapped.
    #[serde(default, deserialize_with = "loose_text")]
    pub recording_mbid: Option<String>,
}

/// `GET /1/user/{user}/similar-users`.
///
/// Verified against api.listenbrainz.org/1/user/{user}/similar-users on 2026-11-03: tests/fixtures/listenbrainz/similar_users_account.json (an empty list, for a new account).
/// Not verified: a row, since the account has none.
#[derive(Debug, Clone, Deserialize)]
pub struct SimilarUsersAnswer {
    /// The similar users, most similar first.
    pub payload: Vec<SimilarUserRow>,
}

/// One similar user.
#[derive(Debug, Clone, Deserialize)]
pub struct SimilarUserRow {
    /// The user's name. The row is skipped if it is null or blank.
    #[serde(deserialize_with = "loose_text")]
    pub user_name: Option<String>,
}

/// `GET /1/stats/user/{user}/genre-activity`.
///
/// Verified against api.listenbrainz.org/1/stats/user/{user}/genre-activity on 2026-11-07: tests/fixtures/listenbrainz/genre_activity_user.json.
/// A list not computed yet is a 204, which reads as an empty list.
#[derive(Debug, Clone, Deserialize)]
pub struct GenreActivityAnswer {
    /// The envelope.
    pub payload: GenreActivityPayload,
}

/// The genre activity envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct GenreActivityPayload {
    /// One row per genre and hour bucket. Required, as for the artist list.
    pub genre_activity: Vec<GenreActivityRow>,
}

/// One genre in one hour bucket.
#[derive(Debug, Clone, Deserialize)]
pub struct GenreActivityRow {
    /// Genre label. The row is skipped if it is null or blank.
    #[serde(deserialize_with = "loose_text")]
    pub genre: Option<String>,
    /// Listens in the bucket. An absent count adds nothing to the genre.
    #[serde(default, deserialize_with = "loose_count")]
    pub listen_count: Option<i64>,
}

/// `GET /1/user/{user}/fresh_releases`.
///
/// Verified against api.listenbrainz.org/1/user/{user}/fresh_releases on 2026-11-03: tests/fixtures/listenbrainz/fresh_releases_account.json (an empty list, for a new account).
/// Not verified: a row, since the account has none.
#[derive(Debug, Clone, Deserialize)]
pub struct FreshReleasesAnswer {
    /// The envelope.
    pub payload: FreshReleasesPayload,
}

/// The fresh-releases envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct FreshReleasesPayload {
    /// The releases. Required: an answer without the list is not "nothing new".
    pub releases: Vec<FreshReleaseRow>,
}

/// One fresh release. The row is skipped if its id or title is null or blank.
#[derive(Debug, Clone, Deserialize)]
pub struct FreshReleaseRow {
    /// Release group MBID.
    #[serde(deserialize_with = "loose_text")]
    pub release_group_mbid: Option<String>,
    /// Release title.
    #[serde(deserialize_with = "loose_text")]
    pub release_name: Option<String>,
    /// Credited artist.
    #[serde(default, deserialize_with = "loose_text")]
    pub artist_credit_name: Option<String>,
    /// Artist MBIDs.
    #[serde(default)]
    pub artist_mbids: Option<Vec<String>>,
}

/// `GET /1/feedback/user/{user}/get-feedback`.
///
/// Verified against api.listenbrainz.org/1/feedback/user/{user}/get-feedback on 2026-11-03 with `score=1&count=5`: tests/fixtures/listenbrainz/feedback_account.json. Its rows carry no metadata.
/// Not verified: `metadata=true`, which the loved-recordings and loved-artist reads send, so the `track_metadata` shape stays unconfirmed.
/// No spelling is removed: the recording covers the count read only, and both reads decode through this type. A list under no spelling fails decoding.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FeedbackAnswer {
    /// The body inside a `payload`.
    Wrapped {
        /// The body.
        payload: FeedbackBody,
    },
    /// The body at the top level.
    Direct(FeedbackBody),
}

impl FeedbackAnswer {
    /// The feedback rows, under whichever spelling they came.
    #[must_use]
    pub fn rows(self) -> Vec<FeedbackRow> {
        let (Self::Wrapped { payload: body } | Self::Direct(body)) = self;
        match body {
            FeedbackBody::Rows(rows) | FeedbackBody::Keyed { feedback: rows } => rows,
        }
    }
}

/// The feedback list, bare or under a key.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FeedbackBody {
    /// A bare list.
    Rows(Vec<FeedbackRow>),
    /// A list under `feedback` or `recordings`.
    Keyed {
        /// The rows.
        #[serde(alias = "recordings")]
        feedback: Vec<FeedbackRow>,
    },
}

/// One feedback row. Nothing here is required: the list is what the sample
/// counts, and a row with no title anywhere is skipped where titles matter.
#[derive(Debug, Clone, Deserialize)]
pub struct FeedbackRow {
    /// Metadata, first spelling.
    #[serde(default)]
    pub recording_metadata: Option<FeedbackMetadata>,
    /// Metadata, second spelling.
    #[serde(default)]
    pub track_metadata: Option<FeedbackMetadata>,
    /// Metadata, third spelling.
    #[serde(default)]
    pub metadata: Option<FeedbackMetadata>,
    /// Track title beside the metadata.
    #[serde(default, deserialize_with = "loose_text")]
    pub track_name: Option<String>,
    /// Artist name beside the metadata.
    #[serde(default, deserialize_with = "loose_text")]
    pub artist_name: Option<String>,
    /// Release title beside the metadata.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_name: Option<String>,
    /// Recording MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub recording_mbid: Option<String>,
    /// Release MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_mbid: Option<String>,
}

/// The metadata of one feedback row.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FeedbackMetadata {
    /// Track title.
    #[serde(default, deserialize_with = "loose_text")]
    pub track_name: Option<String>,
    /// Track title, second spelling.
    #[serde(default, deserialize_with = "loose_text")]
    pub recording_name: Option<String>,
    /// Artist name.
    #[serde(default, deserialize_with = "loose_text")]
    pub artist_name: Option<String>,
    /// Artist name, second spelling.
    #[serde(default, deserialize_with = "loose_text")]
    pub artist: Option<String>,
    /// Release title.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_name: Option<String>,
    /// Release title, second spelling.
    #[serde(default, deserialize_with = "loose_text")]
    pub album_name: Option<String>,
    /// Recording MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub recording_mbid: Option<String>,
    /// Release MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_mbid: Option<String>,
    /// Artist MBIDs.
    #[serde(default)]
    pub artist_mbids: Option<Vec<String>>,
    /// A lone artist MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub artist_mbid: Option<String>,
    /// ListenBrainz's own MusicBrainz mapping.
    #[serde(default)]
    pub mbid_mapping: Option<MbidMapping>,
}

/// `GET /1/user/{user}/playlists/recommendations`.
///
/// Verified against api.listenbrainz.org/1/user/{user}/playlists/recommendations on 2026-11-03: tests/fixtures/listenbrainz/recommendation_playlists_account.json (an empty list, for a new account).
/// Not verified: an entry of `playlists`. The older code read a 404 as "none
/// yet"; no 404 is recorded for this endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct RecommendationPlaylistsAnswer {
    /// The playlists. Required: an answer without the list is not "none".
    pub playlists: Vec<PlaylistEntry>,
}

/// One entry of the recommendations list.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistEntry {
    /// The JSPF playlist.
    pub playlist: PlaylistHeader,
}

/// The header of a JSPF playlist.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistHeader {
    /// The playlist's URL, whose last segment is its id: required. The entry
    /// is skipped if it is null or blank.
    #[serde(deserialize_with = "loose_text")]
    pub identifier: Option<String>,
    /// JSPF extensions.
    #[serde(default)]
    pub extension: Option<PlaylistExtensions>,
}

/// The JSPF extensions of a playlist.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistExtensions {
    /// The MusicBrainz playlist extension.
    #[serde(default, rename = "https://musicbrainz.org/doc/jspf#playlist")]
    pub musicbrainz: Option<PlaylistExtension>,
}

/// The MusicBrainz playlist extension.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistExtension {
    /// Its extra fields.
    #[serde(default)]
    pub additional_metadata: Option<PlaylistAdditionalMetadata>,
}

/// A playlist's extra fields.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistAdditionalMetadata {
    /// What made the playlist.
    #[serde(default)]
    pub algorithm_metadata: Option<AlgorithmMetadata>,
}

/// What made a playlist.
#[derive(Debug, Clone, Deserialize)]
pub struct AlgorithmMetadata {
    /// The algorithm, such as `weekly-jams`.
    #[serde(default, deserialize_with = "loose_text")]
    pub source_patch: Option<String>,
}

/// `GET /1/playlist/{id}`. A 404 is a playlist that was replaced.
///
/// Not verified: no playlist id to record. The account has no recommended
/// playlist to take one from (2026-11-03).
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistAnswer {
    /// The JSPF playlist.
    pub playlist: PlaylistBody,
}

/// A JSPF playlist with its tracks.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistBody {
    /// Playlist title.
    #[serde(default, deserialize_with = "loose_text")]
    pub title: Option<String>,
    /// Creation date as ListenBrainz sends it.
    #[serde(default, deserialize_with = "loose_text")]
    pub date: Option<String>,
    /// Tracks in playlist order.
    #[serde(default)]
    pub track: Option<Vec<PlaylistTrack>>,
}

/// One JSPF track. JSPF makes every field optional and no answer is recorded
/// yet, so a track missing its title or creator is skipped, not a failed
/// playlist.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistTrack {
    /// Track title.
    #[serde(default, deserialize_with = "loose_text")]
    pub title: Option<String>,
    /// Credited artist.
    #[serde(default, deserialize_with = "loose_text")]
    pub creator: Option<String>,
    /// Album title.
    #[serde(default, deserialize_with = "loose_text")]
    pub album: Option<String>,
    /// Identifier URLs; the recording's holds its MBID.
    #[serde(default, deserialize_with = "loose_strings")]
    pub identifier: Vec<String>,
    /// Length in milliseconds.
    #[serde(default, deserialize_with = "loose_count")]
    pub duration: Option<i64>,
    /// JSPF extensions.
    #[serde(default)]
    pub extension: Option<TrackExtensions>,
}

/// The JSPF extensions of a track.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackExtensions {
    /// The MusicBrainz track extension.
    #[serde(default, rename = "https://musicbrainz.org/doc/jspf#track")]
    pub musicbrainz: Option<TrackExtension>,
}

/// The MusicBrainz track extension.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackExtension {
    /// Its extra fields.
    #[serde(default)]
    pub additional_metadata: Option<TrackAdditionalMetadata>,
}

/// A track's extra fields.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackAdditionalMetadata {
    /// The credited artists, in order.
    #[serde(default)]
    pub artists: Option<Vec<TrackArtist>>,
    /// The release the cover art comes from.
    #[serde(default, deserialize_with = "loose_text")]
    pub caa_release_mbid: Option<String>,
}

/// One credited artist of a track.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackArtist {
    /// Artist MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub artist_mbid: Option<String>,
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

/// A list of strings where anything else reads as empty.
fn loose_strings<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        List(Vec<String>),
        Other(IgnoredAny),
    }
    Ok(match Loose::deserialize(deserializer)? {
        Loose::List(list) => list,
        Loose::Other(_) => Vec::new(),
    })
}
