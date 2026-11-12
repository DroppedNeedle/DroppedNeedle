//! The scan store's newer statements: the failure row a window or page
//! records, and for its chunked clean-ups the rows each one reads before
//! it starts and the one-row deletes its chunks run.

// `sql!` needs a macro call in parentheses to take it as one part.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::{Choice, ListLen};
use droppedneedle_kernel::db::time::Millis;

use crate::identity::sql::PERSONS;
use crate::scan::models::{
    ArtworkSource, Availability, Counter, EffectivePolicy, FailureCode, GroupingSource, Phase,
    RequestedControl, ScanKind, ScanPhase, ScanTrigger, TagProvenance, Verdict,
};
use crate::scan::sqlite_store::sql::{
    MOVED_COLUMNS, MOVED_MATCH, SAME_PLACE, TRACK_GONE, TRACK_PRESENT, WALKED_PAST, awaits_admin,
    covers, fenced_by, held, held_back, holds, refused_scope, walked_past,
};
use crate::scan::store::RevisionKind;

impl Choice for Counter {
    fn all() -> Vec<Self> {
        Counter::ALL.to_vec()
    }
}

/// True when the track row a saved row of the table named points at is gone.
macro_rules! track_gone {
    ($table:literal) => {
        concat!(
            "NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.id = ",
            $table,
            ".file_id)"
        )
    };
}

/// True when the artist row `$artist` names (`?1` or a column) holds an id
/// other than `?2` on a credit, leaving out track `?3` and album `?4`.
macro_rules! holds_other_id {
    ($artist:literal) => {
        concat!(
            "(EXISTS(SELECT 1 FROM local_track_artists WHERE local_artist_id = ",
            $artist,
            " AND artist_mbid < ?2 AND local_track_id IS NOT ?3) \
             OR EXISTS(SELECT 1 FROM local_track_artists WHERE local_artist_id = ",
            $artist,
            " AND artist_mbid > ?2 AND local_track_id IS NOT ?3) \
             OR EXISTS(SELECT 1 FROM local_album_artists WHERE local_artist_id = ",
            $artist,
            " AND artist_mbid < ?2 AND local_album_id IS NOT ?4) \
             OR EXISTS(SELECT 1 FROM local_album_artists WHERE local_artist_id = ",
            $artist,
            " AND artist_mbid > ?2 AND local_album_id IS NOT ?4))"
        )
    };
}

droppedneedle_kernel::sql_choice! {
    /// Where a judgement takes the new album's held rows from.
    pub(super) enum HeldRows {
        /// The rows under a hold, at the end of run `?1`.
        UnderHolds,
        /// The remembered rows of album `?1` still indexed on it, at a look.
        Remembered,
    }
}

