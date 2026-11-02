//! The SQL of the corrections module.

// `sql!` needs a macro call in parentheses to take it as one part.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;

use crate::identify::sql::CURRENT_PROOF;
use crate::jobs::sql::live_job;
use crate::publish::sql::HAS_BYTES_RECORD;
use crate::scan::sqlite_store::sql::holds;

droppedneedle_kernel::sql_choice! {
    /// A kind of reference to an artist, counted for a merge.
    pub(crate) enum ReferenceKind {
        AlbumCredits,
        TrackCredits,
        PrimaryAlbums,
        Favorites,
        Ratings,
        History,
    }
}

droppedneedle_kernel::sql! {
    pub(super) CANDIDATE_IDS = "SELECT c.mbid FROM ( \
             SELECT artist_mbid AS mbid, local_artist_id AS artist FROM local_track_artists \
              WHERE artist_mbid IS NOT NULL \
             UNION SELECT artist_mbid, local_artist_id FROM local_album_artists \
              WHERE artist_mbid IS NOT NULL \
             UNION SELECT lower(provider_artist_id), local_artist_id \
              FROM local_artist_external_identities WHERE provider = 'musicbrainz' \
             UNION SELECT artist_mbid, source_local_artist_id FROM library_artist_credit_proofs \
              WHERE source_local_artist_id IS NOT NULL) c \
             JOIN local_artists a ON a.id = c.artist AND a.retired_into_artist_id IS NULL \
             GROUP BY c.mbid HAVING COUNT(*) > 1 ORDER BY c.mbid";
    pub(super) FOLLOW_MOVED = "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
                     right_artist_id, reason_code, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)";
    pub(super) ANY_WAITING = "SELECT EXISTS (SELECT 1 FROM local_artist_merge_candidates \
         WHERE reason_code = ?1 AND state = 'open')";
    pub(super) FOLLOW_WAITING = "SELECT id, left_artist_id, right_artist_id FROM local_artist_merge_candidates \
             WHERE reason_code = ?1 AND state = 'open' ORDER BY id";
    pub(super) BUMP_ARTIST_REVISION = "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) RECORD_MOVE = "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
         right_artist_id, reason_code, created_at, updated_at) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?5 WHERE NOT EXISTS (SELECT 1 FROM local_artists \
          WHERE id IN (?2, ?3) AND kind = 'various_artists')";
    pub(super) FOLLOW_RENAMED = "SELECT DISTINCT m.left_artist_id FROM local_artist_merge_candidates m \
             WHERE m.reason_code IN (?1, ?2) AND m.state = 'open' \
             AND NOT EXISTS (SELECT 1 FROM local_album_artists \
              WHERE local_artist_id = m.left_artist_id) \
             AND NOT EXISTS (SELECT 1 FROM local_track_artists \
              WHERE local_artist_id = m.left_artist_id) \
             AND NOT EXISTS (SELECT 1 FROM local_albums \
              WHERE album_artist_id = m.left_artist_id) \
             ORDER BY m.left_artist_id";
    pub(super) LIVE_ARTIST_REVISION = "SELECT row_revision FROM local_artists WHERE id = ?1 \
                 AND retired_into_artist_id IS NULL";
    pub(super) DROP_ARTIST_LINKS = "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1";
    pub(super) WRITE_MERGED_ARTIST_LINK = "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, selected_at) VALUES (?1, 'musicbrainz', \
                     ?2, ?3, ?4)";
    pub(super) IDENTITY_ROW = "SELECT provider_artist_id, decision_source FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = 'musicbrainz'";
    pub(super) REPOINT_MERGE_CANDIDATES = "UPDATE OR IGNORE local_artist_merge_candidates SET right_artist_id = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 \
         WHERE reason_code IN (?4, ?5, ?6) AND state = 'open' AND right_artist_id = ?1 \
         AND left_artist_id <> ?2";
    pub(super) REPOINT_ALBUM_CREDITS = "UPDATE local_album_artists SET local_artist_id = ?2, row_revision = row_revision + 1 \
         WHERE local_artist_id = ?1";
    pub(super) CONTINUE_REPLACED = "SELECT EXISTS(SELECT 1 FROM local_tracks o JOIN local_tracks n \
             ON n.id = ?2 AND n.local_album_id = o.local_album_id \
             AND n.availability = 'indexed' \
             WHERE o.id = ?1 AND o.local_album_id = ?3 AND o.availability = 'missing' \
             AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = o.id))";
    pub(super) CONTINUE_GIVEN = "SELECT EXISTS(SELECT 1 FROM local_tracks o JOIN local_tracks n \
                 ON n.id = ?2 AND n.availability = 'indexed' \
                 WHERE o.id = ?1 AND o.availability = 'missing' \
                 AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = o.id))";
    pub(super) ADD_TRACK_ALIAS = "INSERT INTO local_track_aliases (alias, local_track_id, created_at, \
         moves_overrides) VALUES (?1, ?2, ?3, ?4)";
    pub(super) REPOINT_TRACK_ALIASES = "UPDATE local_track_aliases SET local_track_id = ?2, \
         moves_overrides = moves_overrides AND ?3 WHERE local_track_id = ?1";
    pub(super) DROP_LIVE_TRACK_ALIASES = "DELETE FROM local_track_aliases WHERE EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.id = local_track_aliases.alias AND t.availability = 'indexed')";
    pub(super) TRACK_ALIAS_TARGET = "SELECT local_track_id, moves_overrides FROM local_track_aliases WHERE alias = ?1";
    pub(super) ACCEPTED_RELEASE = "SELECT lower(trim(e.release_mbid)) FROM local_album_external_identities e \
         WHERE e.local_album_id = ?1 AND e.provider = 'musicbrainz' \
         AND trim(COALESCE(e.release_mbid, '')) <> '' \
         AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
           WHERE s.local_album_id = e.local_album_id) \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
           WHERE c.local_album_id = e.local_album_id)";
    pub(super) PAIRS_AMONG = "SELECT t.id, t.local_album_id, t.availability, lower(trim(i.release_track_mbid)), \
         lower(NULLIF(trim(i.release_mbid), '')), \
         (SELECT x.local_track_id FROM local_track_aliases x WHERE x.alias = t.id) \
         FROM local_tracks t \
         JOIN local_track_external_identities i \
           ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
         WHERE t.local_album_id IN (SELECT value FROM json_each(?1)) \
         AND trim(COALESCE(i.release_track_mbid, '')) <> ''";
    pub(super) ALBUM_OF = "SELECT local_album_id FROM local_tracks WHERE id = ?1";
    pub(super) PLAYS_OF_TRACK = "SELECT id FROM library_play_history WHERE local_track_id = ?1";
    pub(super) FAVORITES_TO_MOVE = "SELECT f.user_id FROM library_user_favorites f WHERE f.item_kind = 'track' \
         AND f.item_id = ?1 AND NOT EXISTS (SELECT 1 FROM library_user_favorites g \
         WHERE g.user_id = f.user_id AND g.item_kind = 'track' AND g.item_id = ?2)";
    pub(super) FAVORITES_IN_BOTH = "SELECT f.user_id, f.created_at, n.display_name FROM library_user_favorites f \
             LEFT JOIN library_user_favorite_names n ON n.user_id = f.user_id \
             AND n.item_kind = f.item_kind AND n.item_id = f.item_id \
             WHERE f.item_kind = 'track' AND f.item_id = ?1 \
             AND EXISTS (SELECT 1 FROM library_user_favorites g WHERE g.user_id = f.user_id \
             AND g.item_kind = 'track' AND g.item_id = ?2)";
    pub(super) FAVORITE_NAMES_IN_BOTH = "SELECT n.user_id, n.display_name FROM library_user_favorite_names n \
             WHERE n.item_kind = 'track' AND n.item_id = ?1 \
             AND EXISTS (SELECT 1 FROM library_user_favorites g WHERE g.user_id = n.user_id \
             AND g.item_kind = 'track' AND g.item_id = ?2) \
             AND NOT EXISTS (SELECT 1 FROM library_user_favorite_names m \
             WHERE m.user_id = n.user_id AND m.item_kind = 'track' AND m.item_id = ?2)";
    pub(super) RATINGS_TO_MOVE = "SELECT r.user_id FROM library_user_ratings r WHERE r.item_kind = 'track' \
         AND r.item_id = ?1 AND NOT EXISTS (SELECT 1 FROM library_user_ratings s \
         WHERE s.user_id = r.user_id AND s.item_kind = 'track' AND s.item_id = ?2)";
    pub(super) RATINGS_IN_BOTH = "SELECT r.user_id, r.rating, r.rated_at FROM library_user_ratings r \
             WHERE r.item_kind = 'track' AND r.item_id = ?1 \
             AND EXISTS (SELECT 1 FROM library_user_ratings s WHERE s.user_id = r.user_id \
             AND s.item_kind = 'track' AND s.item_id = ?2)";
    pub(super) PLAYLIST_ROWS_OF_TRACK = "SELECT id FROM catalog_playlist_tracks WHERE library_file_id = ?1 \
         ORDER BY playlist_id, position";
    pub(super) REPOINT_PLAYLIST_ROWS = "UPDATE catalog_playlist_tracks SET library_file_id = ?2, track_source_id = \
         CASE WHEN track_source_id = ?1 THEN ?2 ELSE track_source_id END \
         WHERE library_file_id = ?1";
    pub(super) BOOKMARKS_OF_TRACK = "SELECT b.user_id, b.changed_at, (SELECT c.changed_at FROM compat_bookmarks c \
             WHERE c.user_id = b.user_id AND c.file_id = ?2) \
             FROM compat_bookmarks b WHERE b.file_id = ?1";
    pub(super) BOOKMARK_ROW = "SELECT file_id, position_ms, comment, created_at, changed_at \
                 FROM compat_bookmarks WHERE user_id = ?1 AND file_id = ?2";
    pub(super) DROP_BOOKMARK = "DELETE FROM compat_bookmarks WHERE user_id = ?1 AND file_id = ?2";
    pub(super) REPOINT_BOOKMARK = "UPDATE compat_bookmarks SET file_id = ?3 WHERE user_id = ?1 AND file_id = ?2";
    pub(super) PLAY_MARKS_OF_TRACK = "SELECT m.user_id, m.marked_played_at, m.marked_unplayed_at, \
             n.local_track_id IS NOT NULL, n.marked_played_at, n.marked_unplayed_at \
             FROM library_user_play_marks m LEFT JOIN library_user_play_marks n \
             ON n.user_id = m.user_id AND n.local_track_id = ?2 \
             WHERE m.local_track_id = ?1";
    pub(super) REPOINT_PLAY_MARK = "UPDATE library_user_play_marks SET local_track_id = ?3 \
                 WHERE user_id = ?1 AND local_track_id = ?2";
    pub(super) WRITE_PLAY_MARK = "UPDATE library_user_play_marks SET marked_played_at = ?3, marked_unplayed_at = ?4 \
             WHERE user_id = ?1 AND local_track_id = ?2";
    pub(super) DROP_PLAY_MARK = "DELETE FROM library_user_play_marks WHERE user_id = ?1 AND local_track_id = ?2";
    pub(super) QUEUE_ITEMS_OF_TRACK = "SELECT user_id, item_index FROM compat_play_queue_items WHERE file_id = ?1 \
             ORDER BY user_id, item_index";
    pub(super) REPOINT_QUEUE_ITEMS = "UPDATE compat_play_queue_items SET file_id = ?2 WHERE file_id = ?1";
    pub(super) OVERRIDES_TO_MOVE = "SELECT o.id, o.updated_at, o.row_revision FROM library_management_overrides o \
             WHERE o.subject_kind = 'track' AND o.local_track_id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM library_management_overrides w \
             WHERE w.subject_kind = 'track' AND w.local_track_id = ?2 \
             AND w.field_name = o.field_name)";
    pub(super) REPOINT_OVERRIDE = "UPDATE library_management_overrides SET local_track_id = ?2, updated_at = ?3, \
             row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source, row_revision, \
         EXISTS(SELECT 1 FROM library_custom_edition_active c WHERE c.local_album_id = ?1) \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2";
    pub(super) ALBUM_TITLE = "SELECT title FROM local_albums WHERE id = ?1";
    pub(super) OPEN_REVIEW = "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, candidates_json, \
         state, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)";
    pub(super) ALBUM_TRACKS = "SELECT id FROM local_tracks WHERE local_album_id = ?1";
    pub(super) LOAD_ALBUM = "SELECT id, title, COALESCE(album_artist_name, ''), row_revision, grouping_source \
             FROM local_albums WHERE id = ?1 AND retired_into_album_id IS NULL";
    pub(super) ALBUM_TRACK_IDS = "SELECT id FROM local_tracks WHERE local_album_id = ?1 ORDER BY relative_path, id";
    pub(super) IDENTITY_STATE = "SELECT row_revision FROM local_albums WHERE id = ?1";
    pub(super) RETIRE_ALBUM = "UPDATE local_albums SET retired_into_album_id = ?2, grouping_locked = 1, \
         updated_at = ?3, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) REPOINT_RETIRED_ALBUMS = "UPDATE local_albums SET retired_into_album_id = ?2 WHERE retired_into_album_id = ?1";
    pub(super) DROP_OPEN_RETAG_PAIRS = "DELETE FROM library_retag_pairs WHERE old_album_id = ?1 AND state = 'open'";
    pub(super) REPOINT_ALBUM_ALIASES = "UPDATE local_album_aliases SET local_album_id = ?2 WHERE local_album_id = ?1";
    pub(super) ADD_MERGED_ALBUM_ALIAS = "INSERT OR IGNORE INTO local_album_aliases (alias, local_album_id, kind, created_at) \
         VALUES (?1, ?2, 'merged_album', ?3)";
    pub(super) DROP_ALBUM_TAKEOVER = "DELETE FROM library_album_takeovers WHERE local_album_id = ?1";
    pub(super) COPY_EXCLUSION = "INSERT OR IGNORE INTO library_management_exclusions (local_album_id, reason, \
         excluded_by_user_id, excluded_at) SELECT ?2, reason, excluded_by_user_id, excluded_at \
         FROM library_management_exclusions WHERE local_album_id = ?1";
    pub(super) DROP_EXCLUSION = "DELETE FROM library_management_exclusions WHERE local_album_id = ?1";
    pub(super) REPOINT_ALBUM_OVERRIDES = "UPDATE OR IGNORE library_management_overrides SET local_album_id = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 \
         WHERE local_album_id = ?1 AND subject_kind = 'album'";
    pub(super) REJECT_PENDING_REVIEWS = "UPDATE library_identify_reviews SET state = 'rejected', updated_at = ?2 \
         WHERE local_album_id = ?1 AND state = 'pending'";
    pub(super) DROP_WAITING_JOBS = "DELETE FROM library_identify_jobs WHERE local_album_id = ?1 \
         AND state IN ('queued', 'deferred')";
    pub(super) OFFER = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
             AND retired_into_album_id IS NULL)";
    pub(super) REGROUP = "UPDATE local_albums SET grouping_source = 'manual', grouping_locked = 1, \
             updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) ADD_SPLIT_ALBUM = "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, album_artist_sort_name, \
         year, original_release_date, primary_genre, is_compilation, grouping_source, \
         grouping_locked, created_at, updated_at) \
         SELECT ?1, root_id, 'manual:' || ?1, ?2, ?3, ?4, ?5, album_artist_id, \
         album_artist_sort_name, year, original_release_date, primary_genre, is_compilation, \
         'manual', 1, ?6, ?6 FROM local_albums WHERE id = ?7";
    pub(super) COPY_SPLIT_ALBUM_CREDITS = "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, \
         credited_name, join_phrase) SELECT ?1, position, local_artist_id, role, credited_name, \
         join_phrase FROM local_album_artists WHERE local_album_id = ?2";
    pub(super) EMPTIED_BY_NAME = "SELECT id, right_artist_id, reason_code FROM local_artist_merge_candidates \
                 WHERE left_artist_id = ?1 AND reason_code IN (?2, ?3) AND state = 'open' \
                 ORDER BY id";
    pub(super) LIVE_ARTISTS_OF_PAIR = "SELECT COUNT(*) FROM local_artists WHERE id IN (?1, ?2) \
                 AND retired_into_artist_id IS NULL";
    pub(super) LINK_DECISION_SOURCE = "SELECT decision_source FROM local_artist_external_identities \
             WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
             AND lower(provider_artist_id) = ?2";
    pub(super) MBIDS_FROM_MERGE_ACTIONS = "SELECT json_extract(before_json, '$.provider_artist_id') \
             FROM library_catalog_actions WHERE local_artist_id = ?1 \
             AND reason_code = 'MUSICBRAINZ_MERGED_ARTIST_ID' \
             AND json_valid(after_json) AND json_valid(before_json) \
             AND json_extract(after_json, '$.provider_artist_id') = ?2 \
             ORDER BY created_at DESC LIMIT 1";
    pub(super) FULLY_PROVEN = "SELECT EXISTS (SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1) \
             OR EXISTS (SELECT 1 FROM local_album_artists WHERE local_artist_id = ?1)";
    pub(super) OTHER_ID = "SELECT " + MERGE_BASIS_OTHER_ID;
    pub(super) FILES_PROVE = "SELECT EXISTS (SELECT 1 FROM local_track_artists \
             WHERE local_artist_id = ?1 AND artist_mbid = ?2) \
             OR EXISTS (SELECT 1 FROM local_album_artists \
             WHERE local_artist_id = ?1 AND artist_mbid = ?2)";
    pub(super) LIVE_ROW = "WITH RECURSIVE chain(id, next, depth) AS ( \
         SELECT id, retired_into_artist_id, 0 FROM local_artists WHERE id = ?1 \
         UNION ALL SELECT a.id, a.retired_into_artist_id, chain.depth + 1 \
          FROM local_artists a JOIN chain ON a.id = chain.next WHERE chain.depth < 16) \
         SELECT id FROM chain WHERE next IS NULL LIMIT 1";
    pub(super) CONTRADICTED = "SELECT lower(provider_artist_id) FROM local_artist_external_identities \
             WHERE local_artist_id = ?1 AND provider = 'musicbrainz'";
    pub(super) MARKED_DISTINCT = "WITH members(id) AS (SELECT value FROM json_each(?1)), \
         pairs AS (SELECT \
          COALESCE((SELECT local_artist_id FROM local_artist_aliases \
           WHERE alias = d.left_artist_id), d.left_artist_id) AS left_id, \
          COALESCE((SELECT local_artist_id FROM local_artist_aliases \
           WHERE alias = d.right_artist_id), d.right_artist_id) AS right_id \
          FROM library_artist_reconciliation_dismissals d) \
         SELECT EXISTS(SELECT 1 FROM pairs WHERE left_id <> right_id \
          AND left_id IN (SELECT id FROM members) AND right_id IN (SELECT id FROM members))";
    pub(super) ORDINARY_NAME = "SELECT display_name FROM local_artists WHERE id = ?1 \
         AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?2, ?3)";
    pub(super) PLACE_ALBUM = "SELECT release_mbid, selected_at FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = 'musicbrainz'";
    pub(super) EDITION = "SELECT i.release_track_mbid, i.recording_mbid, i.medium_position, \
         i.release_track_position FROM local_tracks t \
         JOIN local_track_external_identities i \
           ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
         WHERE t.local_album_id = ?1 AND trim(COALESCE(i.release_track_mbid, '')) <> '' \
         AND (trim(COALESCE(i.release_mbid, '')) = '' \
           OR lower(trim(i.release_mbid)) = lower(trim(?2))) \
         ORDER BY t.id";
    pub(super) HELD_TRACKS = "SELECT lower(trim(i.release_track_mbid)) FROM local_tracks t \
         JOIN local_track_external_identities i \
           ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
         WHERE t.local_album_id = ?1 AND +t.availability = 'indexed' \
         AND trim(COALESCE(i.release_track_mbid, '')) <> ''";
    pub(super) RETAGGED_BY_HAND = "SELECT t.id, t.missing_since, t.album_title_folded, \
             COALESCE(t.album_artist_name_folded, ''), t.disc_number, t.track_number \
             FROM local_tracks t JOIN local_track_external_identities i \
               ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
             WHERE t.local_album_id = ?1 AND t.availability = 'missing' \
             AND lower(trim(i.release_track_mbid)) = ?2 ORDER BY t.id";
    pub(super) REPLACED_BY = "SELECT EXISTS(SELECT 1 FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND state = ?2)";
    pub(super) GONE_TRACK_FACTS = "SELECT missing_since, root_id, relative_path FROM local_tracks WHERE id = ?1";
    pub(super) NEW_TRACK_FACTS = "SELECT imported_at, root_id, relative_path FROM local_tracks WHERE id = ?1";
    pub(super) SCAN_OVERLAP_AND_CHANGE = "SELECT EXISTS(SELECT 1 FROM library_scan_runs \
               WHERE started_at <= ?2 AND ?2 <= COALESCE(terminal_at, ?3)), \
             EXISTS(SELECT 1 FROM library_catalog_actions WHERE local_album_id = ?4 \
               AND action_kind = 'remove_track' AND local_track_id = ?1)";
    pub(super) RUN_ENDED_BY = "SELECT EXISTS(SELECT 1 FROM library_scan_runs WHERE terminal_at <= ?1)";
    pub(super) WALKED = "SELECT s.relative_path FROM library_scan_runs r \
             JOIN library_scan_run_scopes s ON s.run_id = r.id AND s.root_id = ?1 \
               AND s.discovery_state = 'completed' \
             WHERE r.state = 'completed' AND r.terminal_at >= ?2 AND r.terminal_at < ?3 \
             AND NOT EXISTS (SELECT 1 FROM library_scan_failures f WHERE f.run_id = r.id \
               AND f.root_id = s.root_id AND f.relative_path = s.relative_path \
               AND f.failure_code = 'MASS_MISSING_GUARD')";
    pub(super) SET_TRACK_ALBUM = "UPDATE local_tracks SET local_album_id = ?2 WHERE id = ?1";
    pub(super) ALBUM_HAS_LOCKED_MEMBER = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1 \
             AND membership_locked = 1)";
    pub(super) RESET_ALBUM_GROUPING = "UPDATE local_albums SET grouping_source = CASE WHEN ?2 THEN grouping_source \
             ELSE 'automatic' END, grouping_locked = ?2, updated_at = ?3, \
             row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) ALBUM_HAS_CUSTOM_EDITION = "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)";
    pub(super) DROP_AUTOMATIC_QUEUE_ROW = "DELETE FROM library_management_automatic_queue WHERE local_album_id = ?1";
    pub(super) MOVE_ALBUM_TRACKS = "UPDATE local_tracks SET local_album_id = ?2 WHERE local_album_id = ?1";
    pub(super) MOVE_ALBUM_PLAYLIST_ROWS = "UPDATE catalog_playlist_tracks SET album_id = ?2 WHERE album_id = ?1";
    pub(super) ALBUM_IS_AUTOMATIC = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1 \
             AND availability = 'indexed' AND applied_policy = 'automatic')";
    pub(super) SIDE = "SELECT title, COALESCE(album_artist_name, ''), (SELECT COUNT(*) FROM local_tracks t \
         WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         FROM local_albums a WHERE a.id = ?1";
    pub(super) DROP_RETAG_PAIR = "DELETE FROM library_retag_pairs WHERE old_album_id = ?1 AND new_album_id = ?2";
    pub(super) DISMISS = "UPDATE library_retag_pairs SET state = ?3, updated_at = ?4 \
         WHERE old_album_id = ?1 AND state = ?5 AND new_album_id IN \
         (SELECT id FROM local_albums WHERE id = ?2 OR retired_into_album_id = ?2)";
    pub(super) FIND = "SELECT p.new_album_id, p.state FROM library_retag_pairs p \
             JOIN local_albums n ON n.id = p.new_album_id \
             WHERE p.old_album_id = ?1 AND (p.new_album_id = ?2 OR n.retired_into_album_id = ?2) \
             ORDER BY p.new_album_id = ?2 DESC, p.state = ?3 DESC LIMIT 1";
    pub(super) RETIRED_INTO = "SELECT retired_into_album_id FROM local_albums WHERE id = ?1";
    pub(super) SUCCESSOR = "SELECT COALESCE(retired_into_album_id, id) FROM local_albums WHERE id = ?1";
    pub(super) REPLAY = "SELECT after_json FROM library_catalog_actions WHERE idempotency_key = ?1";
    pub(super) RECORD = "INSERT INTO library_catalog_actions (id, idempotency_key, actor_user_id, action_kind, \
         local_artist_id, local_album_id, before_json, after_json, reason_code, created_at) \
         VALUES (?1, ?2, (SELECT id FROM auth_users WHERE id = ?3), ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
    /// The track a local id names now: the track itself while its file is
    /// indexed, else the track its alias names (a missing track that
    /// continued into another), else the id itself, so a read's own
    /// availability test answers for it as for any id.
    pub fragment CURRENT_TRACK = "SELECT id FROM ( \
    SELECT id, 0 AS rank FROM local_tracks WHERE id = ?1 AND availability = 'indexed' \
    UNION ALL SELECT local_track_id, 1 FROM local_track_aliases WHERE alias = ?1 \
    UNION ALL SELECT ?1, 2) \
    ORDER BY rank LIMIT 1";
    /// Albums with a missing, unaliased row mapped to a release track that an
    /// indexed row of the same album is also mapped to, over every missing
    /// row. Reads each album's indexed rows by album.
    pub(super) CANDIDATE_ALBUMS = CANDIDATE_ALBUM_ROWS + "o.availability = 'missing'" + CANDIDATE_ALBUM_TEST;
    /// [`CANDIDATE_ALBUMS`] for album `?1` only, by its rows.
    pub(super) CANDIDATE_ALBUM = CANDIDATE_ALBUM_ROWS + "+o.availability = 'missing' AND o.local_album_id = ?1" + CANDIDATE_ALBUM_TEST;
    pub(super) fragment CANDIDATE_ALBUM_ROWS = "SELECT DISTINCT o.local_album_id FROM local_tracks o \
    JOIN local_track_external_identities oi \
      ON oi.local_track_id = o.id AND oi.provider = 'musicbrainz' \
    WHERE oi.release_track_mbid IS NOT NULL AND ";
    pub(super) fragment CANDIDATE_ALBUM_TEST = " AND NOT EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = o.id) \
    AND EXISTS (SELECT 1 FROM local_tracks n \
      JOIN local_track_external_identities ni \
        ON ni.local_track_id = n.id AND ni.provider = 'musicbrainz' \
      WHERE n.local_album_id = o.local_album_id AND +n.availability = 'indexed' \
      AND lower(ni.release_track_mbid) = lower(oi.release_track_mbid))";
    /// Aliases whose own row is not indexed and that still hold something a
    /// person made: one statement per table. A track override that the
    /// target already has for its field, or on an alias whose overrides stay
    /// (a replaced version), stays on the old row and does not count.
    pub(super) HOLDING_ALIASES = "WITH dead AS (SELECT x.alias FROM local_track_aliases x \
      CROSS JOIN local_tracks t ON t.id = x.alias WHERE t.availability <> 'indexed') \
    SELECT local_track_id FROM library_play_history WHERE local_track_id IN dead \
    UNION SELECT item_id FROM library_user_favorites \
      WHERE item_kind = 'track' AND item_id IN dead \
    UNION SELECT item_id FROM library_user_ratings \
      WHERE item_kind = 'track' AND item_id IN dead \
    UNION SELECT library_file_id FROM catalog_playlist_tracks WHERE library_file_id IN dead \
    UNION SELECT file_id FROM compat_bookmarks WHERE file_id IN dead \
    UNION SELECT local_track_id FROM library_user_play_marks WHERE local_track_id IN dead \
    UNION SELECT file_id FROM compat_play_queue_items WHERE file_id IN dead \
    UNION SELECT o.local_track_id FROM library_management_overrides o \
      JOIN local_track_aliases x ON x.alias = o.local_track_id \
      WHERE o.subject_kind = 'track' AND o.local_track_id IN dead \
      AND x.moves_overrides = 1 AND NOT EXISTS (SELECT 1 FROM library_management_overrides w \
        WHERE w.subject_kind = 'track' AND w.local_track_id = x.local_track_id \
        AND w.field_name = o.field_name)";
    /// True when artist `?1` is proven to be an artist other than `?2`: a
    /// credit's file holds another id, or a current proof names another.
    /// Each probe is a range on an (artist, id) index.
    pub(super) fragment MERGE_BASIS_OTHER_ID = "(EXISTS(SELECT 1 FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid < ?2) \
     OR EXISTS(SELECT 1 FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid > ?2) \
     OR EXISTS(SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid < ?2) \
     OR EXISTS(SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid > ?2) \
     OR EXISTS(SELECT 1 FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid < ?2 AND (" + CURRENT_PROOF + ")) \
     OR EXISTS(SELECT 1 FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid > ?2 AND (" + CURRENT_PROOF + ")))";
    /// The rows a current proof says are `?1`.
    pub(super) PROOF_ROWS = "SELECT DISTINCT p.source_local_artist_id \
     FROM library_artist_credit_proofs p WHERE p.artist_mbid = ?1 \
     AND p.source_local_artist_id IS NOT NULL AND (" + CURRENT_PROOF + ")";
    /// The `created_at` of row `?1`, proven to be `?2` without a link, when it
    /// belongs in that group: live, neither sentinel (`?3`, `?4`) nor a
    /// Various Artists row, no link, and nothing proving it another artist.
    pub(super) UNLINKED_MEMBER = "SELECT created_at FROM local_artists WHERE id = ?1 \
     AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?3, ?4) \
     AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities WHERE local_artist_id = ?1) \
     AND NOT " + MERGE_BASIS_OTHER_ID;
    /// True when every track and album credit of artist `?1` holds `?2`: by
    /// its file's id, or by a current proof of `?2` at that very credit.
    pub(super) EVERY_CREDIT_HOLDS = "SELECT NOT EXISTS (SELECT 1 FROM local_track_artists credit \
     WHERE credit.local_artist_id = ?1 AND credit.artist_mbid IS NOT ?2 \
     AND NOT EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
      WHERE p.subject_kind = 'track' AND p.subject_id = credit.local_track_id \
      AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
      AND p.artist_mbid = ?2 AND (" + CURRENT_PROOF + "))) \
     AND NOT EXISTS (SELECT 1 FROM local_album_artists credit \
     WHERE credit.local_artist_id = ?1 AND credit.artist_mbid IS NOT ?2 \
     AND NOT EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
      WHERE p.subject_kind = 'album' AND p.subject_id = credit.local_album_id \
      AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
      AND p.artist_mbid = ?2 AND (" + CURRENT_PROOF + ")))";
    /// Up to `?3` of artist `?1`'s credits whose file holds `?2`.
    pub(super) FILE_IDS = "SELECT 'track', local_track_id, position FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid = ?2 \
     UNION ALL SELECT 'album', local_album_id, position FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid = ?2 LIMIT ?3";
    /// How many of artist `?1`'s credits have a file holding `?2`.
    pub(super) FILE_ID_COUNT = "SELECT \
     (SELECT COUNT(*) FROM local_track_artists WHERE local_artist_id = ?1 AND artist_mbid = ?2) \
     + (SELECT COUNT(*) FROM local_album_artists WHERE local_artist_id = ?1 AND artist_mbid = ?2)";
    /// Up to `?3` current proofs that artist `?1` is `?2`.
    pub(super) RELEASE_PROOFS = "SELECT p.evidence_hash, p.release_mbid \
     FROM library_artist_credit_proofs p WHERE p.source_local_artist_id = ?1 \
     AND p.artist_mbid = ?2 AND (" + CURRENT_PROOF + ") \
     ORDER BY p.subject_kind, p.subject_id, p.credit_position LIMIT ?3";
    /// How many current proofs say artist `?1` is `?2`.
    pub(super) RELEASE_PROOF_COUNT = "SELECT COUNT(*) FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid = ?2 AND (" + CURRENT_PROOF + ")";
    /// True when something other than credits still on it keeps row `?1`,
    /// whose proven credits for `?3` moved to the row that is now `?2`, from
    /// following them: it is not live, is a sentinel (`?4`, `?5`) or a Various
    /// Artists row, has a link, has a proof naming another id, or has a play
    /// of a track now credited to neither row (a play goes with its row, and
    /// must not land on another artist's).
    pub(super) HELD_BACK = "SELECT NOT EXISTS (SELECT 1 FROM local_artists WHERE id = ?1 \
     AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?4, ?5)) \
     OR EXISTS (SELECT 1 FROM local_artist_external_identities WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM library_artist_credit_proofs \
      WHERE (source_local_artist_id = ?1 OR local_artist_id = ?1) AND artist_mbid <> ?3) \
     OR EXISTS (SELECT 1 FROM library_play_history h WHERE h.local_artist_id = ?1 \
      AND EXISTS (SELECT 1 FROM local_track_artists c WHERE c.local_track_id = h.local_track_id) \
      AND NOT EXISTS (SELECT 1 FROM local_track_artists c \
       WHERE c.local_track_id = h.local_track_id AND c.local_artist_id IN (?1, ?2)))";
    /// True when row `?1` still has a credit, or an album names it as album
    /// artist: it follows its credits once the rest of them have moved.
    pub(super) STILL_CREDITED = "SELECT EXISTS (SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM local_albums WHERE album_artist_id = ?1)";
    /// Indexed rows with an embedded release-track or recording id and no
    /// identity naming a release track, on albums whose match names a release
    /// and that have no custom edition, no queued placement and no match state
    /// but a choice that fitted no file. A row whose embedded release-track id
    /// an indexed row of the album already holds (a second copy) is left out.
    /// Starts from the albums' identities and reads each album's indexed rows
    /// by album.
    pub(super) CANDIDATES = "SELECT t.local_album_id, t.id, \
    lower(trim(COALESCE(t.embedded_release_track_mbid, ''))), \
    lower(trim(COALESCE(t.embedded_recording_mbid, ''))), \
    lower(trim(COALESCE(t.embedded_release_mbid, ''))), \
    t.disc_number, t.track_number, t.imported_at, \
    t.album_title_folded, COALESCE(t.album_artist_name_folded, '') \
    FROM local_album_external_identities ae \
    JOIN local_tracks t ON t.local_album_id = ae.local_album_id AND +t.availability = 'indexed' \
    WHERE ae.provider = 'musicbrainz' AND trim(COALESCE(ae.release_mbid, '')) <> '' \
    AND (trim(COALESCE(t.embedded_release_track_mbid, '')) <> '' \
      OR trim(COALESCE(t.embedded_recording_mbid, '')) <> '') \
    AND NOT EXISTS (SELECT 1 FROM local_track_external_identities i \
      WHERE i.local_track_id = t.id AND i.provider = 'musicbrainz' \
      AND trim(COALESCE(i.release_track_mbid, '')) <> '') \
    AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
      WHERE c.local_album_id = ae.local_album_id) \
    AND NOT EXISTS (SELECT 1 FROM library_edition_remap_queue q \
      WHERE q.local_album_id = ae.local_album_id) \
    AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
      WHERE s.local_album_id = ae.local_album_id \
      AND s.reason_code IS NOT 'CHOSEN_EDITION_FITS_NO_FILE') \
    AND NOT EXISTS (SELECT 1 FROM local_tracks h \
      JOIN local_track_external_identities hi \
        ON hi.local_track_id = h.id AND hi.provider = 'musicbrainz' \
      WHERE h.local_album_id = t.local_album_id AND +h.availability = 'indexed' \
      AND trim(COALESCE(t.embedded_release_track_mbid, '')) <> '' \
      AND lower(trim(hi.release_track_mbid)) = lower(trim(t.embedded_release_track_mbid))) \
    ORDER BY t.local_album_id, t.id";
    /// The scan run that started last, as a SQL expression.
    pub(super) fragment LATEST_RUN = "(SELECT id FROM library_scan_runs WHERE started_at IS NOT NULL \
    ORDER BY started_at DESC LIMIT 1)";
    /// Old and new albums on one accepted exact release: the old one with no
    /// indexed track and a mapped track that went missing since the oldest
    /// run on record began, the new one with an indexed track. Starts from the
    /// missing rows and joins the two albums through their release.
    pub(super) RELEASE_JOIN_CANDIDATES = "SELECT DISTINCT a.id, b.id FROM local_tracks o \
    JOIN local_albums a ON a.id = o.local_album_id AND a.retired_into_album_id IS NULL \
    JOIN local_album_external_identities ae \
      ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' \
    JOIN local_album_external_identities be \
      ON lower(be.release_mbid) = lower(ae.release_mbid) AND be.provider = 'musicbrainz' \
      AND be.local_album_id <> a.id \
    JOIN local_albums b ON b.id = be.local_album_id AND b.retired_into_album_id IS NULL \
    WHERE o.availability = 'missing' \
    AND o.missing_since >= (SELECT (MIN(started_at) - 0.5) / 1000.0 FROM library_scan_runs) \
    AND (?1 IS NULL OR a.id = ?1) AND (?2 IS NULL OR b.id = ?2) \
    AND trim(COALESCE(ae.release_mbid, '')) <> '' \
    AND EXISTS (SELECT 1 FROM local_track_external_identities oi \
      WHERE oi.local_track_id = o.id AND oi.provider = 'musicbrainz' \
      AND trim(COALESCE(oi.release_track_mbid, '')) <> '') \
    AND NOT EXISTS (SELECT 1 FROM local_tracks t \
      WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
    AND EXISTS (SELECT 1 FROM local_tracks t \
      WHERE t.local_album_id = b.id AND t.availability = 'indexed') \
    AND NOT EXISTS (SELECT 1 FROM library_album_match_state s \
      WHERE s.local_album_id IN (a.id, b.id)) \
    AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
      WHERE c.local_album_id IN (a.id, b.id)) \
    ORDER BY a.id, b.id";
    /// Open pairs with a reason a person decides on, both albums live, the
    /// new album standing for the album it was merged into since, and with
    /// `?6` an album id, only those naming it. Pairs already joined that way
    /// are left out. One row per old album and the album its new one stands
    /// for: the newest pair's reason and time (SQLite takes the bare columns
    /// from the row that holds the `MAX`).
    pub(super) fragment OPEN_PAIRS = "SELECT p.old_album_id AS old_id, \
     COALESCE(n.retired_into_album_id, n.id) AS new_id, p.reason AS reason, \
     MAX(p.recorded_at) AS recorded_at \
     FROM library_retag_pairs p \
     JOIN local_albums o ON o.id = p.old_album_id \
     JOIN local_albums n ON n.id = p.new_album_id \
     LEFT JOIN local_albums t ON t.id = n.retired_into_album_id \
     WHERE p.state = ?1 AND p.reason IN (?2, ?3, ?4, ?5) \
     AND o.retired_into_album_id IS NULL \
     AND (n.retired_into_album_id IS NULL OR t.retired_into_album_id IS NULL) \
     AND COALESCE(n.retired_into_album_id, n.id) <> p.old_album_id \
     AND (?6 IS NULL OR p.old_album_id = ?6 OR COALESCE(n.retired_into_album_id, n.id) = ?6) \
     GROUP BY p.old_album_id, COALESCE(n.retired_into_album_id, n.id)";
    pub(super) OPEN_PAIR_COUNT = "SELECT COUNT(*) FROM (" + OPEN_PAIRS + ")";
    pub(super) OPEN_PAIR_PAGE = "SELECT old_id, new_id, reason, recorded_at FROM (" + OPEN_PAIRS + ") \
             ORDER BY recorded_at DESC, old_id, new_id LIMIT ?7 OFFSET ?8";
    /// (artist id, MusicBrainz artist id) for the artists of a list that have a link.
    family ARTIST_LINKS_OF = artist_links_of;
    /// References of one kind to each artist of a list: (artist, count).
    family REFERENCES_OF = references_of;
    pub(super) REPOINT_TRACK_CREDITS = "UPDATE local_track_artists SET local_artist_id = ?2, row_revision = row_revision + 1 \
         WHERE local_artist_id = ?1";
    pub(super) REPOINT_ALBUM_ARTIST = "UPDATE local_albums SET album_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE album_artist_id = ?1";
    /// The retiree's source links the survivor already has.
    pub(super) DROP_SHARED_SOURCE_LINKS = "DELETE FROM local_entity_source_links WHERE local_artist_id = ?1 AND EXISTS \
         (SELECT 1 FROM local_entity_source_links kept WHERE kept.local_artist_id = ?2 \
          AND kept.provider = local_entity_source_links.provider \
          AND kept.external_entity_type = local_entity_source_links.external_entity_type \
          AND kept.external_id = local_entity_source_links.external_id)";
    pub(super) REPOINT_SOURCE_LINKS = "UPDATE local_entity_source_links SET local_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE local_artist_id = ?1";
    pub(super) DROP_ARTIST_LINK = "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1";
    /// The only writer of `local_artists.retired_into_artist_id`.
    pub(super) RETIRE_ARTIST_ROW = "UPDATE local_artists SET retired_into_artist_id = ?2, converged_mbid = ?4, \
         updated_at = ?3, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) REPOINT_ARTIST_ALIASES = "UPDATE local_artist_aliases SET local_artist_id = ?2 WHERE local_artist_id = ?1";
    pub(super) ALIAS_RETIRED_ARTIST = "INSERT OR IGNORE INTO local_artist_aliases (alias, local_artist_id, kind, created_at) \
         VALUES (?1, ?2, 'merged_artist', ?3)";
    pub(super) REPOINT_CREDIT_PROOFS = "UPDATE library_artist_credit_proofs SET \
         local_artist_id = CASE WHEN local_artist_id = ?1 THEN ?2 ELSE local_artist_id END, \
         source_local_artist_id = CASE WHEN source_local_artist_id = ?1 THEN ?2 \
         ELSE source_local_artist_id END, updated_at = CAST(?3 * 1000 AS INTEGER), \
         row_revision = row_revision + 1 \
         WHERE local_artist_id = ?1 OR source_local_artist_id = ?1";
    pub(super) RESOLVE_MERGE_CANDIDATES = "UPDATE local_artist_merge_candidates SET state = 'resolved', \
             updated_at = CAST(round(?3 * 1000) AS INTEGER), \
         row_revision = row_revision + 1 WHERE state = 'open' \
         AND (left_artist_id = ?1 OR right_artist_id = ?1)";
    /// The rows of a list of tracks, in path order.
    family TRACK_ROWS_IN = track_rows_in;
    /// Lock a list of tracks to album `?1` by hand.
    family LOCK_TRACKS_TO_ALBUM = lock_tracks_to_album;
    /// Whether a missing row of album `?1` has a record of its bytes.
    pub(super) MISSING_ROW_HAS_RECORD = "SELECT EXISTS(SELECT 1 FROM local_tracks t \
         JOIN library_track_management_state s ON s.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND +t.availability = 'missing' AND " + HAS_BYTES_RECORD + ")";
    /// Whether track `?1` has a record of its bytes.
    pub(super) TRACK_HAS_RECORD = "SELECT EXISTS(SELECT 1 FROM library_track_management_state s \
             WHERE s.local_track_id = ?1 AND " + HAS_BYTES_RECORD + ")";
    /// Whether a row of album `?1` outside the replaced rows `?2` holds it, with the album's row count.
    pub(super) ALBUM_STILL_HELD = "SELECT EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?1 \
             AND t.id NOT IN (SELECT value FROM json_each(?2)) AND " + (holds!("t", LATEST_RUN)) + "), \
             (SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1)";
    /// Whether the new row arrived (`?1`) before the scan that marked old row `?3` gone at `?2` began, and the old row holds its album.
    pub(super) WAITS_FOR_A_SCAN = "SELECT ?1 < (SELECT MAX(started_at) FROM library_scan_runs WHERE started_at <= ?2) \
             AND EXISTS(SELECT 1 FROM local_tracks o WHERE o.id = ?3 AND " + (holds!("o", LATEST_RUN)) + ")";
    /// Hand a list of tracks back to automatic grouping.
    family RELEASE_TRACKS_TO_AUTOMATIC = release_tracks_to_automatic;
    /// How many of a list of tracks a person locked or placed.
    family TRACKS_HELD_BY_HAND = tracks_held_by_hand;
    /// An open draft on album `?1`, or a plan item of a job still to come (`?2` is now).
    pub(super) LIVE_WORK_ON_ALBUM = "SELECT EXISTS(SELECT 1 FROM library_contribution_drafts WHERE local_album_id = ?1 \
             AND state IN ('draft','ready','seeded','verifying','needs_review')) \
             OR EXISTS(SELECT 1 FROM library_management_plan_items item \
             JOIN library_operation_jobs job ON job.id = item.job_id \
             WHERE item.local_album_id = ?1 AND " + (live_job!("?2")) + ")";
    /// Whether any file of album `?1`, whatever its availability, has a record of its bytes.
    pub(super) ALBUM_HAS_RECORD = "SELECT EXISTS(SELECT 1 FROM local_tracks t \
         JOIN library_track_management_state s ON s.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND " + HAS_BYTES_RECORD + ")";
}

