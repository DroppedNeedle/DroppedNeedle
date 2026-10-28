//! The requests area's statements: the request ledger, its listeners and
//! hides, the wanted watches, the edition marks, the personal-mix
//! approvals, and what requests read from the library and the accounts.
//! The first group moves a request with its download: settling rows when
//! their task finishes or their album reaches the library, linking rows to
//! a new download, and the sweep that finds rows left behind.

use droppedneedle_kernel::db::statement::ListLen;

use super::sqlite::HistoryOrder;
use crate::landing::sql::OWNED_TRACKS_FROM;

droppedneedle_kernel::sql! {
    /// Settle the rows linked to one task (`?1`) as `?2`, stamping
    /// `completed_at` with `?3` unless it is NULL. Live rows always match;
    /// with `?4` set, failed and incomplete rows match too (a completed
    /// task imports them).
    pub(crate) SETTLE_TASK_ROWS = "UPDATE request_history SET status = ?2, \
             completed_at = COALESCE(?3, completed_at), generation = generation + 1 \
         WHERE download_task_id = ?1 \
           AND (status IN ('pending', 'queued', 'downloading') \
                OR (?4 AND status IN ('failed', 'incomplete'))) \
         RETURNING musicbrainz_id_lower, request_kind, user_id, track_release_group_mbid";
    /// An album row (`?1`, lowercased) whose album the library holds:
    /// imported at `?2` unless a person decided it or it is mid-cancel.
    pub(crate) SETTLE_ALBUM_ROW = "UPDATE request_history SET status = 'imported', \
             completed_at = ?2, generation = generation + 1 \
         WHERE musicbrainz_id_lower = ?1 AND request_kind = 'album' \
           AND status IN ('pending', 'queued', 'downloading', 'awaiting_approval', \
                          'failed', 'incomplete') \
         RETURNING musicbrainz_id_lower, request_kind, user_id, track_release_group_mbid";
    /// What a settled task says about its album: `(download_type,
    /// release_group_mbid)`.
    pub(crate) TASK_ALBUM = "SELECT download_type, release_group_mbid FROM download_tasks \
         WHERE id = ?1";
    /// The album's watch (`?1`) is fulfilled, with outcome `?2` at `?3`.
    pub(crate) FULFIL_WATCH = "UPDATE wanted_watches SET state = 'fulfilled', \
             last_outcome = ?2, last_checked_at = ?3, new_candidate_count = 0 \
         WHERE release_group_mbid_lower = lower(?1)";
    /// Link one row (`?1`) to a new download (`?2`) behind the optional
    /// generation guard `?3`. Only a row a download may serve moves; it
    /// then reads downloading.
    pub(crate) LINK_ROW = "UPDATE request_history SET download_task_id = ?2, \
             status = 'downloading', completed_at = NULL, generation = generation + 1 \
         WHERE musicbrainz_id_lower = ?1 AND (?3 IS NULL OR generation = ?3) \
           AND status IN ('pending', 'queued', 'downloading', 'failed', 'incomplete', \
                          'cancelled')";
    /// Move the rows that followed task `?1` onto its retry `?2`.
    pub(crate) RELINK_ROWS = "UPDATE request_history SET download_task_id = ?2, \
             status = 'downloading', completed_at = NULL, generation = generation + 1 \
         WHERE download_task_id = ?1 \
           AND status IN ('pending', 'queued', 'downloading', 'failed', 'incomplete', \
                          'cancelled') \
         RETURNING musicbrainz_id_lower";
    /// The request a task (`?1`) serves (`?2`), the link's other end.
    pub(crate) MARK_TASK_REQUEST = "UPDATE download_tasks SET request_history_mbid = ?2 \
         WHERE id = ?1";
    /// Finished tasks whose linked rows did not follow them:
    /// `(task id, task status)`.
    pub(crate) UNSETTLED_TASKS = "SELECT DISTINCT dt.id, dt.status FROM request_history rh \
         JOIN download_tasks dt ON dt.id = rh.download_task_id \
         WHERE dt.status IN ('completed', 'partial', 'failed', 'cancelled') \
           AND (rh.status IN ('pending', 'queued', 'downloading') \
                OR (dt.status = 'completed' AND rh.status IN ('failed', 'incomplete')))";
    /// Album rows only the library can settle: open rows with no task left
    /// to follow, and failed rows whose task is gone.
    pub(crate) ALBUMS_WITHOUT_TASK = "SELECT rh.musicbrainz_id_lower FROM request_history rh \
         WHERE rh.request_kind = 'album' \
           AND NOT EXISTS (SELECT 1 FROM download_tasks dt WHERE dt.id = rh.download_task_id) \
           AND (rh.status IN ('pending', 'queued', 'downloading', 'awaiting_approval') \
                OR (rh.status = 'failed' AND rh.download_task_id IS NOT NULL))";

    // The request ledger.

    /// Columns every record read selects, in decode order.
    fragment RECORD_SELECT = "SELECT rh.musicbrainz_id_lower, rh.request_kind, \
         rh.status, rh.artist_name, rh.album_title, rh.artist_mbid, rh.year, rh.release_mbid, \
         rh.track_title, rh.duration_seconds, rh.track_release_group_mbid, rh.user_id, \
         rh.requested_by_name, rh.requested_at, rh.completed_at, rh.download_task_id, \
         rh.generation, rh.dispatch_authorized, rh.monitor_artist, rh.auto_download_artist, \
         rh.reviewed_by_name, rh.reviewed_at, rh.origin \
         FROM request_history rh";
    /// Per-user filter: with `?1` NULL every row passes; otherwise the
    /// row's owner or a listener is `?1` and that user has not hidden it.
    fragment MEMBER_FILTER = "(?1 IS NULL OR ((rh.user_id = ?1 OR EXISTS (SELECT 1 FROM \
         request_history_requesters rr WHERE rr.musicbrainz_id_lower = rh.musicbrainz_id_lower \
         AND rr.user_id = ?1)) AND NOT EXISTS (SELECT 1 FROM request_history_dismissals d \
         WHERE d.musicbrainz_id_lower = rh.musicbrainz_id_lower AND d.user_id = ?1)))";
    /// History filter: kind `?2` and status `?3` when set, for member `?1`.
    fragment HISTORY_FILTER = " WHERE (?2 IS NULL OR rh.request_kind = ?2) \
         AND (?3 IS NULL OR rh.status = ?3) AND " + MEMBER_FILTER;
    /// One row by its key (`?1`).
    pub(crate) RECORD_BY_KEY = RECORD_SELECT + " WHERE rh.musicbrainz_id_lower = ?1";
    /// One row's listeners, the owner included.
    pub(crate) ROW_LISTENERS = "SELECT user_id, requested_by_name \
         FROM request_history_requesters \
         WHERE musicbrainz_id_lower = ?1 ORDER BY requested_at, user_id";
    /// The listeners of several rows: `(key, user id, name)`.
    family LISTENERS_IN = listeners_in;
    /// Asks listener `?1` made since `?2` (ms). A row still waiting for
    /// quota room has used none yet.
    pub(crate) COUNT_ASKS = "SELECT COUNT(*) FROM request_history_requesters rr \
         JOIN request_history rh ON rh.musicbrainz_id_lower = rr.musicbrainz_id_lower \
         WHERE rr.user_id = ?1 AND rr.requested_at >= ?2 \
         AND NOT (rh.status = 'queued' AND rh.download_task_id IS NULL)";
    /// Whether user `?2` owns or listens to row `?1`.
    pub(crate) IS_MEMBER = "SELECT EXISTS (SELECT 1 FROM request_history \
         WHERE musicbrainz_id_lower = ?1 AND user_id = ?2) \
         OR EXISTS (SELECT 1 FROM request_history_requesters \
         WHERE musicbrainz_id_lower = ?1 AND user_id = ?2)";
    /// One row's `(status, generation)`.
    pub(crate) ROW_STATE = "SELECT status, generation FROM request_history \
         WHERE musicbrainz_id_lower = ?1";
    /// One row's status.
    pub(crate) ROW_STATUS = "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1";
    /// Whether row `?1` exists.
    pub(crate) ROW_EXISTS = "SELECT EXISTS (SELECT 1 FROM request_history \
         WHERE musicbrainz_id_lower = ?1)";
    /// Drop one row; its listeners and hides go with it.
    pub(crate) DELETE_ROW = "DELETE FROM request_history WHERE musicbrainz_id_lower = ?1";
    /// Store one new row.
    pub(crate) INSERT_ROW = "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, \
         artist_name, album_title, artist_mbid, year, cover_url, requested_at, completed_at, \
         status, monitor_artist, auto_download_artist, user_id, requested_by_name, \
         reviewed_by_id, reviewed_by_name, reviewed_at, download_task_id, release_mbid, \
         request_kind, track_title, duration_seconds, track_release_group_mbid, \
         dispatch_authorized, generation, origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, \
         ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, \
         ?24)";
    /// The owner as the new row's first listener.
    pub(crate) INSERT_OWNER = "INSERT INTO request_history_requesters (user_id, \
         musicbrainz_id_lower, requested_at, requested_by_name) VALUES (?1, ?2, ?3, ?4)";
    /// Attach listener `?1` to row `?2`, keeping a known name.
    pub(crate) ATTACH_LISTENER = "INSERT INTO request_history_requesters (user_id, \
         musicbrainz_id_lower, requested_at, requested_by_name) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (user_id, musicbrainz_id_lower) DO UPDATE SET \
         requested_by_name = COALESCE(excluded.requested_by_name, \
         request_history_requesters.requested_by_name)";
    /// Detach listener `?2` from row `?1`.
    pub(crate) DETACH_LISTENER = "DELETE FROM request_history_requesters \
         WHERE musicbrainz_id_lower = ?1 AND user_id = ?2";
    /// Keys with a live row of kind `?1`.
    pub(crate) ACTIVE_KEYS = "SELECT musicbrainz_id_lower FROM request_history \
         WHERE request_kind = ?1 \
         AND status IN ('pending','downloading','queued','awaiting_approval')";
    /// Start row `?1` that waits for quota room.
    pub(crate) START_WAITING = "UPDATE request_history SET status = 'pending', \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
         AND status = 'queued' AND download_task_id IS NULL";
    /// The asks of `?2` and of row `?1`'s owner count from `?3`.
    pub(crate) RESTAMP_ASKS = "UPDATE request_history_requesters SET requested_at = ?3 \
         WHERE musicbrainz_id_lower = ?1 AND (user_id = ?2 OR user_id = \
         (SELECT user_id FROM request_history WHERE musicbrainz_id_lower = ?1))";
    /// Put row `?1` back to waiting behind generation `?2`.
    pub(crate) REQUEUE_WAITING = "UPDATE request_history SET status = 'queued', \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
         AND status = 'pending' AND download_task_id IS NULL AND generation = ?2";
    /// Rows waiting for quota room, oldest first.
    pub(crate) WAITING_FOR_QUOTA = RECORD_SELECT
        + " WHERE rh.status = 'queued' AND rh.download_task_id IS NULL \
         ORDER BY rh.requested_at ASC, rh.musicbrainz_id_lower";
    /// One account's role.
    pub(crate) ROLE_OF = "SELECT role FROM auth_users WHERE id = ?1";
    /// Widen row `?1`'s monitoring flags, never narrowing them.
    pub(crate) WIDEN_MONITORING = "UPDATE request_history SET monitor_artist = 1, \
         auto_download_artist = ?2 WHERE musicbrainz_id_lower = ?1 AND monitor_artist = 0";
    /// Switch row `?1` to release `?2` at generation `?3` while no download
    /// serves it: it waits for approval or for quota room. A pending row
    /// with no task is being dispatched, so it keeps the release it has.
    pub(crate) SWITCH_RELEASE = "UPDATE request_history SET release_mbid = ?2 \
         WHERE musicbrainz_id_lower = ?1 AND generation = ?3 \
           AND download_task_id IS NULL AND status IN ('queued', 'awaiting_approval')";
    /// Move row `?1` to status `?2` behind the optional generation guard `?4`.
    pub(crate) UPDATE_STATUS = "UPDATE request_history SET status = ?2, completed_at = ?3, \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
         AND (?4 IS NULL OR generation = ?4)";
    /// Approve row `?1` from status `?5` at generation `?6`.
    pub(crate) CLAIM_APPROVAL = "UPDATE request_history SET status = ?2, \
         dispatch_authorized = 1, reviewed_by_name = ?3, reviewed_at = ?4, \
         download_task_id = NULL, generation = generation + 1 \
         WHERE musicbrainz_id_lower = ?1 AND status = ?5 AND generation = ?6";
    /// Reject row `?1` from status `?5` at generation `?6`.
    pub(crate) CLAIM_REJECTION = "UPDATE request_history SET status = ?2, \
         dispatch_authorized = 0, reviewed_by_name = ?3, reviewed_at = ?4, completed_at = ?4, \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
         AND status = ?5 AND generation = ?6";
    /// Start row `?1` again as `?2`.
    pub(crate) CLAIM_RETRY = "UPDATE request_history SET status = ?2, dispatch_authorized = ?3, \
         requested_at = ?4, completed_at = NULL, download_task_id = NULL, \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1";
    /// Cancel row `?1` outright as `?2` at `?3`.
    pub(crate) CANCEL_ROW = "UPDATE request_history SET status = ?2, completed_at = ?3, \
         dispatch_authorized = 0, generation = generation + 1 \
         WHERE musicbrainz_id_lower = ?1";
    /// Move row `?1` to status `?2`, nothing else.
    pub(crate) SET_STATUS = "UPDATE request_history SET status = ?2, \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1";
    /// Restore row `?1` to `?2` while it sits in `?3` at generation `?4`.
    pub(crate) RESTORE_STATUS = "UPDATE request_history SET status = ?2, completed_at = NULL, \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1 \
         AND status = ?3 AND generation = ?4";
    /// Revoke or restore row `?1`'s dispatch capability.
    pub(crate) SET_DISPATCH_AUTHORIZED = "UPDATE request_history SET dispatch_authorized = ?2, \
         generation = generation + 1 WHERE musicbrainz_id_lower = ?1";
    /// Hide row `?2` from user `?1`.
    pub(crate) DISMISS_ROW = "INSERT OR IGNORE INTO request_history_dismissals \
         (user_id, musicbrainz_id_lower, dismissed_at) VALUES (?1, ?2, ?3)";
    /// Live rows, newest first, for member `?1` and kind `?2` when set.
    pub(crate) ACTIVE_ROWS = RECORD_SELECT
        + " WHERE rh.status IN ('pending','downloading','queued','awaiting_approval') \
         AND (?2 IS NULL OR rh.request_kind = ?2) AND "
        + MEMBER_FILTER
        + " ORDER BY rh.requested_at DESC, rh.musicbrainz_id_lower";
    /// How many history rows the filter passes.
    pub(crate) HISTORY_COUNT = "SELECT COUNT(*) FROM request_history rh" + HISTORY_FILTER;
    /// One history page, one statement per sort.
    family HISTORY_PAGE = history_page;
    /// Rows in status `?1`, oldest first.
    pub(crate) ROWS_IN_STATUS = RECORD_SELECT
        + " WHERE rh.status = ?1 ORDER BY rh.requested_at ASC, rh.musicbrainz_id_lower";
    /// Rows in status `?1` of kind `?2`, oldest first.
    pub(crate) ROWS_IN_STATUS_OF_KIND = RECORD_SELECT
        + " WHERE rh.status = ?1 AND rh.request_kind = ?2 \
         ORDER BY rh.requested_at ASC, rh.musicbrainz_id_lower";
    /// Terminal rows settled before `?1` that no live watch points at.
    pub(crate) PRUNE_TERMINAL = "DELETE FROM request_history \
         WHERE status IN ('imported', 'incomplete', 'failed', 'cancelled', 'rejected') \
           AND COALESCE(completed_at, requested_at) < ?1 \
           AND musicbrainz_id_lower NOT IN (SELECT release_group_mbid_lower \
               FROM wanted_watches WHERE state = 'watching')";

    // Wanted watches.

    /// Columns every watch read selects, in decode order.
    fragment WATCH_SELECT = "SELECT w.release_group_mbid_lower, w.user_id, u.display_name, \
         w.artist_name, w.album_title, w.artist_mbid, w.year, w.cover_url, w.kind, w.state, \
         w.created_at, w.first_release_date, w.check_count, w.quiet_streak, w.next_check_at, \
         w.new_candidate_count FROM wanted_watches w LEFT JOIN auth_users u ON u.id = w.user_id";
    /// One watch by its key.
    pub(crate) WATCH_BY_KEY = WATCH_SELECT + " WHERE w.release_group_mbid_lower = ?1";
    /// Watches of user `?2`, or every watch when `?1`, newest first.
    pub(crate) WATCHES_FOR = WATCH_SELECT
        + " WHERE ?1 OR w.user_id = ?2 ORDER BY w.created_at DESC, w.release_group_mbid_lower";
    /// Watches in state `?1` due at `?2`, oldest due first, at most `?3`.
    pub(crate) WATCHES_DUE = WATCH_SELECT
        + " WHERE w.state = ?1 AND w.next_check_at <= ?2 \
         ORDER BY w.next_check_at, w.release_group_mbid_lower LIMIT ?3";
    /// Stopped and fulfilled watches last touched before `?1`.
    pub(crate) PRUNE_WATCHES = "DELETE FROM wanted_watches \
         WHERE state IN ('stopped', 'fulfilled') \
         AND COALESCE(last_checked_at, created_at) < ?1";
    /// Enrol a new watch; an existing one is left alone.
    pub(crate) ENROL_WATCH = "INSERT INTO wanted_watches (release_group_mbid_lower, \
         release_group_mbid, user_id, artist_name, album_title, artist_mbid, year, cover_url, \
         kind, state, created_at, first_release_date, next_check_at) \
         VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT (release_group_mbid_lower) DO NOTHING";
    /// Re-arm a watch in state `?7` with fresh counters.
    pub(crate) REARM_WATCH = "UPDATE wanted_watches SET state = ?2, user_id = ?3, kind = ?4, \
         created_at = ?5, check_count = 0, quiet_streak = 0, last_checked_at = NULL, \
         next_check_at = ?6, last_outcome = NULL, new_candidate_count = 0, \
         first_release_date = COALESCE(?8, first_release_date) \
         WHERE release_group_mbid_lower = ?1 AND state = ?7";
    /// Record one check; a count of new candidates (`?7`) replaces the
    /// badge, none keeps it.
    pub(crate) RECORD_CHECK = "UPDATE wanted_watches SET check_count = check_count + 1, \
         quiet_streak = ?2, next_check_at = ?3, last_checked_at = ?4, \
         last_outcome = ?5, first_release_date = COALESCE(?6, first_release_date), \
         new_candidate_count = COALESCE(?7, new_candidate_count) \
         WHERE release_group_mbid_lower = ?1";
    /// The candidates watch `?1` has seen: `(source, identity)`.
    pub(crate) SEEN_CANDIDATES = "SELECT source, identity FROM wanted_seen_candidates \
         WHERE release_group_mbid_lower = ?1";
    /// Mark watch `?1` as `?2` with outcome `?3` at `?4`.
    pub(crate) MARK_FULFILLED = "UPDATE wanted_watches SET state = ?2, last_outcome = ?3, \
         last_checked_at = ?4, new_candidate_count = 0 WHERE release_group_mbid_lower = ?1";
    /// Move watch `?1` from state `?3` to `?2`.
    pub(crate) MOVE_WATCH = "UPDATE wanted_watches SET state = ?2 \
         WHERE release_group_mbid_lower = ?1 AND state = ?3";
    /// Make watch `?1` due at `?2`.
    pub(crate) WATCH_DUE_AT = "UPDATE wanted_watches SET next_check_at = ?2 \
         WHERE release_group_mbid_lower = ?1";
    /// Resume watch `?1` from state `?4` as `?2`, due and armed at `?3`.
    pub(crate) RESUME_WATCH = "UPDATE wanted_watches SET state = ?2, next_check_at = ?3, \
         created_at = ?3 WHERE release_group_mbid_lower = ?1 AND state = ?4";
    /// Remember one turned-down candidate.
    pub(crate) ADD_SEEN = "INSERT INTO wanted_seen_candidates (release_group_mbid_lower, \
         source, identity, first_seen_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING";
    /// Clear watch `?1`'s unseen candidates.
    pub(crate) CLEAR_UNSEEN = "UPDATE wanted_watches SET new_candidate_count = 0 \
         WHERE release_group_mbid_lower = ?1";
    /// Stop watch `?1` in state `?3`, armed at or before `?4`, as `?2`.
    pub(crate) STOP_AFTER_REMOVAL = "UPDATE wanted_watches SET state = ?2 \
         WHERE release_group_mbid_lower = ?1 AND state = ?3 AND created_at <= ?4";
    /// Re-arm watch `?1` in state `?4`, armed at or before `?5`, due `?3`.
    pub(crate) REARM_AFTER_REMOVAL = "UPDATE wanted_watches SET state = ?2, created_at = ?3, \
         check_count = 0, quiet_streak = 0, last_checked_at = NULL, next_check_at = ?3, \
         last_outcome = NULL, new_candidate_count = 0 \
         WHERE release_group_mbid_lower = ?1 AND state = ?4 AND created_at <= ?5";

    // Edition marks.

    /// One in-flight edition acquire.
    pub(crate) EDITION_MARK = "SELECT release_group_mbid_lower, task_id, started_at \
         FROM acquire_edition_acquires WHERE release_group_mbid_lower = ?1";
    /// Set one mark.
    pub(crate) SET_EDITION_MARK = "INSERT OR REPLACE INTO acquire_edition_acquires \
         (release_group_mbid_lower, task_id, started_at) VALUES (?1, ?2, ?3)";

    // Personal-mix approvals.

    /// File (or refresh) one pending approval.
    pub(crate) FILE_MIX_APPROVAL = "INSERT INTO personal_mix_approvals \
         (user_id, state, requested_at) VALUES (?1, ?2, ?3) ON CONFLICT (user_id) DO UPDATE SET \
         state = excluded.state, requested_at = excluded.requested_at, \
         reviewed_by_id = NULL, reviewed_by_name = NULL, reviewed_at = NULL";
    /// One user's approval state.
    pub(crate) MIX_APPROVAL_STATE = "SELECT state FROM personal_mix_approvals \
         WHERE user_id = ?1";
    /// Pending approvals, oldest first.
    pub(crate) PENDING_MIX_APPROVALS = "SELECT m.user_id, u.display_name, m.state, \
         m.requested_at FROM personal_mix_approvals m LEFT JOIN auth_users u ON u.id = m.user_id \
         WHERE m.state = 'pending' ORDER BY m.requested_at ASC, m.user_id";
    /// Move user `?1`'s approval from `?6` to `?2`.
    pub(crate) MIX_APPROVAL_TRANSITION = "UPDATE personal_mix_approvals SET state = ?2, \
         reviewed_by_id = ?3, reviewed_by_name = ?4, reviewed_at = ?5 \
         WHERE user_id = ?1 AND state = ?6";

    // What requests read from the library and the accounts.

    /// Indexed bytes in the whole library.
    pub(crate) LIBRARY_BYTES = "SELECT COALESCE(SUM(file_size_bytes), 0) FROM local_tracks \
         WHERE availability = 'indexed'";
    /// Indexed bytes landed by user `?1`'s download tasks. CROSS JOIN keeps
    /// the user's tasks as the outer loop, so each task reaches its tracks
    /// through idx_local_tracks_download_task; a plain JOIN can start from
    /// every track instead.
    pub(crate) USER_BYTES = "SELECT COALESCE(SUM(t.file_size_bytes), 0) FROM download_tasks d \
         CROSS JOIN local_tracks t \
         WHERE t.download_task_id = d.id AND d.user_id = ?1 AND t.availability = 'indexed'";
    /// Owned tracks of several release groups, by recording.
    family MIX_LIBRARY_FILES = mix_library_files;
    /// A mix owner's role and the name their requests show.
    pub(crate) MIX_OWNER = "SELECT role, COALESCE(username_display, username) FROM auth_users \
         WHERE id = ?1";
    /// Users with a live ListenBrainz link.
    pub(crate) LISTENBRAINZ_USERS = "SELECT user_id FROM user_connections \
         WHERE service = 'listenbrainz' AND enabled = 1 ORDER BY user_id";
    /// A download already working on release group `?1`.
    pub(crate) GROUP_DOWNLOADING = "SELECT id FROM download_tasks \
         WHERE lower(release_group_mbid) = ?1 \
         AND status IN ('queued', 'downloading', 'processing') LIMIT 1";
    /// The library copy of group `?1` a fetch with no plan is for: album
    /// `?2` followed to the album it was merged into, else the group's copy
    /// with an edition, a person's first, then the oldest.
    pub(crate) PLANNED_COPY = "SELECT e.local_album_id, e.release_mbid \
         FROM local_album_external_identities e \
         JOIN local_albums b ON b.id = e.local_album_id \
         WHERE b.retired_into_album_id IS NULL AND e.provider = 'musicbrainz' \
           AND lower(e.release_group_mbid) = ?1 AND trim(e.release_mbid) <> '' \
           AND (?2 IS NULL OR e.local_album_id = (SELECT COALESCE(retired_into_album_id, id) \
             FROM local_albums WHERE id = ?2)) \
         ORDER BY e.decision_source <> 'manual', b.created_at LIMIT 1";
    /// Whether the app leaves album `?1`'s files as they are.
    pub(crate) COPY_IS_HELD = "SELECT EXISTS(SELECT 1 FROM library_management_exclusions \
         WHERE local_album_id = ?1) \
         OR EXISTS(SELECT 1 FROM library_album_takeovers WHERE local_album_id = ?1) \
         OR EXISTS(SELECT 1 FROM library_management_automatic_queue \
           WHERE local_album_id = ?1 AND retag_followed_at IS NOT NULL)";
    /// Whether the library holds release group `?1` (lowercased).
    pub(crate) LIBRARY_HOLDS = "SELECT EXISTS (SELECT 1 " + OWNED_TRACKS_FROM + ")";
}