droppedneedle_kernel::sql! {
    /// Record one failure row of a run; a repeat of the same row is kept once.
    pub(super) RECORD_FAILURE = "INSERT OR IGNORE INTO library_scan_failures \
             (run_id, root_id, relative_path, failure_code, failure_detail, phase, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    /// Note that run `?1` owes its end passes, from pass `?2`.
    pub(super) OWE_END_PASSES = "UPDATE library_scan_runs SET next_end_pass = ?2, \
             end_pass_waits_used = 0, end_pass_retry_at = NULL WHERE id = ?1";
    /// The ended runs that owe end passes, oldest first: run, the next
    /// pass, end time, the waits used on it and when it is tried again.
    pub(super) OWED_END_PASSES = "SELECT id, next_end_pass, terminal_at, end_pass_waits_used, \
             end_pass_retry_at FROM library_scan_runs \
             WHERE next_end_pass IS NOT NULL AND terminal_at IS NOT NULL \
             ORDER BY terminal_at, id";
    /// The ended runs that still owe end passes, oldest first.
    pub(super) RUNS_OWING_END_PASSES = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs \
             WHERE terminal_at IS NOT NULL AND next_end_pass IS NOT NULL \
             ORDER BY terminal_at, id";
    /// Move run `?1` on to pass `?2`, or to none.
    pub(super) NEXT_END_PASS = "UPDATE library_scan_runs SET next_end_pass = ?2, \
             end_pass_waits_used = 0, end_pass_retry_at = NULL WHERE id = ?1";
    /// Run `?1`'s next pass is put off: `?2` waits used, tried again at `?3`.
    pub(super) PUT_OFF_END_PASS = "UPDATE library_scan_runs SET end_pass_waits_used = ?2, \
             end_pass_retry_at = ?3 WHERE id = ?1";
    /// The oldest finished run whose inventory waits for clean-up and that
    /// owes no end passes.
    pub(super) CLEANUP_TARGET = "SELECT id FROM library_scan_runs \
             WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 1 \
             AND next_end_pass IS NULL ORDER BY terminal_at, id LIMIT 1";
    /// A run's failure rows, oldest first.
    pub(super) RUN_FAILURES = "SELECT root_id, relative_path, failure_code, recorded_at, failure_detail, phase \
             FROM library_scan_failures WHERE run_id = ?1 ORDER BY rowid";
    /// One keyset page of a run's failure rows.
    pub(super) RUN_FAILURES_PAGE = "SELECT rowid, root_id, relative_path, failure_code, recorded_at, \
             failure_detail, phase FROM library_scan_failures \
             WHERE run_id = ?1 AND (?2 IS NULL OR rowid > ?2) \
             ORDER BY rowid LIMIT ?3";
    /// The rowids of one finished run's album moves.
    pub(super) ALBUM_MOVE_ROWIDS = "SELECT rowid FROM library_scan_album_moves WHERE run_id = ?1";
    /// The rowids of one finished run's inventory.
    pub(super) INVENTORY_ROWIDS = "SELECT rowid FROM library_scan_inventory WHERE run_id = ?1";
    /// Delete one album move of run `?2`.
    pub(super) DELETE_ALBUM_MOVE = "DELETE FROM library_scan_album_moves WHERE rowid = ?1 AND run_id = ?2";
    /// Delete one inventory row of run `?2`.
    pub(super) DELETE_INVENTORY_ROW = "DELETE FROM library_scan_inventory WHERE rowid = ?1 AND run_id = ?2";
    /// A finished run whose inventory is all gone stops waiting for clean-up.
    pub(super) CLEANUP_DONE = "UPDATE library_scan_runs SET inventory_cleanup_pending = 0 \
             WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM library_scan_inventory WHERE run_id = ?1)";
    /// Finished, cleaned runs past the newest `?1`, which history pruning removes.
    pub(super) PRUNED_RUNS = "SELECT id FROM library_scan_runs \
             WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 0 \
             ORDER BY terminal_at DESC, id DESC LIMIT -1 OFFSET ?1";
    /// Delete one run.
    pub(super) DELETE_RUN = "DELETE FROM library_scan_runs WHERE id = ?1";
    /// The rowids of a run's inventory rows whose scope is gone or was walked again since.
    pub(super) STALE_INVENTORY_ROWIDS = "SELECT i.rowid FROM library_scan_inventory i \
             LEFT JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 \
             AND (s.scope_sequence IS NULL OR s.discovery_generation != i.discovery_generation)";
    /// Delete one inventory row of run `?2` while it is still stale.
    pub(super) DELETE_STALE_INVENTORY_ROW = "DELETE FROM library_scan_inventory \
             WHERE rowid = ?1 AND run_id = ?2 AND NOT EXISTS ( \
             SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = library_scan_inventory.run_id \
             AND s.root_id = library_scan_inventory.root_id \
             AND s.relative_path = library_scan_inventory.scope_relative_path \
             AND s.discovery_generation = library_scan_inventory.discovery_generation)";
    /// Whether track `?1` has its edition hints stored, its track total
    /// included: a row from before totals were kept holds NULL there, and
    /// one read since holds the total or 0 for a file that names none.
    pub(super) HAS_EDITION_TAGS = "SELECT EXISTS(SELECT 1 FROM local_track_edition_tags \
             WHERE local_track_id = ?1 AND total_tracks IS NOT NULL)";
    /// Whether track `?1`'s row does not say if its length was measured.
    pub(super) LENGTH_UNREAD = "SELECT EXISTS(SELECT 1 FROM local_tracks \
             WHERE id = ?1 AND duration_measured IS NULL)";
    /// Record whether track `?1`, read at stat revision `?2`, has a measured
    /// length (`?3`), unless the row moved on since or already says.
    pub(super) FILL_LENGTH_MEASURED = "UPDATE local_tracks SET duration_measured = ?3 \
             WHERE id = ?1 AND stat_revision = ?2 AND duration_measured IS NULL";
    /// Store the edition hints of track `?1` read at stat revision `?2`, unless
    /// the row moved on since or already holds them.
    pub(super) FILL_EDITION_TAGS = "INSERT INTO local_track_edition_tags (local_track_id, media, \
             barcode, catalog_number, release_country, total_discs, release_date, total_tracks) \
             SELECT id, ?3, ?4, ?5, ?6, ?7, ?8, ?9 FROM local_tracks \
             WHERE id = ?1 AND stat_revision = ?2 \
             ON CONFLICT (local_track_id) DO NOTHING";
    /// Store the track total (`?3`, 0 when the file names none) of track
    /// `?1` read at stat revision `?2` in an edition row stored before
    /// totals were kept, unless the row moved on since.
    pub(super) FILL_TRACK_TOTAL = "UPDATE local_track_edition_tags SET total_tracks = ?3 \
             WHERE local_track_id = ?1 AND total_tracks IS NULL \
             AND EXISTS (SELECT 1 FROM local_tracks WHERE id = ?1 AND stat_revision = ?2)";
    /// The album of track `?1` when its MusicBrainz match is not settled:
    /// none, or an automatic one without an edition or still unconfirmed.
    /// Only `automatic`: a match v2 took from the files' own ids already
    /// rests on stronger evidence than edition hints. Albums a person
    /// detached, or whose review they turned down, are left alone.
    pub(super) UNSETTLED_ALBUM_OF_TRACK = "SELECT t.local_album_id FROM local_tracks t \
             WHERE t.id = ?1 AND NOT EXISTS (SELECT 1 FROM local_album_external_identities i \
               WHERE i.local_album_id = t.local_album_id AND i.provider = 'musicbrainz' \
               AND (i.decision_source != 'automatic' OR (i.release_mbid IS NOT NULL \
                 AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
                   WHERE s.local_album_id = t.local_album_id)))) \
             AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
               WHERE s.local_album_id = t.local_album_id \
               AND s.reason_code = 'DETACHED_BY_PERSON') \
             AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
               WHERE r.local_album_id = t.local_album_id \
               AND r.state IN ('rejected','dismissed','excluded'))";
    /// When run `?1` entered the phase whose time is running.
    pub(super) PHASE_STARTED_AT = "SELECT phase_started_at FROM library_scan_runs WHERE id = ?1";
    /// Write run `?1`'s state (`?2` to `?5`, from its one run state), the
    /// start time `?6` it keeps once set, its end `?7` with code `?8`, its
    /// phase clock `?9` and timings `?10`, changed at `?11`.
    pub(super) WRITE_RUN_STATE = "UPDATE library_scan_runs SET state = ?2, phase = ?3, \
             resume_phase = ?4, requested_control = ?5, started_at = COALESCE(started_at, ?6), \
             terminal_at = ?7, terminal_code = ?8, phase_started_at = ?9, \
             phase_timings_json = ?10, updated_at = ?11, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?1";
    pub(super) INSERT_ARTIST = "INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, \
         kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4, ?6, ?5, ?5) \
         ON CONFLICT (id) DO NOTHING";
    pub(super) LIVE_ARTIST = "SELECT id FROM ( \
         SELECT id, 0 AS rank FROM local_artists WHERE id = ?1 AND retired_into_artist_id IS NULL \
         UNION ALL SELECT r.id, 1 FROM local_artist_aliases x \
          JOIN local_artists r ON r.id = x.local_artist_id \
          WHERE x.alias = ?1 AND r.retired_into_artist_id IS NULL \
         UNION ALL SELECT r.id, 2 FROM local_artists a \
          JOIN local_artists r ON r.id = a.retired_into_artist_id \
          WHERE a.id = ?1 AND r.retired_into_artist_id IS NULL) \
         ORDER BY rank LIMIT 1";
    pub(super) HOLDERS = "SELECT min(id) FROM ( \
                 SELECT (SELECT local_artist_id FROM local_track_artists \
                  WHERE artist_mbid = ?1 AND local_artist_id > ?2 \
                  ORDER BY local_artist_id LIMIT 1) AS id \
                 UNION ALL SELECT (SELECT local_artist_id FROM local_album_artists \
                  WHERE artist_mbid = ?1 AND local_artist_id > ?2 \
                  ORDER BY local_artist_id LIMIT 1))";
    /// Whether a row `?1` exists, live or retired.
    pub(super) ARTIST_ROW_EXISTS = "SELECT EXISTS(SELECT 1 FROM local_artists WHERE id = ?1)";
    /// Live ordinary rows of folded name `?1`, neither sentinel (`?2`,
    /// `?3`) nor a Various Artists row, oldest first.
    pub(super) SAME_FOLDED_NAME_ROWS = "SELECT id, display_name FROM local_artists \
         WHERE folded_name = ?1 AND kind <> 'various_artists' \
         AND retired_into_artist_id IS NULL AND id NOT IN (?2, ?3) \
         ORDER BY created_at, id";
    pub(super) row SameFoldedNameRow for [SAME_FOLDED_NAME_ROWS] {
        id: String,
        display_name: String,
    }
    pub(super) CONVERGED_MBID_OF_ARTIST = "SELECT converged_mbid FROM local_artists WHERE id = ?1";
    pub(super) ARTIST_NAMES = "SELECT display_name, sort_name FROM local_artists WHERE id = ?1";
    pub(super) WRITE_ARTIST_NAMES = "UPDATE local_artists SET display_name = ?2, folded_name = ?3, \
                 normalized_name = ?3, sort_name = COALESCE(?4, sort_name), updated_at = ?5, \
                 row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) WRITE_ARTWORK_ROW = "INSERT INTO local_album_artwork \
                                 (local_album_id, source, source_locator, content_hash, \
                                  updated_at, version) \
                                 SELECT ?1, ?2, ?3, ?4, ?5, COALESCE((SELECT art_version \
                                     FROM local_album_artwork_checks \
                                     WHERE local_album_id = ?1), 0) + 1 \
                                 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
                                 ON CONFLICT(local_album_id) DO UPDATE SET \
                                 source = excluded.source, \
                                 source_locator = excluded.source_locator, \
                                 cover_url = NULL, \
                                 version = CASE WHEN content_hash IS excluded.content_hash \
                                     THEN version ELSE MAX(version + 1, excluded.version) END, \
                                 content_hash = excluded.content_hash, \
                                 updated_at = excluded.updated_at, \
                                 row_revision = row_revision + 1";
    pub(super) DROP_FILE_ARTWORK = "DELETE FROM local_album_artwork \
                             WHERE local_album_id = ?1 AND source IN ('folder', 'embedded')";
    pub(super) WRITE_ARTWORK_CHECK = "INSERT INTO local_album_artwork_checks \
                     (local_album_id, signature, checked_at, art_version) \
                     SELECT ?1, ?2, ?3, COALESCE((SELECT version FROM local_album_artwork \
                         WHERE local_album_id = ?1), 0) \
                     WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
                     ON CONFLICT(local_album_id) DO UPDATE SET \
                     signature = excluded.signature, checked_at = excluded.checked_at, \
                     art_version = MAX(art_version, excluded.art_version)";
    pub(super) ALBUM_TRACKS = "SELECT t.local_album_id, t.id, t.file_path, t.relative_path, \
         t.stat_revision, t.file_size_bytes FROM local_tracks t \
         JOIN local_albums a ON a.id = t.local_album_id \
         WHERE t.availability = 'indexed' AND a.retired_into_album_id IS NULL \
         ORDER BY t.local_album_id, t.disc_number, t.track_number, t.id";
    pub(super) ALL_ARTWORK = "SELECT local_album_id, source, source_locator, content_hash FROM local_album_artwork";
    pub(super) ALL_ARTWORK_CHECKS = "SELECT local_album_id, signature FROM local_album_artwork_checks";
    pub(super) ALL_LYRICS_CHECKS = "SELECT local_album_id, signature FROM local_album_lyrics_checks";
    pub(super) ALBUM_FOR_TRACK = "SELECT local_album_id FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed'";
    pub(super) TRACK_AT = "SELECT id FROM local_tracks \
                     WHERE root_id = ?1 AND relative_path = ?2 AND availability = 'indexed'";
    pub(super) MARK_MISSING = "UPDATE library_scan_runs SET missing_count = missing_count + ?1 WHERE id = ?2";
    pub(super) SCOPE_GUARDED = "SELECT EXISTS(SELECT 1 FROM library_scan_failures WHERE run_id = ?1 \
                 AND root_id = ?2 AND relative_path = ?3 AND failure_code = ?4)";
    pub(super) FOLDER_HOLDS_INDEXED_TRACKS = "SELECT EXISTS (SELECT 1 FROM local_tracks WHERE root_id = ?1 \
                 AND availability = 'indexed' \
                 AND relative_path > ?2 || '/' AND relative_path < ?2 || '0')";
    pub(super) ADD_REFUSED_SCOPE = "INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, \
                     relative_path, root_path, effective_policy, policy_revision, \
                     discovery_state, error_code) \
                     SELECT ?1, COALESCE(MAX(scope_sequence), -1) + 1, ?2, ?3, ?4, ?5, ?6, \
                     'unavailable', ?7 FROM library_scan_run_scopes WHERE run_id = ?1 \
                     ON CONFLICT (run_id, root_id, relative_path) DO UPDATE SET \
                     discovery_state = excluded.discovery_state, error_code = excluded.error_code";
    pub(super) SCOPE_RENAMED = "SELECT EXISTS(SELECT 1 FROM library_scan_run_scopes WHERE run_id = ?1 \
                 AND root_id = ?2 AND relative_path = ?3 AND error_code = ?4)";
    pub(super) REFRESH_TRACK_STAT = "UPDATE local_tracks SET stat_revision = ?1, file_size_bytes = ?2, \
                 file_mtime_ns = ?3, stat_revision_kind = 'exact' \
                 WHERE root_id = ?4 AND relative_path = ?5";
    pub(super) RECORD_CLASSIFY_FAILURE = "INSERT OR IGNORE INTO library_scan_failures \
                     (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                     recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', 0)";
    pub(super) SHARED_CATALOG_REVISION = "SELECT COALESCE((SELECT value FROM library_catalog_revision WHERE singleton = 1), 0)";
    pub(super) LOAD_CATALOG_MAP = "SELECT relative_path, id, stat_revision, stat_revision_kind, file_size_bytes, \
         file_mtime_ns, tags_read_at FROM local_tracks \
         WHERE root_id = ?1 AND (availability = 'indexed' OR manual_excluded = 1)";
    pub(super) PINNED_ALBUM_STILL_FITS = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
                 AND retired_into_album_id IS NULL AND title_folded = ?2 \
                 AND COALESCE(album_artist_name_folded, '') = ?3)";
    pub(super) ALBUM_HAS_GROUPING_KEY = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 AND grouping_key = ?2)";
    /// Run once per file. The unary plus keeps `retired_into_album_id IS NULL`,
    /// true of nearly every album, from ever being taken as an index key.
    pub(super) ALBUMS_FOR_GROUPING = "SELECT id, grouping_key = ?1 AS exact FROM local_albums \
             WHERE +retired_into_album_id IS NULL AND (grouping_key = ?1 \
             OR (?2 IS NOT NULL AND title_folded = ?3 AND album_artist_name_folded = ?4 \
               AND (grouping_key = ?2 OR (?2 = 'mbid:' AND grouping_key LIKE 'mbid:%')))) \
             ORDER BY created_at, id";
    pub(super) ALBUM_EXISTS = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1)";
    pub(super) GIVE_UP_PATH = "UPDATE local_tracks SET availability = 'missing', \
         missing_at = CASE WHEN availability = 'missing' THEN missing_at ELSE ?2 END, \
         relative_path = ?3, file_path = ?4, path_hash = ?5, row_revision = row_revision + 1 \
         WHERE id = ?1";
    pub(super) ALBUM_CREDIT_ROWS = "SELECT position, local_artist_id, credited_name, artist_mbid \
             FROM local_album_artists WHERE local_album_id = ?1 ORDER BY position";
    pub(super) TRACK_CREDIT_ROWS = "SELECT position, local_artist_id, credited_name, artist_mbid \
             FROM local_track_artists WHERE local_track_id = ?1 ORDER BY position";
    pub(super) RECORD_MOVES = "SELECT EXISTS (SELECT 1 FROM library_artist_credit_proofs \
                     WHERE subject_kind = ?1 AND subject_id = ?2 AND credit_position = ?3 \
                     AND source_local_artist_id = ?4)";
    pub(super) WRITE_ALBUM_ROW = "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, tag_album_title, tag_album_artist_name, \
         album_artist_id, album_artist_sort_name, year, original_release_date, primary_genre, \
         is_compilation, grouping_source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
         'automatic', ?16, ?16) \
         ON CONFLICT (id) DO UPDATE SET root_id = excluded.root_id, title = excluded.title, \
         title_folded = excluded.title_folded, album_artist_name = excluded.album_artist_name, \
         album_artist_name_folded = excluded.album_artist_name_folded, \
         tag_album_title = excluded.tag_album_title, \
         tag_album_artist_name = excluded.tag_album_artist_name, \
         album_artist_id = excluded.album_artist_id, \
         album_artist_sort_name = excluded.album_artist_sort_name, \
         year = COALESCE(excluded.year, local_albums.year), \
         original_release_date = COALESCE(excluded.original_release_date, \
         local_albums.original_release_date), \
         primary_genre = COALESCE(excluded.primary_genre, local_albums.primary_genre), \
         is_compilation = excluded.is_compilation, updated_at = excluded.updated_at";
    pub(super) WRITE_ALBUM_CREDIT = "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase, artist_mbid) VALUES (?1, ?2, ?3, 'main', ?4, ?5, ?6) \
         ON CONFLICT (local_album_id, position) DO UPDATE SET \
         local_artist_id = excluded.local_artist_id, credited_name = excluded.credited_name, \
         join_phrase = excluded.join_phrase, artist_mbid = excluded.artist_mbid";
    pub(super) DROP_ALBUM_CREDITS_FROM = "DELETE FROM local_album_artists WHERE local_album_id = ?1 AND position >= ?2";
    pub(super) TRACK_AT_PATH_FACTS = "SELECT id, local_album_id, availability, duration_seconds, \
             embedded_recording_mbid, title_folded, title_provenance, disc_number, \
             track_number, sample_rate, bit_depth, channels \
             FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) MOVE_TRACK_PATH = "UPDATE local_tracks SET root_id = ?2, relative_path = ?3, path_hash = ?4, \
                 file_path = ?5, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) TRACK_MEMBERSHIP = "SELECT membership_source, membership_locked = 1 AS locked FROM local_tracks WHERE id = ?1";
    pub(super) TRACK_EXISTS = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE id = ?1)";
    pub(super) WRITE_TRACK_ROW = "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, \
         tag_revision, tags_read_at, title, title_folded, artist_name, artist_name_folded, \
         album_title, album_title_folded, album_artist_name, album_artist_name_folded, \
         tag_album_title, tag_album_artist_name, disc_number, track_number, year, genre, \
         genre_folded, release_type, title_sort, artist_sort, album_sort, album_artist_sort, \
         disc_subtitle, is_compilation, embedded_release_group_mbid, embedded_release_mbid, \
         embedded_recording_mbid, embedded_release_track_mbid, embedded_artist_mbid, \
         embedded_album_artist_mbid, duration_seconds, file_format, bit_rate, sample_rate, \
         bit_depth, channels, replaygain_track_gain, replaygain_album_gain, \
         replaygain_track_peak, replaygain_album_peak, availability, ingest_source, \
         imported_at, membership_source, desired_policy_revision, applied_policy_revision, \
         applied_policy, title_provenance, album_title_provenance, album_artist_provenance, \
         duration_measured) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'exact', ?10, ?11, ?12, ?13, ?14, ?15, \
         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, \
         ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, \
         'indexed', 'scan', ?11, 'automatic', ?50, ?50, ?51, ?52, ?53, ?54, ?55) \
         ON CONFLICT (root_id, relative_path) DO UPDATE SET \
         local_album_id = CASE WHEN local_tracks.membership_locked = 1 \
           THEN local_tracks.local_album_id ELSE excluded.local_album_id END, \
         membership_source = CASE WHEN local_tracks.membership_locked = 1 \
           OR local_tracks.local_album_id = excluded.local_album_id \
           THEN local_tracks.membership_source ELSE 'automatic' END, \
         file_path = excluded.file_path, file_size_bytes = excluded.file_size_bytes, \
         file_mtime_ns = excluded.file_mtime_ns, stat_revision = excluded.stat_revision, \
         stat_revision_kind = excluded.stat_revision_kind, tag_revision = excluded.tag_revision, \
         tags_read_at = excluded.tags_read_at, title = excluded.title, \
         title_folded = excluded.title_folded, artist_name = excluded.artist_name, \
         artist_name_folded = excluded.artist_name_folded, album_title = excluded.album_title, \
         album_title_folded = excluded.album_title_folded, \
         album_artist_name = excluded.album_artist_name, \
         album_artist_name_folded = excluded.album_artist_name_folded, \
         tag_album_title = excluded.tag_album_title, \
         tag_album_artist_name = excluded.tag_album_artist_name, \
         disc_number = excluded.disc_number, track_number = excluded.track_number, \
         year = excluded.year, genre = excluded.genre, genre_folded = excluded.genre_folded, \
         release_type = excluded.release_type, title_sort = excluded.title_sort, \
         artist_sort = excluded.artist_sort, album_sort = excluded.album_sort, \
         album_artist_sort = excluded.album_artist_sort, \
         disc_subtitle = excluded.disc_subtitle, is_compilation = excluded.is_compilation, \
         embedded_release_group_mbid = excluded.embedded_release_group_mbid, \
         embedded_release_mbid = excluded.embedded_release_mbid, \
         embedded_recording_mbid = excluded.embedded_recording_mbid, \
         embedded_release_track_mbid = excluded.embedded_release_track_mbid, \
         embedded_artist_mbid = excluded.embedded_artist_mbid, \
         embedded_album_artist_mbid = excluded.embedded_album_artist_mbid, \
         duration_seconds = excluded.duration_seconds, \
         duration_measured = excluded.duration_measured, file_format = excluded.file_format, \
         bit_rate = excluded.bit_rate, sample_rate = excluded.sample_rate, \
         bit_depth = excluded.bit_depth, channels = excluded.channels, \
         replaygain_track_gain = excluded.replaygain_track_gain, \
         replaygain_album_gain = excluded.replaygain_album_gain, \
         replaygain_track_peak = excluded.replaygain_track_peak, \
         replaygain_album_peak = excluded.replaygain_album_peak, \
         desired_policy_revision = excluded.desired_policy_revision, \
         applied_policy_revision = excluded.applied_policy_revision, \
         applied_policy = excluded.applied_policy, \
         title_provenance = excluded.title_provenance, \
         album_title_provenance = excluded.album_title_provenance, \
         album_artist_provenance = excluded.album_artist_provenance, \
         row_revision = local_tracks.row_revision + 1, \
         availability = CASE WHEN local_tracks.manual_excluded = 1 THEN 'excluded' \
           ELSE 'indexed' END, missing_at = NULL, \
         excluded_at = CASE WHEN local_tracks.manual_excluded = 1 \
           THEN COALESCE(local_tracks.excluded_at, excluded.tags_read_at) END";
    pub(super) TRACK_AND_ALBUM_AT_PATH = "SELECT id, local_album_id FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) WRITE_EDITION_TAGS = "INSERT INTO local_track_edition_tags (local_track_id, media, barcode, catalog_number, \
         release_country, total_discs, release_date, total_tracks) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (local_track_id) DO UPDATE SET media = excluded.media, \
         barcode = excluded.barcode, catalog_number = excluded.catalog_number, \
         release_country = excluded.release_country, total_discs = excluded.total_discs, \
         release_date = excluded.release_date, total_tracks = excluded.total_tracks";
    pub(super) DROP_TRACK_CREDITS = "DELETE FROM local_track_artists WHERE local_track_id = ?1";
    pub(super) WRITE_TRACK_CREDIT = "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role, \
         credited_name, join_phrase, artist_mbid) VALUES (?1, ?2, ?3, 'main', ?4, ?5, ?6)";
    pub(super) ALBUM_OF_TRACK = "SELECT local_album_id FROM local_tracks WHERE id = ?1";
    pub(super) DROP_LOCAL_GENRES = "DELETE FROM local_track_genres WHERE local_track_id = ?1 AND source = ?2";
    pub(super) WRITE_LOCAL_GENRE = "INSERT OR IGNORE INTO local_track_genres (local_track_id, position, name, folded_name, \
         source, source_document_revision) VALUES (?1, ?2, ?3, ?4, ?6, ?5)";
    pub(super) RECORD_ALBUM_MOVE = "INSERT INTO library_scan_album_moves (run_id, root_id, relative_path, \
             local_track_id, previous_album_id) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (run_id, root_id, relative_path) DO UPDATE SET \
             local_track_id = excluded.local_track_id, \
             previous_album_id = excluded.previous_album_id";
    pub(super) ALBUMS_TRACKS_MOVED_TO = "SELECT DISTINCT t.local_album_id FROM library_scan_album_moves m \
             JOIN local_tracks t ON t.id = m.local_track_id \
             WHERE m.run_id = ?1 AND m.previous_album_id = ?2";
    pub(super) REGROUPED_COUNTS = "SELECT (SELECT COUNT(DISTINCT m.local_track_id) FROM library_scan_album_moves m \
               JOIN local_tracks t ON t.id = m.local_track_id \
               WHERE m.run_id = ?1 AND m.previous_album_id = ?2 AND t.local_album_id = ?3) AS moved, \
             (SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?2) AS stayed";
    pub(super) COPY_ALBUM_GROUPING = "UPDATE local_albums SET (root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, tag_album_title, \
             tag_album_artist_name, album_artist_id, album_artist_sort_name, year, \
             original_release_date, primary_genre, is_compilation, updated_at) = \
             (SELECT root_id, grouping_key, title, title_folded, album_artist_name, \
             album_artist_name_folded, tag_album_title, tag_album_artist_name, \
             album_artist_id, album_artist_sort_name, year, original_release_date, \
             primary_genre, is_compilation, updated_at FROM local_albums WHERE id = ?2), \
             row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) MOVE_ALBUM_TRACKS = "UPDATE local_tracks SET local_album_id = ?1 WHERE local_album_id = ?2";
    pub(super) DROP_ALBUM_CREDITS = "DELETE FROM local_album_artists WHERE local_album_id = ?1";
    pub(super) MOVE_ALBUM_CREDITS = "UPDATE local_album_artists SET local_album_id = ?1 WHERE local_album_id = ?2";
    pub(super) DROP_ALBUM_ARTWORK = "DELETE FROM local_album_artwork WHERE local_album_id = ?1";
    pub(super) DROP_ALBUM_JOBS = "DELETE FROM library_identify_jobs WHERE local_album_id = ?1";
    pub(super) MOVE_ALBUM_PLAYS = "UPDATE library_play_history SET local_album_id = COALESCE( \
             (SELECT t.local_album_id FROM local_tracks t \
              WHERE t.id = library_play_history.local_track_id), ?1) \
             WHERE local_album_id = ?2";
    pub(super) MOVE_ALBUM_PLAYLIST_TRACKS = "UPDATE catalog_playlist_tracks SET album_id = ?1 WHERE album_id = ?2";
    pub(super) DROP_ALBUM = "DELETE FROM local_albums WHERE id = ?1";
    pub(super) STAMP_IF_RETAGGED_BY_HAND = "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)";
    pub(super) HOLDS_ONLY_ITS_RETAG = "WITH run AS (SELECT started_at FROM library_scan_runs WHERE id = ?1), \
             other AS (SELECT t.imported_at, t.root_id, t.relative_path FROM local_tracks t \
               WHERE t.local_album_id = ?2 AND NOT EXISTS ( \
                 SELECT 1 FROM library_scan_album_moves m WHERE m.run_id = ?1 \
                 AND m.local_track_id = t.id AND m.previous_album_id = ?3)) \
             SELECT EXISTS(SELECT 1 FROM other) AS foreign_rows, \
             EXISTS(SELECT 1 FROM local_albums a, run WHERE a.id = ?2 \
               AND a.created_at < run.started_at) \
             OR EXISTS(SELECT 1 FROM other, run WHERE other.imported_at < run.started_at \
               OR NOT EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ?1 \
                 AND i.root_id = other.root_id AND i.relative_path = other.relative_path)) AS older";
    pub(super) ALBUM_STILL_HELD = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1) \
             OR EXISTS(SELECT 1 FROM catalog_user_favorites \
               WHERE item_kind = 'album' AND item_id = ?1) \
             OR EXISTS(SELECT 1 FROM catalog_user_ratings \
               WHERE item_kind = 'album' AND item_id = ?1) \
             OR EXISTS(SELECT 1 FROM catalog_playlist_tracks WHERE album_id = ?1)";
    pub(super) ALBUMS_LEFT = "SELECT DISTINCT m.previous_album_id FROM library_scan_album_moves m \
         JOIN local_albums a ON a.id = m.previous_album_id \
         WHERE m.run_id = ?1 AND a.retired_into_album_id IS NULL ORDER BY 1";
    pub(super) RECENTLY_MISSING_TRACKS = "SELECT t.id FROM local_tracks t WHERE t.availability = 'missing' \
             AND t.missing_at >= (SELECT MIN(started_at) FROM library_scan_runs) \
             AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = t.id) \
             AND substr(t.relative_path, 1, length(?1)) <> ?1 \
             ORDER BY t.id";
    pub(super) RETAG_PAIR_IN_STATE = "SELECT EXISTS(SELECT 1 FROM library_retag_pairs \
                 WHERE old_album_id = ?1 AND new_album_id = ?2 AND state = ?3)";
    pub(super) COPY_TRACK_CREDITS = "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, \
                 role, credited_name, join_phrase, artist_mbid) \
                 SELECT ?1, position, local_artist_id, role, credited_name, join_phrase, \
                 artist_mbid FROM local_track_artists WHERE local_track_id = ?2";
    pub(super) DEPARTED_FROM = "WITH run AS (SELECT COALESCE(started_at, ?3) AS start_ms \
           FROM library_scan_runs WHERE id = ?1), \
         moved_now AS (SELECT m.local_track_id, m.previous_album_id \
           FROM library_scan_album_moves m JOIN local_tracks t ON t.id = m.local_track_id \
           WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id), \
         left_it AS (SELECT local_track_id FROM library_album_departures \
           WHERE previous_album_id = ?2 \
           UNION SELECT local_track_id FROM library_scan_album_moves \
           WHERE run_id = ?1 AND previous_album_id = ?2) \
         SELECT t.id, t.local_album_id, \
         CASE WHEN EXISTS (SELECT 1 FROM moved_now WHERE moved_now.local_track_id = t.id) \
           THEN (SELECT start_ms FROM run) \
           ELSE COALESCE((SELECT MAX(d.arrived_at) FROM library_album_departures d \
             WHERE d.local_track_id = t.id), (SELECT start_ms FROM run)) END AS arrived_ms, \
         EXISTS (SELECT 1 FROM moved_now WHERE moved_now.local_track_id = t.id \
           AND moved_now.previous_album_id = ?2) AS this_run \
         FROM left_it JOIN local_tracks t ON t.id = left_it.local_track_id \
         WHERE t.local_album_id <> ?2 ORDER BY t.id";
    pub(super) PIECES = "SELECT EXISTS(SELECT 1 FROM local_tracks o WHERE o.local_album_id = ?3 \
                 AND o.id NOT IN (SELECT local_track_id FROM library_album_departures \
                   WHERE previous_album_id = ?2 \
                   UNION SELECT local_track_id FROM library_scan_album_moves \
                   WHERE run_id = ?1 AND previous_album_id = ?2) \
                 AND (o.imported_at IS NULL OR o.imported_at < ?4))";
    pub(super) HAS_COUNTED_DEPARTURE = "SELECT EXISTS(SELECT 1 FROM library_album_departures WHERE previous_album_id = ?1)";
    pub(super) RETIRED = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
         AND retired_into_album_id IS NOT NULL)";
    pub(super) JOIN_ACROSS_SCANS = "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1";
    pub(super) JOIN_ONE = "SELECT EXISTS(SELECT 1 FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND state = ?2)";
    pub(super) ALBUMS_TRACK_LEFT = "SELECT previous_album_id FROM library_album_departures \
                 WHERE local_track_id = ?2 \
                 UNION SELECT previous_album_id FROM library_scan_album_moves \
                 WHERE run_id = ?1 AND local_track_id = ?2";
    pub(super) ALBUM_IS_EMPTY = "SELECT NOT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) LEFT_AT = "SELECT CASE WHEN EXISTS (SELECT 1 FROM library_scan_album_moves \
           WHERE run_id = ?1 AND local_track_id = ?2 AND previous_album_id = ?3) \
         THEN (SELECT COALESCE(started_at, ?4) \
           FROM library_scan_runs WHERE id = ?1) \
         ELSE (SELECT departed_at FROM library_album_departures \
           WHERE local_track_id = ?2 AND previous_album_id = ?3) END";
    pub(super) CHAINED_RETAG_PAIRS = "SELECT p.old_album_id, p.new_album_id FROM library_retag_pairs p \
             JOIN local_albums old ON old.id = p.old_album_id \
             JOIN local_albums new ON new.id = p.new_album_id \
             WHERE p.state = ?1 AND p.reason = ?2 \
             AND old.retired_into_album_id IS NULL AND new.retired_into_album_id IS NULL \
             ORDER BY 1, 2";
    pub(super) DROP_RETAG_PAIR = "DELETE FROM library_retag_pairs WHERE old_album_id = ?1 AND new_album_id = ?2";
    pub(super) RUN_ALBUM_MOVES = "SELECT DISTINCT m.previous_album_id, t.local_album_id \
             FROM library_scan_album_moves m JOIN local_tracks t ON t.id = m.local_track_id \
             JOIN local_albums old ON old.id = m.previous_album_id \
             JOIN local_albums new ON new.id = t.local_album_id \
             WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id \
             AND old.retired_into_album_id IS NULL AND new.retired_into_album_id IS NULL \
             AND NOT EXISTS (SELECT 1 FROM library_album_departures d \
               JOIN local_tracks earlier ON earlier.id = d.local_track_id \
               WHERE d.previous_album_id = m.previous_album_id \
               AND earlier.local_album_id = t.local_album_id) \
             ORDER BY 1, 2";
    pub(super) CARRY_ONE = "INSERT OR IGNORE INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) SELECT ?2, reason, excluded_by_user_id, \
             excluded_at FROM library_management_exclusions WHERE local_album_id = ?1";
    pub(super) KEEP_DEPARTURES_OF = "INSERT OR IGNORE INTO library_album_departures (local_track_id, \
                 previous_album_id, departed_at, arrived_at) SELECT ?2, ?3, \
                 COALESCE(started_at, ?4), \
                 COALESCE(started_at, ?4) \
                 FROM library_scan_runs WHERE id = ?1";
    pub(super) STAMP_DEPARTURE_ARRIVALS = "UPDATE library_album_departures SET arrived_at = ( \
           SELECT COALESCE(started_at, ?2) FROM library_scan_runs \
           WHERE id = ?1) \
         WHERE local_track_id IN (SELECT m.local_track_id FROM library_scan_album_moves m \
           JOIN local_tracks t ON t.id = m.local_track_id \
           WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id)";
    pub(super) DROP_RETURNED_DEPARTURES = "DELETE FROM library_album_departures WHERE EXISTS (SELECT 1 FROM local_tracks t \
           WHERE t.id = library_album_departures.local_track_id \
           AND t.local_album_id = library_album_departures.previous_album_id) \
         OR EXISTS (SELECT 1 FROM local_albums a \
           WHERE a.id = library_album_departures.previous_album_id \
           AND a.retired_into_album_id IS NOT NULL)";
    pub(super) OFFER_HELD_BACK = "SELECT DISTINCT t.local_album_id FROM library_scan_album_moves m \
             JOIN local_tracks t ON t.id = m.local_track_id \
             JOIN library_scan_inventory i ON i.run_id = m.run_id AND i.root_id = m.root_id \
               AND i.relative_path = m.relative_path \
             WHERE m.run_id = ?1 AND t.local_album_id <> m.previous_album_id \
             AND i.effective_policy = 'automatic'";
    pub(super) MARK_INVENTORY_INDEXED = "UPDATE library_scan_inventory SET processing_state = 'indexed', local_track_id = ?4 \
         WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) MARK_INVENTORY_STATE = "UPDATE library_scan_inventory SET processing_state = ?4 \
         WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) SKIP_PENDING_INVENTORY = "UPDATE library_scan_inventory SET processing_state = 'skipped' \
         WHERE run_id = ?1 AND processing_state = 'pending' \
         AND (root_id, relative_path) > (?2, ?3) AND (root_id, relative_path) <= (?4, ?5)";
    pub(super) TRACK_ALBUM_TAGS = "SELECT local_album_id, root_id, relative_path, embedded_release_mbid, album_title, \
             COALESCE(album_artist_name, ?2) AS album_artist, \
             album_title_provenance = 'tag' AND album_artist_provenance = 'tag' AS from_tags, \
             album_artist_sort, year, genre, is_compilation FROM local_tracks WHERE id = ?1";
    pub(super) RETIRED_ALBUM_WITH_KEY = "SELECT id FROM local_albums WHERE grouping_key = ?1 AND retired_into_album_id = ?2 \
             ORDER BY created_at, id LIMIT 1";
    pub(super) REVIVE_ALBUM = "UPDATE local_albums SET retired_into_album_id = NULL, grouping_source = 'automatic', \
             grouping_locked = 0, updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) DROP_MERGED_ALBUM_ALIAS = "DELETE FROM local_album_aliases WHERE alias = ?1 AND kind = 'merged_album'";
    pub(super) ADD_AUTOMATIC_ALBUM = "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, album_artist_sort_name, \
         year, primary_genre, is_compilation, grouping_source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'automatic', ?13, ?13)";
    pub(super) ADD_AUTOMATIC_ALBUM_CREDIT = "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase) VALUES (?1, 0, ?2, 'main', ?3, '')";
    pub(super) ALBUM_ARTIST_ROW = "SELECT c.local_artist_id, c.artist_mbid, c.credited_name \
             FROM local_track_artists c JOIN local_artists a ON a.id = c.local_artist_id \
             WHERE c.local_track_id = ?1 AND c.artist_mbid IS NOT NULL \
             AND a.retired_into_artist_id IS NULL ORDER BY c.position";
    pub(super) TRACK_ID_AT_PATH = "SELECT id FROM local_tracks WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) FOLDER_DIGESTS = "SELECT relative_path, digest, audio_files FROM library_folder_digests \
                 WHERE root_id = ?1";
    pub(super) WRITE_FOLDER_DIGEST = "INSERT INTO library_folder_digests \
                             (root_id, relative_path, digest, audio_files) VALUES (?1, ?2, ?3, ?4) \
                             ON CONFLICT (root_id, relative_path) DO UPDATE SET \
                             digest = excluded.digest, audio_files = excluded.audio_files";
    pub(super) DROP_FOLDER_DIGEST = "DELETE FROM library_folder_digests \
                             WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) PASS = "SELECT started_at FROM library_scan_runs WHERE id = ?1";
    pub(super) HELD_UNPAIRED = "SELECT k.local_album_id FROM library_album_takeovers k \
         WHERE k.cause = ?1 AND k.handed_back_at IS NULL \
         AND NOT EXISTS (SELECT 1 FROM library_retag_pairs p \
           WHERE p.new_album_id = k.local_album_id)";
    pub(super) HANDS_OFF_ALBUM_STATE = "SELECT b.created_at, b.retired_into_album_id IS NULL AS live, \
             EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
               AND (t.ingest_source <> 'scan' OR t.imported_at < b.created_at)) AS foreign_rows, \
             EXISTS (SELECT 1 FROM library_management_exclusions e \
               WHERE e.local_album_id = b.id) AS is_excluded, \
             EXISTS (SELECT 1 FROM library_album_takeovers k WHERE k.local_album_id = b.id \
               AND k.cause = ?2 AND k.handed_back_at IS NULL) AS is_held \
             FROM local_albums b WHERE b.id = ?1";
    pub(super) ALBUM_TRACK_PLACES = "SELECT id, root_id, relative_path, duration_seconds FROM local_tracks \
             WHERE local_album_id = ?1 ORDER BY id";
    pub(super) MISSING_TRACK_LENGTHS = "SELECT duration_seconds FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'missing'";
    pub(super) ALBUM_HAS_INDEXED_TRACK = "SELECT EXISTS (SELECT 1 FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'indexed')";
    pub(super) SETTLE = "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
         SELECT DISTINCT f.root_id, f.relative_path, \
           COALESCE(r.started_at, ?2) \
         FROM library_scan_failures f JOIN library_scan_runs r ON r.id = f.run_id \
         WHERE f.run_id = ?1 AND f.failure_code = 'MASS_MISSING_GUARD' \
         ON CONFLICT (root_id, relative_path) DO NOTHING";
    pub(super) ALBUM_IS_TAKEN_OVER = "SELECT EXISTS (SELECT 1 FROM library_album_takeovers \
             WHERE local_album_id = ?1 AND handed_back_at IS NULL)";
    pub(super) ALBUM_IS_EXCLUDED = "SELECT EXISTS (SELECT 1 FROM library_management_exclusions \
             WHERE local_album_id = ?1)";
    pub(super) REMEMBER = "INSERT INTO library_scan_unjudged_tracks (local_track_id, held_at) VALUES (?1, ?2) \
         ON CONFLICT (local_track_id) DO UPDATE SET held_at = min(held_at, excluded.held_at)";
    pub(super) REMEMBERED = "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_unjudged_tracks u ON u.local_track_id = t.id \
         WHERE t.local_album_id = ?1)";
    pub(super) JUDGE_REMEMBERED = "DELETE FROM library_scan_unjudged_tracks WHERE local_track_id IN \
         (SELECT id FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) EDITION_RELEASE = "SELECT lower(trim(release_mbid)) FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = 'musicbrainz' \
         AND trim(COALESCE(release_mbid, '')) <> ''";
    pub(super) SCOPE_DISCOVERY_STATE = "SELECT discovery_state FROM library_scan_run_scopes \
                     WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) SCOPE_DISCOVERY_GENERATION = "SELECT discovery_generation FROM library_scan_run_scopes \
                     WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) COMPLETE_SCOPE_DISCOVERY = "UPDATE library_scan_run_scopes SET discovery_state = ?1, error_code = ?2 \
                 WHERE run_id = ?3 AND root_id = ?4 AND relative_path = ?5";
    pub(super) BEGIN_SCOPE_WALK = "UPDATE library_scan_run_scopes SET walk_revision = COALESCE(walk_revision, \
                 (SELECT revision FROM library_scan_fences WHERE singleton = 1)) \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) RESTART_SCOPE_DISCOVERY = "UPDATE library_scan_run_scopes SET discovery_state = 'pending', error_code = NULL, \
                 discovery_generation = discovery_generation + 1, walk_revision = NULL \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) RESET_SCOPES_FOR_RESUME = "UPDATE library_scan_run_scopes SET discovery_state = 'pending', \
                 error_code = NULL, discovery_generation = discovery_generation + 1, \
                 walk_revision = NULL \
                 WHERE run_id = ?1 AND discovery_state != 'completed'";
    pub(super) RECOUNT_DISCOVERED = "UPDATE library_scan_runs SET discovered_count = ( \
                 SELECT COUNT(*) FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ), row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) FINALIZE_DISCOVERY = "UPDATE library_scan_runs SET total_count = ( \
                 SELECT COUNT(*) FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ), discovered_count = ( \
                 SELECT COUNT(*) FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ), updated_at = ?2, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 WHERE id = ?1";
    pub(super) INVENTORY_FOR_RUN = "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
                 i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
                 i.policy_revision, COALESCE(i.local_track_id, t.id) AS local_track_id, \
                 i.scope_relative_path \
                 FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 LEFT JOIN local_tracks t ON t.root_id = i.root_id \
                 AND t.relative_path = i.relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ORDER BY i.rowid DESC LIMIT ?2";
    pub(super) INVENTORY_PAGE = "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
                 i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
                 i.policy_revision, i.local_track_id, i.scope_relative_path \
                 FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 AND i.processing_state = 'pending' \
                 AND (i.root_id, i.relative_path) > (?2, ?3) \
                 ORDER BY i.root_id, i.relative_path LIMIT ?4";
    pub(super) WRITE_INVENTORY_ROW = "INSERT INTO library_scan_inventory (run_id, root_id, relative_path, \
         scope_relative_path, discovery_generation, absolute_path, file_size_bytes, \
         file_mtime_ns, stat_revision, policy_revision, effective_policy, \
         comparison_result, local_track_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT (run_id, root_id, relative_path) DO UPDATE SET \
         scope_relative_path = excluded.scope_relative_path, \
         discovery_generation = excluded.discovery_generation, \
         absolute_path = excluded.absolute_path, \
         file_size_bytes = excluded.file_size_bytes, \
         file_mtime_ns = excluded.file_mtime_ns, \
         stat_revision = excluded.stat_revision, \
         policy_revision = excluded.policy_revision, \
         effective_policy = excluded.effective_policy, \
         comparison_result = excluded.comparison_result, \
         local_track_id = excluded.local_track_id, processing_state = 'pending'";
    pub(super) RECORD_INVENTORY_FAILURE = "INSERT OR IGNORE INTO library_scan_failures \
         (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
         recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', ?6)";
    pub(super) ADD_DISCOVERED_COUNT = "UPDATE library_scan_runs SET discovered_count = discovered_count + ?1, \
         updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?3";
    pub(super) RUN_REVISION = "SELECT row_revision FROM library_scan_runs WHERE id = ?1";
    pub(super) DROP_ALBUM_LYRICS = "DELETE FROM local_track_lyrics WHERE local_track_id IN \
                     (SELECT id FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) WRITE_TRACK_LYRICS = "INSERT INTO local_track_lyrics (local_track_id, source) \
                         SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM local_tracks WHERE id = ?1) \
                         ON CONFLICT(local_track_id) DO UPDATE SET source = excluded.source";
    pub(super) WRITE_LYRICS_CHECK = "INSERT INTO local_album_lyrics_checks (local_album_id, signature, checked_at) \
                     SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
                     ON CONFLICT(local_album_id) DO UPDATE SET \
                     signature = excluded.signature, checked_at = excluded.checked_at";
    pub(super) LOAD_SCOPES = "SELECT root_id, scope_id, relative_path, root_path, effective_policy, \
         policy_revision, estimated_count FROM library_scan_run_scopes \
         WHERE run_id = ?1 ORDER BY scope_sequence";
    pub(super) ENSURE_SCAN_STREAM = "INSERT INTO library_event_stream_revisions (stream_kind, value) \
         VALUES ('scan', 0) ON CONFLICT (stream_kind) DO NOTHING";
    pub(super) BUMP_SCAN_STREAM = "UPDATE library_event_stream_revisions SET value = value + 1 \
         WHERE stream_kind = 'scan'";
    pub(super) READ_SCAN_STREAM = "SELECT value FROM library_event_stream_revisions WHERE stream_kind = 'scan'";
    pub(super) RECORD_MOUNT = "INSERT INTO library_scan_mounts (root_id, relative_path) VALUES (?1, ?2) \
                 ON CONFLICT (root_id, relative_path) DO NOTHING";
    pub(super) RECORDED = "SELECT reason, state FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND new_album_id = ?2";
    pub(super) RECORD = "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
             recorded_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT (old_album_id, new_album_id) DO UPDATE SET \
             reason = excluded.reason, updated_at = excluded.updated_at \
             WHERE library_retag_pairs.reason IN (?6, ?7, ?8) \
             AND library_retag_pairs.state = ?4 \
             AND excluded.reason <> library_retag_pairs.reason";
    pub(super) END_DISMISSED_RETURNED = "DELETE FROM library_retag_pairs WHERE state = ?3 AND new_album_id IN \
         (SELECT id FROM local_albums WHERE id = ?2 OR retired_into_album_id = ?2) \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?2 \
           AND t.availability = 'indexed') \
         AND EXISTS (SELECT 1 FROM library_scan_album_moves m \
           JOIN local_tracks t ON t.id = m.local_track_id \
           WHERE m.run_id = ?1 AND m.previous_album_id = ?2 \
           AND t.local_album_id = library_retag_pairs.old_album_id) \
         RETURNING old_album_id, new_album_id";
    pub(super) STREAM_REVISION = "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?1";
    pub(super) QUEUE_CURRENT_INDEX = "SELECT current_index FROM compat_play_queues WHERE user_id = ?1";
    pub(super) CLEAR_QUEUE_ITEMS = "DELETE FROM compat_play_queue_items WHERE user_id = ?1";
    pub(super) ADD_QUEUE_ITEM = "INSERT INTO compat_play_queue_items (user_id, item_index, file_id) \
                 VALUES (?1, ?2, ?3)";
    pub(super) SET_QUEUE_CURRENT_INDEX = "UPDATE compat_play_queues SET current_index = ?2 WHERE user_id = ?1";
    pub(super) COALESCE_RUN_REQUEST = "UPDATE library_scan_runs SET coalesced_request_count = coalesced_request_count + 1, \
             updated_at = ?1, row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = ?2";
    pub(super) LAST_SCOPE_SEQUENCE = "SELECT COALESCE(MAX(scope_sequence), -1) FROM library_scan_run_scopes WHERE run_id = ?1";
    pub(super) DROP_RUN_SCOPE = "DELETE FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) ADD_RUN_SCOPE = "INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, scope_id, \
                 relative_path, root_path, effective_policy, policy_revision, estimated_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    pub(super) SET_RUN_AGGREGATE_SCOPE = "UPDATE library_scan_runs SET aggregate_scope = ?1, updated_at = ?2, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?3";
    pub(super) ADD_RUN = "INSERT INTO library_scan_runs (id, kind, trigger, requested_by_user_id, state, phase, \
         aggregate_scope, queued_at, updated_at, inventory_cleanup_pending, \
         row_revision, event_revision) \
         VALUES (?1, ?2, ?3, ?4, 'queued', 'queued', ?5, ?6, ?6, 1, 1, 0)";
    pub(super) ACTIVE_RUN_COUNT = "SELECT COUNT(*) FROM library_scan_runs \
         WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping')";
    pub(super) NEXT_QUEUED_RUN = "SELECT id FROM library_scan_runs WHERE state = 'queued' ORDER BY rowid LIMIT 1";
    /// SQL that is true when a credit of artist `?1` holds an id other than
    /// `?2`, leaving out the credits of track `?3` and album `?4` (NULL leaves
    /// out none). Each probe is a range on an (artist, id) index and stops at
    /// the first row found.
    pub(super) fragment HOLDS_OTHER_ID = (holds_other_id!("?1"));
    /// Audit kind of the tracks whose paths other music took.
    pub(super) fragment OTHER_MUSIC_KIND = "other_music_at_path";
    /// Albums a person keeps their hands on: left unmanaged, kept as a custom
    /// edition, or stepped back from and not handed back.
    pub(super) fragment HANDS_OFF = "SELECT local_album_id FROM library_management_exclusions \
    UNION SELECT local_album_id FROM library_custom_edition_active \
    UNION SELECT local_album_id FROM library_album_takeovers WHERE handed_back_at IS NULL";
    pub(super) fragment NEW = "SELECT t.id, t.local_album_id, t.relative_path FROM local_tracks t \
         WHERE t.root_id = ?2 AND t.availability = 'indexed' AND t.ingest_source = 'scan' \
         AND t.imported_at >= ?3 \
         AND EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ?1 \
           AND i.root_id = t.root_id AND i.relative_path = t.relative_path)";
    pub(super) fragment RUN_COLUMNS = "id, kind, trigger, requested_by_user_id, state, phase, \
    resume_phase, requested_control, aggregate_scope, total_count, discovered_count, \
    inspected_count, new_count, changed_count, indexed_count, unchanged_count, \
    excluded_count, missing_count, errored_count, identification_enqueued_count, \
    coalesced_request_count, queued_at, started_at, updated_at, terminal_at, \
    terminal_code, phase_timings_json, row_revision, event_revision";
    pub(super) PROVEN_ROWS = "SELECT created_at FROM local_artists WHERE id = ?1 \
                 AND retired_into_artist_id IS NULL AND kind <> 'various_artists' \
                 AND id NOT IN (?5, ?6) AND NOT EXISTS (SELECT 1 FROM \
                 local_artist_external_identities WHERE local_artist_id = ?1) \
                 AND NOT " + HOLDS_OTHER_ID;
    pub(super) SAYS_OTHER = "SELECT EXISTS(SELECT 1 FROM local_artists WHERE id = ?1 \
                 AND (kind = 'various_artists' OR id IN (?5, ?6))) \
                 OR EXISTS(SELECT 1 FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
                 AND lower(provider_artist_id) <> ?2) \
                 OR " + HOLDS_OTHER_ID;
    pub(super) WAITS_FOR_THE_REST = "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?1 \
         AND t.id NOT IN (SELECT value FROM json_each(?3)) \
         AND ((substr(t.relative_path, 1, length(?4) + length(t.id) + 1) = ?4 || t.id || '/' \
           AND EXISTS (SELECT 1 FROM library_catalog_actions a, \
             json_each(a.after_json, '$.pairs') p \
             WHERE a.local_album_id = ?1 AND a.action_kind = '" + OTHER_MUSIC_KIND + "' \
             AND json_extract(p.value, '$.old_track_id') = t.id)) \
         OR (t.availability != 'excluded' AND NOT (t.availability = 'missing' \
           AND (t.missing_at IS NULL OR t.missing_at <= ?2)))))";
    pub(super) LOAD_RUN = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs WHERE id = ?1";
    pub(super) NEW_EVERYWHERE = NEW;
    pub(super) NEW_BELOW = NEW + " AND t.relative_path >= ?4 AND t.relative_path < ?5";
    pub(super) STILL_NAMED = "SELECT EXISTS(SELECT 1 FROM local_track_artists c \
                 JOIN local_tracks t ON t.id = c.local_track_id \
                 WHERE c.local_artist_id = ?2 AND c.credited_name = ?3 \
                 AND c.local_track_id IS NOT ?4 AND t.availability = 'indexed' \
                 AND NOT " + TRACK_GONE + ") \
                 OR EXISTS(SELECT 1 FROM local_album_artists c \
                 JOIN local_albums a ON a.id = c.local_album_id \
                 WHERE c.local_artist_id = ?2 AND c.credited_name = ?3 \
                 AND c.local_album_id <> ?5 AND a.retired_into_album_id IS NULL \
                 AND EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                 AND t.availability = 'indexed' AND t.id IS NOT ?4 AND NOT " + TRACK_GONE + "))";
    pub(super) ALBUM_PRESENCE = "SELECT t.root_id, t.relative_path, " + TRACK_PRESENT + " AS present FROM local_tracks t \
         WHERE t.local_album_id = ?2 AND t.availability = 'indexed'";
    pub(super) FIND_MOVED_BY_RECORDING = "SELECT t.id, t.local_album_id, " + SAME_PLACE + " AS same_place, t.duration_seconds FROM local_tracks t" + " WHERE t.embedded_recording_mbid = ?2 AND " + TRACK_GONE;
    pub(super) FIND_MOVED_BY_TITLE = "SELECT t.id, t.local_album_id, " + SAME_PLACE + " AS same_place, t.duration_seconds FROM local_tracks t" + " WHERE t.title_folded = ?2 AND " + SAME_PLACE + " AND " + TRACK_GONE;
    pub(super) MOVED_PAIRS_OF_GONE_TRACK = "SELECT " + MOVED_COLUMNS + " FROM local_tracks t JOIN local_tracks n \
         ON n.availability = 'indexed' AND n.album_title_folded = t.album_title_folded \
         AND " + MOVED_MATCH + " WHERE t.id = ?1";
    pub(super) MOVED_PAIRS_OF_NEW_TRACK = "SELECT " + MOVED_COLUMNS + " FROM local_tracks n JOIN local_tracks t \
         ON t.availability = 'missing' AND t.album_title_folded = n.album_title_folded \
         AND " + MOVED_MATCH + " WHERE n.id = ?1";
    pub(super) LOST_ROWS = "SELECT t.id, t.local_album_id, t.root_id, t.relative_path, t.duration_seconds \
         FROM (" + HANDS_OFF + ") h \
         CROSS JOIN local_albums a ON a.id = h.local_album_id \
         CROSS JOIN local_tracks t ON t.local_album_id = a.id \
         WHERE a.retired_into_album_id IS NULL AND ((t.availability = 'missing' \
         AND t.missing_at >= ?2) OR (t.availability = 'indexed' AND " + WALKED_PAST + "))";
    pub(super) LIST_HISTORY = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs WHERE terminal_at IS NOT NULL \
                 AND (?2 IS NULL OR terminal_at < ?2 OR (terminal_at = ?2 AND id < ?3)) \
                 ORDER BY terminal_at DESC, id DESC LIMIT ?1";
    pub(super) LATEST_FILESYSTEM_TERMINAL = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs WHERE terminal_at IS NOT NULL \
                 ORDER BY terminal_at DESC, id DESC";
    pub(super) RESUMABLE = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs \
                         WHERE state IN ('discovering','indexing','reconciling','pausing','stopping') \
                         ORDER BY started_at, id LIMIT 1";
    pub(super) RECOVER = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs \
                 WHERE state IN ('queued','discovering','indexing','reconciling','paused') \
                 ORDER BY queued_at, id";
    pub(super) CURRENT_RUNS = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs \
         WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
         ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, queued_at, id";
    pub(super) OPEN_RUN_TO_JOIN = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs \
         WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
         ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, rowid";
    /// Whether run `?1` moved a track onto album `?2` from an album a track still holds.
    pub(super) ALBUM_HELD_BACK_IN_RUN = "SELECT EXISTS(SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_album_moves m ON m.run_id = ?1 AND m.root_id = t.root_id \
           AND m.relative_path = t.relative_path AND m.local_track_id = t.id \
         WHERE t.local_album_id = ?2 AND m.previous_album_id <> ?2 \
         AND EXISTS (SELECT 1 FROM local_tracks left_behind \
           WHERE left_behind.local_album_id = m.previous_album_id AND " + (holds!("left_behind", "?1")) + "))";
    /// Whether a scan that has not ended moved a track onto album `?1` from an album a track still holds.
    pub(super) ALBUM_AWAITS_RETAG = "SELECT EXISTS(SELECT 1 FROM library_scan_runs run \
             CROSS JOIN local_tracks t \
             CROSS JOIN library_scan_album_moves m ON m.run_id = run.id \
               AND m.root_id = t.root_id AND m.relative_path = t.relative_path \
               AND m.local_track_id = t.id \
             WHERE run.terminal_at IS NULL AND t.local_album_id = ?1 \
             AND m.previous_album_id <> ?1 \
             AND EXISTS (SELECT 1 FROM local_tracks left_behind \
               WHERE left_behind.local_album_id = m.previous_album_id AND " + (holds!("left_behind", "run.id")) + "))";
    /// The automatic-policy albums run `?1` moved tracks to from album `?2`, when no track holds it any more.
    pub(super) ALBUMS_RELEASED_BY = "SELECT DISTINCT t.local_album_id FROM library_scan_album_moves m \
         JOIN local_tracks t ON t.id = m.local_track_id \
         JOIN library_scan_inventory i ON i.run_id = m.run_id AND i.root_id = m.root_id \
           AND i.relative_path = m.relative_path \
         WHERE m.run_id = ?1 AND m.previous_album_id = ?2 AND t.local_album_id <> ?2 \
         AND i.effective_policy = 'automatic' \
         AND NOT EXISTS (SELECT 1 FROM local_tracks left_behind \
           WHERE left_behind.local_album_id = ?2 AND " + (holds!("left_behind", "?1")) + ")";
    /// Whether a track row still holds album `?2` in run `?1`.
    pub(super) ALBUM_HELD_IN_RUN = "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?2 AND " + (holds!("t", "?1")) + ")";
    /// Albums a departure on record or a move of run `?1` names, not retired, that no track row holds.
    pub(super) ALBUMS_NO_ROW_HOLDS = "SELECT a.id FROM local_albums a WHERE a.retired_into_album_id IS NULL \
             AND a.id IN (SELECT previous_album_id FROM library_album_departures \
               UNION SELECT previous_album_id FROM library_scan_album_moves WHERE run_id = ?1) \
             AND NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id AND " + (holds!("t", "?1")) + ") \
             ORDER BY a.id";
    /// The (root, path) pairs a scan of run `?1` writes no row for: where a
    /// track lies that a write fencing it names (`fenced_by!`: still open, or
    /// moved after the walk of the scope covering it began), the path a
    /// fencing write moved a file away from, where an open write is placing a
    /// file, and where a fencing move or one that awaits an administrator
    /// (`awaits_admin!`) took a track's file. The last keeps a move that needs
    /// attention from listing its file as a second track: once it is marked
    /// handled, the next scan finds the old path gone and continues the track
    /// at the new one. One read: the open and waiting rows by the state index
    /// (the open states listed are every state `fenced_by!` does not call
    /// terminal), the rows moved since the run's earliest walk by their
    /// revision. The CROSS JOIN keeps the fencing rows outer, so tracks are
    /// read by id: without statistics SQLite 3.54 would walk them instead.
    pub(super) FENCED_PATHS = "WITH fencing AS ( \
       SELECT id, state, fence_revision, track_id, source_root, source_rel, dest_root, dest_rel \
         FROM library_publish_journal \
         WHERE state IN ('prepared', 'staged', 'published', 'committed', 'cleanup_pending') \
       UNION \
       SELECT id, state, fence_revision, track_id, source_root, source_rel, dest_root, dest_rel \
         FROM library_publish_journal WHERE fence_revision > \
         (SELECT MIN(walk_revision) FROM library_scan_run_scopes WHERE run_id = ?1)), \
     named AS ( \
       SELECT state, fence_revision, track_id FROM fencing WHERE track_id IS NOT NULL \
       UNION ALL \
       SELECT f.state, f.fence_revision, r.replaced_track_id FROM fencing f \
         JOIN library_publish_replacements r ON r.journal_id = f.id \
         WHERE r.replaced_track_id IS NOT NULL) \
     SELECT t.root_id, t.relative_path FROM named j CROSS JOIN local_tracks t ON t.id = j.track_id \
      WHERE EXISTS (SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = ?1 \
        AND s.root_id = t.root_id \
        AND (s.relative_path = '.' OR t.relative_path = s.relative_path \
          OR substr(t.relative_path, 1, length(s.relative_path) + 1) = s.relative_path || '/') \
        AND " + (fenced_by!()) + ") \
     UNION \
     SELECT source_root, source_rel FROM fencing WHERE source_root IS NOT NULL \
     UNION \
     SELECT dest_root, dest_rel FROM fencing \
      WHERE state IN ('prepared', 'staged', 'published', 'committed', 'cleanup_pending') \
        OR (track_id IS NOT NULL AND source_root IS NOT NULL \
          AND (source_root IS NOT dest_root OR source_rel IS NOT dest_rel)) \
     UNION \
     SELECT j.dest_root, j.dest_rel FROM library_publish_journal j \
      WHERE j.track_id IS NOT NULL AND j.source_root IS NOT NULL \
        AND (j.source_root IS NOT j.dest_root OR j.source_rel IS NOT j.dest_rel) AND " + (awaits_admin!());
    /// Lift the holds run `?1` settled (`?2` is now).
    pub(super) LIFT_SETTLED_HOLDS = "DELETE FROM library_scan_held_scopes AS h WHERE " + HOLD_LIFTS;
    /// The held rows of live albums the holds run `?1` lifts let go, with each one's earliest hold.
    pub(super) ROWS_LET_GO = "SELECT n.local_album_id, n.id, MIN(h.held_at) AS first_held_at \
             FROM library_scan_held_scopes h \
             CROSS JOIN local_tracks n \
             CROSS JOIN local_albums b ON b.id = n.local_album_id \
             WHERE " + HOLD_LIFTS + " AND " + (held!("n", "h")) + " AND b.retired_into_album_id IS NULL \
             GROUP BY n.id";
    /// Whether a scanned file of album `?1` sits in a held folder, first indexed after the hold began.
    pub(super) ALBUM_HOLDS_NEW_FILES = "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_held_scopes h \
         WHERE t.local_album_id = ?1 AND " + (held!("t", "h")) + ")";
    /// Mounts under scope `?2` of root `?1` with no file under them in run `?3`'s inventory, with how many indexed tracks the run walked past.
    pub(super) EMPTY_MOUNTS = "SELECT relative_path, indexed FROM (SELECT m.relative_path, \
               (SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = m.root_id \
                AND t.availability = 'indexed' \
                AND t.relative_path > m.relative_path || '/' \
                AND t.relative_path < m.relative_path || '0' AND " + (walked_past!("?3")) + ") AS indexed \
             FROM library_scan_mounts m \
             WHERE m.root_id = ?1 AND " + (covers!("?2", "m.relative_path")) + " \
             AND NOT EXISTS (SELECT 1 FROM library_scan_inventory i \
               JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
               AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
               WHERE i.run_id = ?3 AND i.root_id = ?1 \
               AND i.relative_path > m.relative_path || '/' \
               AND i.relative_path < m.relative_path || '0' \
               AND s.discovery_generation = i.discovery_generation)) \
             WHERE indexed > 0 ORDER BY relative_path";
    /// The live rows proven to be the MusicBrainz artist `?2`, the survivor first. `?5` and `?6` are the sentinel ids.
    pub(super) ROWS_PROVEN_AS_MBID = "SELECT a.id, i.decision_source IN " + PERSONS + " AS chosen \
             FROM local_artist_external_identities i \
             JOIN local_artists a ON a.id = i.local_artist_id \
             WHERE lower(i.provider_artist_id) = ?2 AND i.provider = 'musicbrainz' \
             AND a.retired_into_artist_id IS NULL AND a.kind <> 'various_artists' \
             AND a.id NOT IN (?5, ?6) \
             AND (i.decision_source IN " + PERSONS + " OR NOT " + (holds_other_id!("a.id")) + ") \
             ORDER BY chosen DESC, a.created_at, a.id";
    /// Indexed rows under one scope, outside a folder run `?3` refused as a link.
    pub(super) INDEXED_COUNT_IN_SCOPE = "SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = ?1 \
                         AND +t.availability = 'indexed' AND " + IN_SCOPE_T + " AND NOT " + (refused_scope!("?3", "t"));
    /// Indexed rows run `?3` walked past under one scope, and the rows the scope holds once its new files index.
    pub(super) VANISH_COUNTS = "SELECT \
                         (SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = ?1 \
                          AND +t.availability = 'indexed' AND " + IN_SCOPE_T + " AND " + (walked_past!("?3")) + ") AS walked_past_count, \
                         (SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = ?1 \
                          AND +t.availability = 'indexed' AND " + IN_SCOPE_T + " AND NOT " + (refused_scope!("?3", "t")) + ") \
                         + (SELECT COUNT(*) FROM library_scan_inventory i WHERE i.run_id = ?3 \
                          AND i.root_id = ?1 AND i.scope_relative_path = ?2 \
                          AND i.comparison_result = 'new') AS total";
    /// Rows walked past under the scope whose file the run's inventory holds at the same place below a twin in `?4`.
    pub(super) ROWS_FOUND_AT_TWINS = "SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = ?1 \
             AND +t.availability = 'indexed' AND " + IN_SCOPE_T + " AND " + (walked_past!("?3")) + " \
             AND EXISTS (SELECT 1 FROM json_each(?4) w CROSS JOIN library_scan_inventory i \
               ON i.run_id = ?3 AND i.root_id = ?1 \
               AND i.relative_path = w.value || substr(t.relative_path, length(?2) + 1))";
    /// Indexed paths run `?3` walked past under one scope, and whether a folder covering each was held back.
    pub(super) MISSING_CATALOG_PATHS = "SELECT t.relative_path, " + (held_back!("?3", "t")) + " AS held_back FROM local_tracks t \
             WHERE t.root_id = ?1 AND t.availability = 'indexed' AND " + IN_SCOPE + " \
             AND " + (walked_past!("?3")) + " ORDER BY t.relative_path";
    /// Catalog tracks under any of a list of scopes, each counted once. Each scope binds its root and path.
    family SCOPES_TRACK_COUNT = scopes_track_count;
    /// Saved compat bookmarks whose track row is gone.
    pub(super) DROP_ORPHAN_BOOKMARKS = "DELETE FROM compat_bookmarks WHERE " + (track_gone!("compat_bookmarks"));
    pub(super) USERS_WITH_ORPHAN_QUEUE_ITEMS = "SELECT DISTINCT user_id FROM compat_play_queue_items WHERE " + (track_gone!("compat_play_queue_items"));
    /// User `?1`'s queue in order, and whether each entry's track row is gone.
    pub(super) QUEUE_ITEMS_OF_USER = "SELECT item_index, file_id, " + (track_gone!("compat_play_queue_items")) + " AS gone FROM compat_play_queue_items \
                 WHERE user_id = ?1 ORDER BY item_index";
    pub(super) RUNS_PAUSING_OR_STOPPING = "SELECT id FROM library_scan_runs WHERE state IN ('pausing','stopping') ORDER BY queued_at, id";
    pub(super) RUNS_STOPPING = "SELECT id FROM library_scan_runs WHERE state = 'stopping' ORDER BY queued_at, id";
    /// Add `?1` to one counter of run `?2`.
    family ADD_TO_COUNTER = add_to_counter;
    /// The `scope_covers_path` rule for a catalog path column `relative_path` and a scope bound as `?2`: "." covers everything, else the path is the scope or extends it past a slash. Byte comparisons only: paths are case-sensitive, so no `LIKE`, and every path under "scope/" sorts between "scope/" and "scope0" ('0' follows '/').
    pub(super) fragment IN_SCOPE = "(?2 = '.' OR relative_path = ?2 \
    OR (relative_path > ?2 || '/' AND relative_path < ?2 || '0'))";
    /// [`IN_SCOPE`] for a `local_tracks t` alias.
    pub(super) fragment IN_SCOPE_T = "(?2 = '.' OR t.relative_path = ?2 \
    OR (t.relative_path > ?2 || '/' AND t.relative_path < ?2 || '0'))";
    /// Mark one listed path (`?2` under root `?1`) missing at `?3`, only while run `?4` still walked past it.
    pub(super) MARK_WALKED_PAST_MISSING = "UPDATE local_tracks AS t SET availability = 'missing', missing_at = ?3 \
     WHERE t.root_id = ?1 AND t.relative_path = ?2 AND t.availability = 'indexed' AND " + (walked_past!("?4"));
    /// Whether run `?1` lifts the hold aliased `h`: the run completed and walked a folder covering it to the end, held back or refused no folder covering it or inside it, and no file under it went missing in the run from an album that holds no indexed file and is not merged into another. `?2` is now, for a run with no start.
    pub(super) fragment HOLD_LIFTS = "EXISTS (SELECT 1 FROM library_scan_runs r \
       JOIN library_scan_run_scopes s ON s.run_id = r.id \
       WHERE r.id = ?1 AND r.state = 'completed' AND s.root_id = h.root_id \
       AND s.discovery_state = 'completed' AND " + (covers!("s.relative_path", "h.relative_path")) + ") \
     AND NOT " + (held_back!("?1", "h")) + " AND NOT " + (refused_scope!("?1", "h")) + " AND NOT EXISTS (SELECT 1 FROM library_scan_failures g WHERE g.run_id = ?1 \
       AND g.root_id = h.root_id \
       AND g.failure_code IN ('MASS_MISSING_GUARD', 'SCOPE_THROUGH_LINK') \
       AND " + (covers!("h.relative_path", "g.relative_path")) + ") \
     AND NOT EXISTS (SELECT 1 FROM local_tracks t \
       JOIN local_albums a ON a.id = t.local_album_id \
       WHERE t.availability = 'missing' AND t.root_id = h.root_id \
       AND t.missing_at >= (SELECT COALESCE(started_at, ?2) \
         FROM library_scan_runs WHERE id = ?1) \
       AND " + (covers!("h.relative_path", "t.relative_path")) + " AND a.retired_into_album_id IS NULL \
       AND NOT EXISTS (SELECT 1 FROM local_tracks k \
         WHERE k.local_album_id = a.id AND k.availability = 'indexed'))";
     fragment WALKED_PAST_INDEXED = " OR (t.availability = 'indexed' AND " + WALKED_PAST + ")";
    family REPLACEMENT_ROWS = replacements_sql;

    pub(super) row OwedEndPass for [OWED_END_PASSES] {
        id: String,
        next_end_pass: String,
        terminal_at: Millis,
        end_pass_waits_used: i64,
        end_pass_retry_at: Option<Millis>,
    }
    pub(super) row RunRow for [
        RUNS_OWING_END_PASSES,
        LOAD_RUN,
        LIST_HISTORY,
        LATEST_FILESYSTEM_TERMINAL,
        RESUMABLE,
        RECOVER,
        CURRENT_RUNS,
        OPEN_RUN_TO_JOIN,
    ] {
        id: String,
        kind: ScanKind,
        trigger: ScanTrigger,
        requested_by_user_id: Option<String>,
        state: String,
        phase: String,
        resume_phase: Option<ScanPhase>,
        requested_control: RequestedControl,
        aggregate_scope: String,
        total_count: i64,
        discovered_count: i64,
        inspected_count: i64,
        new_count: i64,
        changed_count: i64,
        indexed_count: i64,
        unchanged_count: i64,
        excluded_count: i64,
        missing_count: i64,
        errored_count: i64,
        identification_enqueued_count: i64,
        coalesced_request_count: i64,
        queued_at: Millis,
        started_at: Option<Millis>,
        updated_at: Millis,
        terminal_at: Option<Millis>,
        terminal_code: Option<FailureCode>,
        phase_timings_json: String,
        row_revision: i64,
        event_revision: i64,
    }
    pub(super) row ScopeRow for [LOAD_SCOPES] {
        root_id: String,
        scope_id: Option<String>,
        relative_path: String,
        root_path: Option<String>,
        effective_policy: EffectivePolicy,
        policy_revision: String,
        estimated_count: Option<i64>,
    }
    pub(super) row InventoryRow for [INVENTORY_FOR_RUN, INVENTORY_PAGE] {
        root_id: String,
        relative_path: String,
        absolute_path: String,
        file_size_bytes: i64,
        file_mtime_ns: i64,
        stat_revision: String,
        effective_policy: EffectivePolicy,
        comparison_result: Verdict,
        policy_revision: String,
        local_track_id: Option<String>,
        scope_relative_path: String,
    }
    pub(super) row FailureRow for [RUN_FAILURES] {
        root_id: String,
        relative_path: String,
        failure_code: FailureCode,
        recorded_at: Millis,
        failure_detail: String,
        phase: Phase,
    }
    pub(super) row FailurePageRow for [RUN_FAILURES_PAGE] {
        rowid: i64,
        root_id: String,
        relative_path: String,
        failure_code: FailureCode,
        recorded_at: Millis,
        failure_detail: String,
        phase: Phase,
    }
    pub(super) row ArtistNames for [ARTIST_NAMES] {
        display_name: String,
        sort_name: Option<String>,
    }
    pub(super) row AlbumTrackFile for [ALBUM_TRACKS] {
        local_album_id: String,
        id: String,
        file_path: String,
        relative_path: String,
        stat_revision: String,
        file_size_bytes: i64,
    }
    pub(super) row ArtworkRow for [ALL_ARTWORK] {
        local_album_id: String,
        source: ArtworkSource,
        source_locator: Option<String>,
        content_hash: Option<String>,
    }
    pub(super) row AlbumCheck for [ALL_ARTWORK_CHECKS, ALL_LYRICS_CHECKS] {
        local_album_id: String,
        signature: String,
    }
    pub(super) row CatalogRow for [LOAD_CATALOG_MAP] {
        relative_path: String,
        id: String,
        stat_revision: String,
        stat_revision_kind: RevisionKind,
        file_size_bytes: i64,
        file_mtime_ns: i64,
        tags_read_at: Option<Millis>,
    }
    pub(super) row GroupingCandidate for [ALBUMS_FOR_GROUPING] {
        id: String,
        exact: bool,
    }
    pub(super) row CreditRead for [ALBUM_CREDIT_ROWS, TRACK_CREDIT_ROWS] {
        position: i64,
        local_artist_id: String,
        credited_name: Option<String>,
        artist_mbid: Option<String>,
    }
    pub(super) row TrackFacts for [TRACK_AT_PATH_FACTS] {
        id: String,
        local_album_id: String,
        availability: Availability,
        duration_seconds: Option<f64>,
        embedded_recording_mbid: Option<String>,
        title_folded: String,
        title_provenance: TagProvenance,
        disc_number: i64,
        track_number: i64,
        sample_rate: Option<i64>,
        bit_depth: Option<i64>,
        channels: Option<i64>,
    }
    pub(super) row TrackMembership for [TRACK_MEMBERSHIP] {
        membership_source: GroupingSource,
        locked: bool,
    }
    pub(super) row TrackAndAlbum for [TRACK_AND_ALBUM_AT_PATH] {
        id: String,
        local_album_id: String,
    }
    pub(super) row RegroupedCounts for [REGROUPED_COUNTS] {
        moved: i64,
        stayed: i64,
    }
    pub(super) row RetagHolds for [HOLDS_ONLY_ITS_RETAG] {
        foreign_rows: bool,
        older: bool,
    }
    pub(super) row DepartedRow for [DEPARTED_FROM] {
        id: String,
        local_album_id: String,
        arrived_ms: i64,
        this_run: bool,
    }
    pub(super) row RetagAlbums for [CHAINED_RETAG_PAIRS, END_DISMISSED_RETURNED] {
        old_album_id: String,
        new_album_id: String,
    }
    pub(super) row AlbumMove for [RUN_ALBUM_MOVES] {
        previous_album_id: String,
        local_album_id: String,
    }
    pub(super) row TrackAlbumTags for [TRACK_ALBUM_TAGS] {
        local_album_id: String,
        root_id: String,
        relative_path: String,
        embedded_release_mbid: Option<String>,
        album_title: String,
        album_artist: String,
        from_tags: bool,
        album_artist_sort: Option<String>,
        year: Option<i64>,
        genre: Option<String>,
        is_compilation: i64,
    }
    pub(super) row AlbumArtistRow for [ALBUM_ARTIST_ROW] {
        local_artist_id: String,
        artist_mbid: String,
        credited_name: String,
    }
    pub(super) row FolderDigestRow for [FOLDER_DIGESTS] {
        relative_path: String,
        digest: i64,
        audio_files: i64,
    }
    pub(super) row HandsOffState for [HANDS_OFF_ALBUM_STATE] {
        created_at: i64,
        live: bool,
        foreign_rows: bool,
        is_excluded: bool,
        is_held: bool,
    }
    pub(super) row TrackPlaceRow for [ALBUM_TRACK_PLACES] {
        id: String,
        root_id: String,
        relative_path: String,
        duration_seconds: Option<f64>,
    }
    pub(super) row RetagPairRow for [RECORDED] {
        reason: String,
        state: String,
    }
    pub(super) row AlbumPresenceRow for [ALBUM_PRESENCE] {
        root_id: String,
        relative_path: String,
        present: bool,
    }
    pub(super) row MovedCandidate for [FIND_MOVED_BY_RECORDING, FIND_MOVED_BY_TITLE] {
        id: String,
        local_album_id: String,
        same_place: bool,
        duration_seconds: Option<f64>,
    }
    pub(super) row MovedPair for [MOVED_PAIRS_OF_GONE_TRACK, MOVED_PAIRS_OF_NEW_TRACK] {
        old_id: String,
        old_album_id: String,
        new_id: String,
        new_album_id: String,
        recording: bool,
        old_seconds: Option<f64>,
        new_seconds: Option<f64>,
    }
    pub(super) row LostRow for [LOST_ROWS] {
        id: String,
        local_album_id: String,
        root_id: String,
        relative_path: String,
        duration_seconds: Option<f64>,
    }
    pub(super) row NewTrackRow for [NEW_EVERYWHERE, NEW_BELOW] {
        id: String,
        local_album_id: String,
        relative_path: String,
    }
    pub(super) row FencedPath for [FENCED_PATHS] {
        root_id: String,
        relative_path: String,
    }
    pub(super) row LetGoRow for [ROWS_LET_GO] {
        local_album_id: String,
        id: String,
        first_held_at: i64,
    }
    pub(super) row EmptyMount for [EMPTY_MOUNTS] {
        relative_path: String,
        indexed: i64,
    }
    pub(super) row ProvenRow for [ROWS_PROVEN_AS_MBID] {
        id: String,
        chosen: bool,
    }
    pub(super) row VanishCounts for [VANISH_COUNTS] {
        walked_past_count: i64,
        total: i64,
    }
    pub(super) row MissingCatalogPath for [MISSING_CATALOG_PATHS] {
        relative_path: String,
        held_back: bool,
    }
    pub(super) row QueueItem for [QUEUE_ITEMS_OF_USER] {
        item_index: i64,
        file_id: String,
        gone: bool,
    }
    pub(super) row ReplacementRow for family [replacements_sql] {
        new_album_id: String,
        old_album_id: String,
        track_id: String,
    }
}

