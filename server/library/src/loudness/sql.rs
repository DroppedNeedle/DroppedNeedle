//! The loudness loop's and the levelling route's statements.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use super::LoudnessOutcome;

/// `column` set from the new measurement, unless the row was measured at
/// the same stat revision and this one failed: then it keeps its own.
macro_rules! kept_or_new {
    ($column:literal) => {
        concat!(
            $column,
            " = CASE WHEN m.outcome = 'measured' AND excluded.outcome IS NOT 'measured' \
             AND m.stat_revision = excluded.stat_revision THEN m.",
            $column,
            " ELSE excluded.",
            $column,
            " END, "
        )
    };
}

droppedneedle_kernel::sql! {
    /// Up to `?2` indexed tracks under the library folders `?1` (a JSON
    /// array of root ids) with no measurement of their current bytes, the
    /// files the scan read last first, then the tracks measured before
    /// measurement version `?3`. An unmeasurable file is not tried again
    /// for a new version.
    pub(super) DUE = "SELECT t.id, t.root_id, t.file_path, t.relative_path, t.stat_revision, \
                      t.file_size_bytes, t.file_mtime_ns \
                      FROM local_tracks t \
                      LEFT JOIN library_loudness_measurements m ON m.local_track_id = t.id \
                      WHERE t.availability = 'indexed' \
                      AND t.root_id IN (SELECT value FROM json_each(?1)) \
                      AND (m.local_track_id IS NULL OR m.stat_revision IS NOT t.stat_revision \
                      OR (m.outcome = 'measured' AND m.measure_version < ?3)) \
                      ORDER BY m.stat_revision IS t.stat_revision, \
                      t.tags_read_at DESC, t.search_key DESC LIMIT ?2";
    pub(super) row DueTrack for [DUE] {
        id: String,
        root_id: String,
        file_path: String,
        relative_path: String,
        stat_revision: String,
        file_size_bytes: i64,
        file_mtime_ns: i64,
    }
    /// Keep track `?1`'s measurement (`?3` to `?10`), only while its file is
    /// still at stat revision `?2`: a file the scan has read again since is
    /// measured again instead. A failed measure of a row already measured at
    /// that revision (measuring again for a new version) keeps its numbers
    /// and takes only the new version, so it is not due again.
    pub(super) KEEP = (concat!(
        "INSERT INTO library_loudness_measurements AS m \
         (local_track_id, stat_revision, outcome, peak, blocks, measured_at, \
         sound_start_ms, fade_start_ms, sound_end_ms, measure_version) \
         SELECT id, stat_revision, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10 \
         FROM local_tracks WHERE id = ?1 AND stat_revision = ?2 \
         ON CONFLICT (local_track_id) DO UPDATE SET \
         stat_revision = excluded.stat_revision, ",
        kept_or_new!("outcome"),
        kept_or_new!("peak"),
        kept_or_new!("blocks"),
        kept_or_new!("measured_at"),
        kept_or_new!("sound_start_ms"),
        kept_or_new!("fade_start_ms"),
        kept_or_new!("sound_end_ms"),
        "measure_version = excluded.measure_version"
    ));
    /// Every indexed track of indexed track `?1`'s album, with its
    /// ReplayGain tags and its measurement of its current bytes, if any,
    /// with that measurement's sound edges and version.
    /// No rows when `?1` is not an indexed track.
    pub(super) ALBUM_LEVELS = "SELECT t.id, t.replaygain_track_gain AS tag_track_gain, \
                               t.replaygain_track_peak AS tag_track_peak, \
                               t.replaygain_album_gain AS tag_album_gain, \
                               t.replaygain_album_peak AS tag_album_peak, \
                               m.outcome, m.peak, m.blocks, m.sound_start_ms, \
                               m.fade_start_ms, m.sound_end_ms, m.measure_version \
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
        sound_start_ms: Option<i64>,
        fade_start_ms: Option<i64>,
        sound_end_ms: Option<i64>,
        measure_version: Option<i64>,
    }
}
