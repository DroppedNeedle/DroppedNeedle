//! The statements of the held list (`acquire_held_entries`,
//! `acquire_held_files`).

use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql! {
    /// The entry states that still wait for a person: the one meaning of
    /// an open entry.
    pub fragment OPEN = "('held', 'placing')";
    /// Every entry column, in the order `Entry::from_row` reads them.
    pub(super) fragment ENTRY_ROW = "SELECT id, user_id, origin, group_key, release_group_mbid, \
             recording_mbid, drop_item_id, artist_name, album_title, state, created_at, \
             expires_at, retry_at FROM acquire_held_entries";
    /// Every file column, in the order `HeldFile::from_row` reads them.
    pub(super) fragment FILE_ROW = "SELECT id, entry_id, root, path, original_name, release_mbid, \
             release_track_mbid, recording_mbid, disc_number, track_number, track_title, \
             file_format, duration_seconds, expected_duration_seconds, evidence_title, \
             evidence_artist, evidence_score, source, source_task_id, reason, reason_detail, \
             created_at FROM acquire_held_files";
    /// The open entry of user `?1` under key `?2`.
    pub(super) OPEN_ENTRY = "SELECT id FROM acquire_held_entries \
             WHERE user_id = ?1 AND group_key = ?2 AND state IN " + OPEN;
    /// A new open entry.
    pub(super) INSERT_ENTRY = "INSERT INTO acquire_held_entries (user_id, origin, group_key, \
             release_group_mbid, recording_mbid, drop_item_id, artist_name, album_title, state, \
             created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'held', ?9, ?10)";
    /// Push entry `?1`'s expiry out to `?2`, never earlier, and fill its
    /// card text where it has none. A file joined it, so its automatic
    /// retry starts again from the first try.
    pub(super) EXTEND_ENTRY = "UPDATE acquire_held_entries \
             SET expires_at = MAX(expires_at, ?2), retry_tries = 0, retry_at = NULL, \
                 release_group_mbid = COALESCE(release_group_mbid, ?3), \
                 artist_name = COALESCE(artist_name, ?4), album_title = COALESCE(album_title, ?5) \
             WHERE id = ?1";
    /// A held file of entry `?1` for release track `?2`, or with no track
    /// and the name `?3`.
    pub(super) SAME_FILE = "SELECT id FROM acquire_held_files \
             WHERE entry_id = ?1 AND state = 'held' \
               AND CASE WHEN ?2 IS NOT NULL THEN lower(release_track_mbid) = lower(?2) \
                   ELSE release_track_mbid IS NULL AND original_name = ?3 END \
             LIMIT 1";
    /// A new held file.
    pub(super) INSERT_FILE = "INSERT INTO acquire_held_files (entry_id, root, path, \
             original_name, release_mbid, release_track_mbid, recording_mbid, disc_number, \
             track_number, track_title, file_format, duration_seconds, \
             expected_duration_seconds, evidence_title, evidence_artist, evidence_score, \
             source, source_task_id, reason, reason_detail, state, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18, ?19, ?20, 'held', ?21)";
    /// Open entries of owner `?1` (NULL for everyone), album `?2` and
    /// download `?3` (each NULL for any), newest first.
    pub(super) LIST_ENTRIES = ENTRY_ROW + " e WHERE e.state IN " + OPEN + " \
             AND (?1 IS NULL OR e.user_id = ?1) \
             AND (?2 IS NULL OR e.release_group_mbid = lower(?2)) \
             AND (?3 IS NULL OR EXISTS (SELECT 1 FROM acquire_held_files f \
                 WHERE f.entry_id = e.id AND f.state = 'held' AND f.source_task_id = ?3)) \
             ORDER BY e.created_at DESC, e.id DESC";
    /// One entry, whatever its state.
    pub(super) ENTRY_BY_ID = ENTRY_ROW + " WHERE id = ?1";
    /// The held files of entry `?1`, in album order.
    pub(super) HELD_FILES_OF = FILE_ROW + " WHERE entry_id = ?1 AND state = 'held' \
             ORDER BY disc_number, track_number, original_name, id";
    /// One held file.
    pub(super) FILE_BY_ID = FILE_ROW + " WHERE id = ?1 AND state = 'held'";
    /// Claim open entry `?1` for an action.
    pub(super) CLAIM = "UPDATE acquire_held_entries SET state = 'placing' \
             WHERE id = ?1 AND state = 'held'";
    /// Give a claimed entry `?1` back.
    pub(super) RELEASE = "UPDATE acquire_held_entries SET state = 'held' \
             WHERE id = ?1 AND state = 'placing'";
    /// Resolve held file `?1` as `?2` at `?3`.
    pub(super) RESOLVE_FILE = "UPDATE acquire_held_files SET state = ?2, resolved_at = ?3 \
             WHERE id = ?1 AND state = 'held'";
    /// Entry `?1` is resolved at `?2` once none of its files is held.
    pub(super) SETTLE_ENTRY = "UPDATE acquire_held_entries SET state = 'resolved', \
             resolved_at = ?2 WHERE id = ?1 AND state IN " + OPEN + " \
             AND NOT EXISTS (SELECT 1 FROM acquire_held_files \
                 WHERE entry_id = ?1 AND state = 'held')";
    /// Keep held file `?1` for reason `?2` with detail `?3`.
    pub(super) REHOLD = "UPDATE acquire_held_files SET reason = ?2, reason_detail = ?3 \
             WHERE id = ?1 AND state = 'held'";
    /// Pair held file `?1` with a release track.
    pub(super) PAIR_FILE = "UPDATE acquire_held_files SET release_mbid = ?2, \
             release_track_mbid = ?3, recording_mbid = ?4, disc_number = ?5, \
             track_number = ?6, track_title = ?7, expected_duration_seconds = ?8 \
             WHERE id = ?1 AND state = 'held'";
    /// Whether download task `?1` still has a file held in an open entry.
    pub(super) TASK_HOLDS = "SELECT EXISTS (SELECT 1 FROM acquire_held_files f \
             JOIN acquire_held_entries e ON e.id = f.entry_id \
             WHERE f.source_task_id = ?1 AND f.state = 'held' AND e.state IN " + OPEN + ")";
    /// Tasks among a list that still have a file held in an open entry.
    family TASKS_HOLDING = tasks_holding;
    /// Open entries of owner `?1` (NULL for everyone).
    pub(super) COUNT_OPEN = "SELECT COUNT(*) FROM acquire_held_entries \
             WHERE state IN " + OPEN + " AND (?1 IS NULL OR user_id = ?1)";
    /// Download copies of album `?1` held at or before `?2`, in entries
    /// waiting for a person; an entry being acted on is left alone.
    pub(super) REMOVED_ALBUM_FILES = "SELECT f.id, f.entry_id, f.path \
             FROM acquire_held_files f JOIN acquire_held_entries e ON e.id = f.entry_id \
             WHERE e.origin = 'download' AND e.release_group_mbid = lower(?1) \
               AND e.state = 'held' AND f.root = 'held' AND f.state = 'held' \
               AND f.created_at <= ?2";
    /// Owner `?1`'s files held as only the album version by their other
    /// track downloads (not `?3`) of recording `?2`.
    pub(super) ALBUM_VERSION_FILES = FILE_ROW + " f WHERE f.state = 'held' \
             AND f.reason = 'album_version_only' \
             AND f.entry_id IN (SELECT id FROM acquire_held_entries \
                 WHERE user_id = ?1 AND state IN " + OPEN + ") \
             AND f.source_task_id IN (SELECT id FROM download_tasks \
                 WHERE user_id = ?1 AND download_type = 'track' \
                   AND recording_mbid = ?2 AND id != ?3)";
    /// Entries past their expiry at `?1`, oldest first, at most `?2`.
    pub(super) DUE_ENTRIES = "SELECT id FROM acquire_held_entries \
             WHERE state = 'held' AND expires_at <= ?1 ORDER BY expires_at, id LIMIT ?2";
    /// Claim entry `?1` past its expiry at `?2` for the prune.
    pub(super) EXPIRE_ENTRY = "UPDATE acquire_held_entries SET state = 'expired', \
             resolved_at = ?2 WHERE id = ?1 AND state = 'held' AND expires_at <= ?2";
    /// Every held file of entry `?1`, discarded at `?2`; answers where
    /// each copy is.
    pub(super) DISCARD_ALL_FILES = "UPDATE acquire_held_files SET state = 'discarded', \
             resolved_at = ?2 WHERE entry_id = ?1 AND state = 'held' RETURNING root, path";
    /// The reasons our side holds a file for, which it retries by itself
    /// (`Reason::kind` is `Library`).
    pub fragment LIBRARY_REASONS = "('target_occupied', 'upgrade_blocked', 'library_changed', \
             'library_unavailable', 'naming_script_failed', 'library_folder_missing', \
             'local_fault')";
    /// Held file `f` is retried by itself: held for a library reason and
    /// paired with a track, as the Retry button needs.
    pub(super) fragment RETRIES = "(f.state = 'held' AND f.reason IN " + LIBRARY_REASONS + " \
             AND (COALESCE(f.release_track_mbid, '') != '' \
                  OR COALESCE(f.recording_mbid, '') != ''))";
    /// Entries waiting for a person with a file retried by itself, due at
    /// `?1`, longest waiting first, at most `?2`: id and owner.
    pub(super) DUE_RETRIES = "SELECT e.id, e.user_id FROM acquire_held_entries e \
             WHERE e.state = 'held' AND (e.retry_at IS NULL OR e.retry_at <= ?1) \
               AND EXISTS (SELECT 1 FROM acquire_held_files f \
                   WHERE f.entry_id = e.id AND " + RETRIES + ") \
             ORDER BY COALESCE(e.retry_at, 0), e.id LIMIT ?2";
    /// Whether entry `?1` still has a file retried by itself, and its
    /// tries so far.
    pub(super) RETRY_STATE = "SELECT EXISTS (SELECT 1 FROM acquire_held_files f \
             WHERE f.entry_id = ?1 AND " + RETRIES + "), retry_tries \
             FROM acquire_held_entries WHERE id = ?1";
    /// Entry `?1` was retried `?2` times in a row; the next try is at `?3`
    /// (NULL with no tries: at the next pass).
    pub(super) SET_RETRY = "UPDATE acquire_held_entries SET retry_tries = ?2, retry_at = ?3 \
             WHERE id = ?1";
    /// Entries an action had claimed.
    pub(super) PLACING_ENTRIES = ENTRY_ROW + " WHERE state = 'placing' ORDER BY id";
    /// Every copy in the held folder an open entry still names.
    pub(super) HELD_FOLDER_PATHS = "SELECT f.path FROM acquire_held_files f \
             JOIN acquire_held_entries e ON e.id = f.entry_id \
             WHERE f.root = 'held' AND f.state = 'held' AND e.state IN " + OPEN;
    /// The open entry of drop item `?1`.
    pub(super) ENTRY_OF_ITEM = ENTRY_ROW + " WHERE drop_item_id = ?1 AND state IN " + OPEN;
    /// Add `?2` imported files to drop item `?1`.
    pub(super) ITEM_IMPORTED = "UPDATE drop_import_items \
             SET files_imported = files_imported + ?2 WHERE id = ?1";
    /// Whether drop item `?1` had a file the library already held.
    pub(super) fragment ITEM_OWNED = "EXISTS (SELECT 1 FROM acquire_held_files f \
             JOIN acquire_held_entries e ON e.id = f.entry_id \
             WHERE e.drop_item_id = ?1 AND f.state = 'already_owned')";
    /// Drop item `?1` whose entry is decided: imported when any of its
    /// files went in, skipped (sentence `?4`) when the library had them,
    /// else discarded (sentence `?2`). Only an item waiting for review
    /// moves.
    pub(super) SETTLE_ITEM = "UPDATE drop_import_items SET \
             status = CASE WHEN files_imported > 0 THEN 'imported' \
                 WHEN " + ITEM_OWNED + " THEN 'skipped' ELSE 'discarded' END, \
             detail = CASE WHEN files_imported > 0 THEN detail \
                 WHEN " + ITEM_OWNED + " THEN ?4 ELSE ?2 END, \
             reason_code = CASE WHEN files_imported = 0 AND " + ITEM_OWNED + " \
                 THEN 'already_owned' END, \
             staging_paths = '[]', updated_at = ?3 \
             WHERE id = ?1 AND status IN ('needs_review', 'processing')";
}

/// `?, ?, ...` for a list of `len` values.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// Tasks among the list that still have a file held in an open entry.
pub(super) fn tasks_holding(tasks: ListLen) -> String {
    format!(
        "SELECT DISTINCT f.source_task_id FROM acquire_held_files f \
         JOIN acquire_held_entries e ON e.id = f.entry_id \
         WHERE f.state = 'held' AND e.state IN {OPEN} AND f.source_task_id IN ({})",
        marks(tasks)
    )
}