pub(super) fn scopes_track_count(scopes: ListLen) -> String {
    let clauses: Vec<String> = (0..scopes.get())
        .map(|index| {
            let root = 2 * index + 1;
            let path = root + 1;
            format!(
                "(root_id = ?{root} AND (?{path} = '.' OR relative_path = ?{path} \
                 OR (relative_path > ?{path} || '/' AND relative_path < ?{path} || '0')))"
            )
        })
        .collect();
    format!(
        "SELECT COUNT(*) FROM local_tracks WHERE {}",
        clauses.join(" OR ")
    )
}

pub(super) fn add_to_counter(counter: Counter) -> String {
    let column = counter.column();
    format!("UPDATE library_scan_runs SET {column} = {column} + ?1 WHERE id = ?2")
}

/// Held rows and the rows of other live albums on the same release track
/// that went since the hold began, as (new album, old album, held row).
/// Starts from the holds or the album's tracks, so no hold means no work,
/// and reaches the old album through its release. The old album's rows are
/// read through its index by name: without planner statistics SQLite would
/// build a temporary index over every track instead. Only a run end counts
/// an old row walked past as gone: no run walks anything at a look.
pub(super) fn replacements_sql(source: HeldRows) -> String {
    let (from, held_row, walked_past) = match source {
        HeldRows::UnderHolds => (
            "library_scan_held_scopes h CROSS JOIN local_tracks n",
            held!("n", "h"),
            WALKED_PAST_INDEXED,
        ),
        HeldRows::Remembered => (
            "local_tracks n CROSS JOIN library_scan_unjudged_tracks h \
             ON h.local_track_id = n.id",
            "n.local_album_id = ?1 AND n.availability = 'indexed'",
            "",
        ),
    };
    format!(
        "SELECT DISTINCT n.local_album_id AS new_album_id, a.id AS old_album_id, n.id AS track_id \
         FROM {from} \
         CROSS JOIN local_albums b ON b.id = n.local_album_id \
         CROSS JOIN local_track_external_identities ni \
           ON ni.local_track_id = n.id AND ni.provider = 'musicbrainz' \
         CROSS JOIN local_album_external_identities be \
           ON be.local_album_id = b.id AND be.provider = 'musicbrainz' \
         CROSS JOIN local_album_external_identities ae \
           ON lower(ae.release_mbid) = lower(be.release_mbid) AND ae.provider = 'musicbrainz' \
         CROSS JOIN local_albums a ON a.id = ae.local_album_id \
         CROSS JOIN local_tracks t INDEXED BY idx_local_tracks_album_availability \
           ON t.local_album_id = a.id AND t.availability IN ('missing', 'indexed') \
         CROSS JOIN local_track_external_identities oi \
           ON oi.local_track_id = t.id AND oi.provider = 'musicbrainz' \
         WHERE {held_row} AND b.retired_into_album_id IS NULL \
         AND trim(COALESCE(be.release_mbid, '')) <> '' \
         AND a.id <> b.id AND a.retired_into_album_id IS NULL \
         AND trim(COALESCE(ni.release_track_mbid, '')) <> '' \
         AND lower(trim(oi.release_track_mbid)) = lower(trim(ni.release_track_mbid)) \
         AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = t.id) \
         AND ((t.availability = 'missing' AND t.missing_at >= h.held_at){walked_past}) \
         ORDER BY 1, 2, 3"
    )
}
