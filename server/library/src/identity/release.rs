//! The identity an accepted match gives one file.

/// Accepted exact-edition identity for one file.
///
/// The release-track MBID is distinct from the recording MBID and is
/// never inferred from it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseIdentity {
    /// Accepted MusicBrainz release MBID.
    pub release_mbid: String,
    /// Release-group MBID the release belongs to.
    pub release_group_mbid: String,
    /// Recording MBID for this track.
    pub recording_mbid: String,
    /// Release-track MBID for this track on the accepted release.
    pub release_track_mbid: String,
    /// Accepted album identity revision, pinned separately from the
    /// per-track mapping revision.
    pub album_identity_revision: u64,
    /// Per-track mapping revision.
    pub mapping_revision: u64,
}
