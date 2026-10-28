//! The flows' statements: the upgrade worklist's reads of the library and
//! the download tasks, and the admin directory.

use droppedneedle_kernel::db::statement::ListLen;

use crate::landing::sql::OWNED_TRACKS_FROM;

droppedneedle_kernel::sql! {
    /// Every indexed track of an identified album with its quality facts:
    /// `(group, title, artist, year, artist mbid, format, bitrate, depth)`.
    pub(crate) IDENTIFIED_TRACKS = "SELECT \
             lower(COALESCE(ai.release_group_mbid, t.embedded_release_group_mbid)), \
             b.title, b.album_artist_name, b.year, ae.provider_artist_id, \
             t.file_format, t.bit_rate, t.bit_depth \
         FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
         LEFT JOIN local_album_external_identities ai ON ai.local_album_id = b.id \
         LEFT JOIN local_artist_external_identities ae \
           ON ae.local_artist_id = b.album_artist_id \
         WHERE t.availability = 'indexed' \
           AND COALESCE(ai.release_group_mbid, t.embedded_release_group_mbid, '') <> ''";
    /// The quality facts of the library's tracks of release group `?1`.
    pub(crate) GROUP_TRACK_QUALITY = "SELECT t.file_format, t.bit_rate, t.bit_depth "
        + OWNED_TRACKS_FROM;
    /// The quality facts of the library's tracks of recording `?1`. The
    /// `t.id IN` list names every track either id could match, through the
    /// lowered-id indexes, so the tracks are reached by key.
    pub(crate) RECORDING_TRACK_QUALITY = "SELECT t.file_format, t.bit_rate, t.bit_depth \
         FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         WHERE t.availability = 'indexed' \
           AND t.id IN (SELECT local_track_id FROM local_track_external_identities \
               WHERE lower(recording_mbid) = ?1 \
             UNION ALL SELECT id FROM local_tracks \
               WHERE lower(COALESCE(embedded_recording_mbid, '')) = ?1) \
           AND lower(COALESCE(ti.recording_mbid, t.embedded_recording_mbid, '')) = ?1";
    /// The quality facts of several tracks: `(id, format, bitrate, depth)`.
    family TRACK_QUALITY_IN = track_quality_in;
    /// Release groups with an open held entry.
    pub(crate) HELD_GROUPS = "SELECT DISTINCT lower(release_group_mbid) \
         FROM acquire_held_entries \
         WHERE state IN ('held', 'placing') AND release_group_mbid IS NOT NULL";
    /// Finished upgrade tasks by album, newest first within one:
    /// `(group, status, finished at)`.
    pub(crate) UPGRADE_HISTORY = "SELECT lower(release_group_mbid), status, \
             COALESCE(completed_at, cancelled_at, updated_at) / 1000 \
         FROM download_tasks \
         WHERE origin = 'upgrade' AND release_group_mbid <> '' \
           AND status IN ('completed', 'partial', 'failed', 'cancelled') \
         ORDER BY 1, created_at DESC, rowid DESC";
    /// When the newest upgrade task was made.
    pub(crate) NEWEST_UPGRADE = "SELECT max(created_at) / 1000 FROM download_tasks \
         WHERE origin = 'upgrade'";
    /// The oldest admin.
    pub(crate) OLDEST_ADMIN = "SELECT id FROM auth_users WHERE role = 'admin' \
         ORDER BY created_at, id LIMIT 1";
}

/// The quality facts of `tracks` tracks: `(id, format, bitrate, depth)`.
pub(crate) fn track_quality_in(tracks: ListLen) -> String {
    format!(
        "SELECT id, file_format, bit_rate, bit_depth FROM local_tracks WHERE id IN ({})",
        vec!["?"; tracks.get()].join(",")
    )
}