pub(super) fn artist_links_of(ids: ListLen) -> String {
    let marks = marks(ids);
    format!(
        "SELECT local_artist_id, provider_artist_id FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND local_artist_id IN ({marks}) \
         ORDER BY local_artist_id"
    )
}

pub(super) fn track_rows_in(ids: ListLen) -> String {
    let marks = marks(ids);
    format!(
        "SELECT id, local_album_id, row_revision FROM local_tracks WHERE id IN ({marks}) \
             ORDER BY relative_path, id"
    )
}

pub(super) fn lock_tracks_to_album(tracks: ListLen) -> String {
    let marks = marks(tracks);
    format!(
        "UPDATE local_tracks SET local_album_id = ?1, membership_source = 'manual', \
             membership_locked = 1, row_revision = row_revision + 1 WHERE id IN ({marks})"
    )
}

pub(super) fn release_tracks_to_automatic(tracks: ListLen) -> String {
    format!(
        "UPDATE local_tracks SET membership_locked = 0, membership_source = 'automatic', \
             row_revision = row_revision + 1 WHERE id IN ({})",
        marks(tracks)
    )
}

pub(super) fn tracks_held_by_hand(tracks: ListLen) -> String {
    format!(
        "SELECT COUNT(*) FROM local_tracks WHERE id IN ({}) \
             AND (membership_locked = 1 OR membership_source <> 'automatic')",
        marks(tracks)
    )
}