/// `?, ?, ...` for a list of `len` values.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// The listeners of `rows` request rows: `(key, user id, name)`.
pub(crate) fn listeners_in(rows: ListLen) -> String {
    format!(
        "SELECT musicbrainz_id_lower, user_id, requested_by_name \
         FROM request_history_requesters WHERE musicbrainz_id_lower IN ({}) \
         ORDER BY requested_at, user_id",
        marks(rows)
    )
}

/// One history page in `order`: member `?1`, kind `?2`, status `?3`,
/// limit `?4`, offset `?5`.
pub(crate) fn history_page(order: HistoryOrder) -> String {
    let order = match order {
        HistoryOrder::Oldest => "rh.requested_at ASC",
        HistoryOrder::Status => "rh.status ASC, rh.requested_at DESC",
        HistoryOrder::Newest => "rh.requested_at DESC",
    };
    format!(
        "{RECORD_SELECT}{HISTORY_FILTER} ORDER BY {order}, rh.musicbrainz_id_lower \
         LIMIT ?4 OFFSET ?5"
    )
}

/// Owned tracks of `groups` lowercased release groups:
/// `(track id, group, recording, track number, disc number)`. The `+`
/// keeps the planner off the availability index, so tracks are reached
/// by their album from the group's identity rows.
pub(crate) fn mix_library_files(groups: ListLen) -> String {
    format!(
        "SELECT t.id, lower(ae.release_group_mbid), lower(te.recording_mbid), \
         t.track_number, t.disc_number FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id \
         JOIN local_album_external_identities ae ON ae.local_album_id = b.id \
         JOIN local_track_external_identities te \
         ON te.local_track_id = t.id AND te.provider = 'musicbrainz' \
         WHERE b.retired_into_album_id IS NULL AND +t.availability = 'indexed' \
         AND te.recording_mbid IS NOT NULL AND lower(ae.release_group_mbid) IN ({})",
        marks(groups)
    )
}
