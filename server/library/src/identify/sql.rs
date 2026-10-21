//! The identify store's reads of one album's or one track's MusicBrainz
//! match.

droppedneedle_kernel::sql! {
    /// One album's match (`?1`, provider `?2`).
    pub(super) ALBUM_IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source, \
             row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// One track's match (`?1`, provider `?2`).
    pub(super) TRACK_IDENTITY = "SELECT recording_mbid, release_track_mbid, decision_source, \
             row_revision FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2";
}
