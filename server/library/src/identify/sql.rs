//! The identify store's statements for one album's or one track's
//! MusicBrainz match: the reads the import seal makes on its own
//! transaction, and the guarded writes it shares with the store's saves.

droppedneedle_kernel::sql! {
    /// Decision sources an automatic pass may replace: its own rows and
    /// ids the tags carried.
    pub(super) fragment REVISABLE = "('automatic','embedded')";
    /// One album's match (`?1`, provider `?2`).
    pub(super) ALBUM_IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source, \
             row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    /// One track's match (`?1`, provider `?2`).
    pub(super) TRACK_IDENTITY = "SELECT recording_mbid, release_track_mbid, decision_source, \
             row_revision FROM local_track_external_identities \
             WHERE local_track_id = ?1 AND provider = ?2";
    /// Save an album's match. The write itself refuses to put an
    /// automatic or embedded row over a person's.
    pub(super) SAVE_ALBUM_IDENTITY = "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, release_mbid, decision_source, \
             selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, \
             decision_source = excluded.decision_source, \
             selected_at = excluded.selected_at, row_revision = excluded.row_revision \
             WHERE excluded.decision_source NOT IN " + REVISABLE + " \
             OR local_album_external_identities.decision_source IN " + REVISABLE;
    /// Save a track's match, guarded like the album's.
    pub(super) SAVE_TRACK_IDENTITY = "INSERT INTO local_track_external_identities \
             (local_track_id, provider, recording_mbid, release_track_mbid, decision_source, \
             selected_at, row_revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             recording_mbid = excluded.recording_mbid, \
             release_track_mbid = excluded.release_track_mbid, \
             decision_source = excluded.decision_source, \
             selected_at = excluded.selected_at, row_revision = excluded.row_revision \
             WHERE excluded.decision_source NOT IN " + REVISABLE + " \
             OR local_track_external_identities.decision_source IN " + REVISABLE;
}
