//! Sound analysis statements.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use super::SoundOutcome;

/// Indexed tracks with no row at analysis version `?1` for their current
/// bytes. A row of another version never counts.
macro_rules! no_current_row {
    () => {
        "NOT EXISTS (SELECT 1 FROM library_sound_analyses s \
         WHERE s.local_track_id = t.id AND s.analysis_version = ?1 \
         AND s.stat_revision = t.stat_revision)"
    };
}

droppedneedle_kernel::sql! {
    /// Up to `?3` indexed tracks under the library folders `?2` (a JSON
    /// array of root ids) with no row at analysis version `?1` for their
    /// current bytes, the files the scan read last first.
    pub(super) DUE = (concat!(
        "SELECT t.id, t.root_id, t.file_path, t.relative_path, t.stat_revision, \
         t.file_size_bytes, t.file_mtime_ns \
         FROM local_tracks t \
         WHERE t.availability = 'indexed' \
         AND t.root_id IN (SELECT value FROM json_each(?2)) AND ",
        no_current_row!(),
        " ORDER BY t.tags_read_at DESC, t.search_key DESC LIMIT ?3"
    ));
    pub(super) row DueTrack for [DUE] {
        id: String,
        root_id: String,
        file_path: String,
        relative_path: String,
        stat_revision: String,
        file_size_bytes: i64,
        file_mtime_ns: i64,
    }
    /// Whether any indexed track under the library folders `?2` (a JSON
    /// array of root ids) has no row at analysis version `?1` for its
    /// current bytes.
    pub(super) ANY_DUE = (concat!(
        "SELECT EXISTS (SELECT 1 FROM local_tracks t WHERE t.availability = 'indexed' \
         AND t.root_id IN (SELECT value FROM json_each(?2)) AND ",
        no_current_row!(),
        ")"
    ));
    /// Keep track `?1`'s result at analysis version `?3` (`?4` to `?9`),
    /// only while its file is still at stat revision `?2`: a file the scan
    /// has read again since is analysed again instead.
    pub(super) KEEP = "INSERT INTO library_sound_analyses \
                       (local_track_id, analysis_version, stat_revision, model, outcome, \
                       vector, tags, analysed_seconds, analysed_at) \
                       SELECT id, ?3, stat_revision, ?4, ?5, ?6, ?7, ?8, ?9 \
                       FROM local_tracks WHERE id = ?1 AND stat_revision = ?2 \
                       ON CONFLICT (local_track_id, analysis_version) DO UPDATE SET \
                       stat_revision = excluded.stat_revision, model = excluded.model, \
                       outcome = excluded.outcome, vector = excluded.vector, \
                       tags = excluded.tags, analysed_seconds = excluded.analysed_seconds, \
                       analysed_at = excluded.analysed_at";
    /// Drop every row of another analysis version than `?1`, once every
    /// indexed track under the library folders `?2` (a JSON array of root
    /// ids) has a row at `?1` (for any bytes): until then the old rows keep
    /// answering. A removed folder's tracks, still indexed, do not count.
    pub(super) DROP_OLD_VERSIONS = "DELETE FROM library_sound_analyses \
                                    WHERE analysis_version <> ?1 AND NOT EXISTS \
                                    (SELECT 1 FROM local_tracks t \
                                    WHERE t.availability = 'indexed' \
                                    AND t.root_id IN (SELECT value FROM json_each(?2)) \
                                    AND NOT EXISTS \
                                    (SELECT 1 FROM library_sound_analyses s \
                                    WHERE s.local_track_id = t.id AND s.analysis_version = ?1))";
    /// The analysis version reads answer from: the newest other version
    /// while one is kept (the current one `?1` does not cover the library
    /// yet), else `?1`.
    pub(super) SERVING_VERSION = "SELECT COALESCE(MAX(analysis_version), ?1) \
                                  FROM library_sound_analyses WHERE analysis_version <> ?1";
    /// Every track with a sound at analysis version `?1` for its current
    /// bytes, by track id.
    pub(super) VECTORS = "SELECT s.local_track_id AS track_id, s.vector, s.tags \
                          FROM library_sound_analyses s \
                          JOIN local_tracks t ON t.id = s.local_track_id \
                          AND t.stat_revision = s.stat_revision \
                          WHERE s.analysis_version = ?1 AND s.outcome = 'analysed' \
                          AND t.availability = 'indexed' ORDER BY s.local_track_id";
    pub(super) row Vector for [VECTORS, TRACK_VECTOR] {
        track_id: String,
        vector: Vec<u8>,
        tags: Vec<u8>,
    }
    /// Track `?2`'s sound at analysis version `?1`, for its current bytes.
    pub(super) TRACK_VECTOR = "SELECT s.local_track_id AS track_id, s.vector, s.tags \
                               FROM library_sound_analyses s \
                               JOIN local_tracks t ON t.id = s.local_track_id \
                               AND t.stat_revision = s.stat_revision \
                               WHERE s.analysis_version = ?1 AND s.local_track_id = ?2 \
                               AND s.outcome = 'analysed' AND t.availability = 'indexed'";
    /// The indexed tracks under the library folders `?2` (a JSON array of
    /// root ids), with how many have a row at analysis version `?1` for
    /// their current bytes: analysed, and not.
    pub(super) COVERAGE = "SELECT COUNT(*) AS tracks, \
                           COALESCE(SUM(s.outcome = 'analysed'), 0) AS analysed, \
                           COALESCE(SUM(s.outcome <> 'analysed'), 0) AS not_analysed \
                           FROM local_tracks t \
                           LEFT JOIN library_sound_analyses s ON s.local_track_id = t.id \
                           AND s.analysis_version = ?1 AND s.stat_revision = t.stat_revision \
                           WHERE t.availability = 'indexed' \
                           AND t.root_id IN (SELECT value FROM json_each(?2))";
    pub(super) row Coverage for [COVERAGE] {
        tracks: i64,
        analysed: i64,
        not_analysed: i64,
    }
    /// The track under the library folders `?2` last found not analysable
    /// at analysis version `?1`, for its current bytes: where it lies below
    /// its library folder and why.
    pub(super) LAST_NOT_ANALYSED = "SELECT t.relative_path, s.outcome \
                                    FROM library_sound_analyses s \
                                    JOIN local_tracks t ON t.id = s.local_track_id \
                                    AND t.stat_revision = s.stat_revision \
                                    WHERE s.analysis_version = ?1 AND s.outcome <> 'analysed' \
                                    AND t.availability = 'indexed' \
                                    AND t.root_id IN (SELECT value FROM json_each(?2)) \
                                    ORDER BY s.analysed_at DESC, s.local_track_id LIMIT 1";
    pub(super) row NotAnalysed for [LAST_NOT_ANALYSED] {
        relative_path: String,
        outcome: SoundOutcome,
    }
    /// The catalog revision, which every commit that changes catalog rows
    /// moves (a scan that reads a file again, a track that goes), and the
    /// newest analysis row's rowid, which every added row moves.
    pub(super) MARK = "SELECT COALESCE((SELECT value FROM library_catalog_revision \
                       WHERE singleton = 1), 0) AS catalog_revision, \
                       COALESCE((SELECT MAX(rowid) FROM library_sound_analyses), 0) \
                       AS newest_row";
    pub(super) row Mark for [MARK] {
        catalog_revision: i64,
        newest_row: i64,
    }
    /// Every track with a sound at analysis version `?1` for its current
    /// bytes, with what the search compares besides its sound: its title,
    /// its first credited artist (the library's and MusicBrainz's ids) and
    /// its recording.
    pub(super) SEARCH_TRACKS = (concat!(
        "SELECT s.local_track_id AS track_id, s.vector, s.tags, t.title AS title, \
         COALESCE(NULLIF(t.artist_name, ''), t.album_artist_name, '') AS artist_name, \
         ta.local_artist_id AS artist_id, \
         COALESCE(ae.provider_artist_id, t.embedded_artist_mbid) AS artist_mbid, \
         t.local_album_id AS album_id, t.album_title AS album_title, \
         t.duration_seconds AS duration_seconds, ",
        crate::recording_mbid!("te", "t"),
        " AS recording_mbid \
         FROM library_sound_analyses s \
         JOIN local_tracks t ON t.id = s.local_track_id \
         AND t.stat_revision = s.stat_revision \
         LEFT JOIN local_track_artists ta ON ta.local_track_id = t.id AND ta.position = 0 \
         LEFT JOIN local_artist_external_identities ae \
         ON ae.local_artist_id = ta.local_artist_id AND ae.provider = 'musicbrainz' \
         LEFT JOIN local_track_external_identities te ON te.local_track_id = t.id \
         AND te.provider = 'musicbrainz' \
         WHERE s.analysis_version = ?1 AND s.outcome = 'analysed' \
         AND t.availability = 'indexed' ORDER BY s.local_track_id"
    ));
    pub(super) row SearchTrack for [SEARCH_TRACKS] {
        track_id: String,
        vector: Vec<u8>,
        tags: Vec<u8>,
        title: String,
        artist_name: String,
        artist_id: Option<String>,
        artist_mbid: Option<String>,
        album_id: String,
        album_title: Option<String>,
        duration_seconds: Option<f64>,
        recording_mbid: Option<String>,
    }
    /// The sound bench's first tuning for person `?1`: the analysis version
    /// serving now (the newest other than `?3` that has rows, else `?3`),
    /// whether any track has a sound at it, and the library track with a
    /// sound at that version for its current bytes, as the sound search
    /// reads them, that they played most since `?2` (then latest played).
    /// One row; the seed's columns are NULL when they played no such track.
    pub(super) BENCH_SEED = "WITH v AS (SELECT COALESCE(MAX(analysis_version), ?3) AS version \
         FROM library_sound_analyses WHERE analysis_version <> ?3), \
         played AS (SELECT local_track_id AS id, COUNT(*) AS plays, MAX(played_at) AS last_at \
         FROM library_play_history WHERE user_id = ?1 AND played_at >= ?2 \
         AND local_track_id IS NOT NULL GROUP BY local_track_id), \
         seed AS (SELECT t.id AS track_id, t.title AS title, \
         COALESCE(NULLIF(t.artist_name, ''), t.album_artist_name, '') AS artist_name, \
         t.local_album_id AS album_id, t.album_title AS album_title, \
         t.duration_seconds AS duration_seconds \
         FROM played p JOIN local_tracks t ON t.id = p.id \
         JOIN library_sound_analyses s ON s.local_track_id = t.id \
         AND s.stat_revision = t.stat_revision \
         WHERE s.analysis_version = (SELECT version FROM v) AND s.outcome = 'analysed' \
         AND +t.availability = 'indexed' \
         ORDER BY p.plays DESC, p.last_at DESC, p.id LIMIT 1) \
         SELECT v.version AS analysis_version, \
         EXISTS (SELECT 1 FROM library_sound_analyses a JOIN local_tracks lt \
         ON lt.id = a.local_track_id AND lt.stat_revision = a.stat_revision \
         WHERE a.analysis_version = v.version AND a.outcome = 'analysed' \
         AND +lt.availability = 'indexed') AS analysed, \
         seed.track_id, seed.title, seed.artist_name, seed.album_id, seed.album_title, \
         seed.duration_seconds FROM v LEFT JOIN seed ON 1 = 1";
    pub(super) row BenchSeedRow for [BENCH_SEED] {
        analysis_version: i64,
        analysed: bool,
        track_id: Option<String>,
        title: Option<String>,
        artist_name: Option<String>,
        album_id: Option<String>,
        album_title: Option<String>,
        duration_seconds: Option<f64>,
    }
}
