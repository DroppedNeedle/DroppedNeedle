//! The loudness loop's and the levelling route's statements.

use super::LoudnessOutcome;

droppedneedle_kernel::sql! {
    /// Up to `?2` indexed tracks under the library folders `?1` (a JSON
    /// array of root ids) with no measurement of their current bytes, the
    /// files the scan read last first.
    pub(super) DUE = "SELECT t.id, t.root_id, t.file_path, t.relative_path, t.stat_revision, \
                      t.file_size_bytes, t.file_mtime_ns \
                      FROM local_tracks t \
                      LEFT JOIN library_loudness_measurements m ON m.local_track_id = t.id \
                      WHERE t.availability = 'indexed' \
                      AND t.root_id IN (SELECT value FROM json_each(?1)) \
                      AND (m.local_track_id IS NULL OR m.stat_revision IS NOT t.stat_revision) \
                      ORDER BY t.tags_read_at DESC, t.search_key DESC LIMIT ?2";
    pub(super) row DueTrack for [DUE] {
        id: String,
        root_id: String,
        file_path: String,
        relative_path: String,
        stat_revision: String,
        file_size_bytes: i64,
        file_mtime_ns: i64,
    }
    /// Keep track `?1`'s measurement (`?3` to `?6`), only while its file is
    /// still at stat revision `?2`: a file the scan has read again since is
    /// measured again instead.
    pub(super) KEEP = "INSERT INTO library_loudness_measurements \
                       (local_track_id, stat_revision, outcome, peak, blocks, measured_at) \
                       SELECT id, stat_revision, ?3, ?4, ?5, ?6 FROM local_tracks \
                       WHERE id = ?1 AND stat_revision = ?2 \
                       ON CONFLICT (local_track_id) DO UPDATE SET \
                       stat_revision = excluded.stat_revision, outcome = excluded.outcome, \
                       peak = excluded.peak, blocks = excluded.blocks, \
                       measured_at = excluded.measured_at";
    /// Every indexed track of indexed track `?1`'s album, with its
    /// ReplayGain tags and its measurement of its current bytes, if any.
    /// No rows when `?1` is not an indexed track.
    pub(super) ALBUM_LEVELS = "SELECT t.id, t.replaygain_track_gain AS tag_track_gain, \
                               t.replaygain_track_peak AS tag_track_peak, \
                               t.replaygain_album_gain AS tag_album_gain, \
                               t.replaygain_album_peak AS tag_album_peak, \
                               m.outcome, m.peak, m.blocks \
                               FROM local_tracks t \
                               LEFT JOIN library_loudness_measurements m \
                               ON m.local_track_id = t.id AND m.stat_revision = t.stat_revision \
                               WHERE t.availability = 'indexed' AND t.local_album_id = \
                               (SELECT local_album_id FROM local_tracks \
                               WHERE id = ?1 AND availability = 'indexed')";
    pub(super) row AlbumLevel for [ALBUM_LEVELS] {
        id: String,
        tag_track_gain: Option<f64>,
        tag_track_peak: Option<f64>,
        tag_album_gain: Option<f64>,
        tag_album_peak: Option<f64>,
        outcome: Option<LoudnessOutcome>,
        peak: Option<f64>,
        blocks: Option<Vec<u8>>,
    }
}