fn marks(len: ListLen) -> String {
    vec!["?"; len.get().max(1)].join(",")
}

pub(crate) fn references_of(kind: ReferenceKind, ids: ListLen) -> String {
    let marks = marks(ids);
    match kind {
        ReferenceKind::AlbumCredits => format!(
            "SELECT local_artist_id, COUNT(*) FROM local_album_artists \
             WHERE local_artist_id IN ({marks}) GROUP BY 1"
        ),
        ReferenceKind::TrackCredits => format!(
            "SELECT local_artist_id, COUNT(*) FROM local_track_artists \
             WHERE local_artist_id IN ({marks}) GROUP BY 1"
        ),
        ReferenceKind::PrimaryAlbums => format!(
            "SELECT album_artist_id, COUNT(*) FROM local_albums \
             WHERE album_artist_id IN ({marks}) GROUP BY 1"
        ),
        ReferenceKind::Favorites => format!(
            "SELECT item_id, COUNT(*) FROM library_user_favorites \
             WHERE item_kind = 'artist' AND item_id IN ({marks}) GROUP BY 1"
        ),
        ReferenceKind::Ratings => format!(
            "SELECT item_id, COUNT(*) FROM library_user_ratings \
             WHERE item_kind = 'artist' AND item_id IN ({marks}) GROUP BY 1"
        ),
        ReferenceKind::History => format!(
            "SELECT local_artist_id, COUNT(*) FROM library_play_history \
             WHERE local_artist_id IN ({marks}) GROUP BY 1"
        ),
    }
}
