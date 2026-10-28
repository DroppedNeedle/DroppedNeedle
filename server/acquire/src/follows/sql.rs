//! The follows' statements: follows, auto-download approvals and import
//! batches, and the follow poll's cursors and feed.

use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql! {
    // Follows.

    /// Which of several artists one user follows.
    family FOLLOWED_AMONG = followed_among;
    /// Whether user `?1` follows artist `?2`.
    pub(crate) IS_FOLLOWED = "SELECT 1 FROM user_followed_artists \
         WHERE user_id = ?1 AND artist_mbid_lower = ?2";
    /// The followed artist `?2` of user `?1`: `(mbid, name)`.
    pub(crate) FOLLOWED_ARTIST = "SELECT artist_mbid, artist_name FROM user_followed_artists \
         WHERE user_id = ?1 AND artist_mbid_lower = ?2";
    /// Unfollow one artist.
    pub(crate) UNFOLLOW = "DELETE FROM user_followed_artists \
         WHERE user_id = ?1 AND artist_mbid_lower = ?2";
    /// Follow one artist; a conflict keeps the intent and start date and
    /// refreshes the name when one is given.
    pub(crate) UPSERT_FOLLOW = "INSERT INTO user_followed_artists (user_id, artist_mbid, \
         artist_mbid_lower, artist_name, auto_download, followed_at, updated_at) \
         VALUES (?1, ?2, ?3, COALESCE(?4, ?5), 0, ?6, ?6) \
         ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
         artist_name = COALESCE(?4, artist_name), updated_at = ?6";
    /// Set the auto-download intent `?1` on one followed row.
    pub(crate) SET_INTENT = "UPDATE user_followed_artists SET auto_download = ?1, \
         updated_at = ?2 WHERE user_id = ?3 AND artist_mbid_lower = ?4";
    /// Follow with auto-download on, or turn it on for an existing follow.
    pub(crate) FOLLOW_ARMED = "INSERT INTO user_followed_artists (user_id, artist_mbid, \
         artist_mbid_lower, artist_name, auto_download, followed_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5) \
         ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
         auto_download = 1, updated_at = ?5";

    // Auto-download approvals and import batches.

    /// File (or refresh) one pending ask; a row in state `?7` stays.
    pub(crate) FILE_APPROVAL = "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
         artist_mbid_lower, artist_name, state, requested_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
         artist_name = excluded.artist_name, state = excluded.state, \
         requested_at = excluded.requested_at, reviewed_by_id = NULL, \
         reviewed_by_name = NULL, reviewed_at = NULL, batch_id = NULL \
         WHERE auto_download_approvals.state <> ?7";
    /// File one batch member; a row in state `?9` stays.
    pub(crate) FILE_BATCH_MEMBER = "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
         artist_mbid_lower, artist_name, state, requested_at, batch_id, source) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (user_id, artist_mbid_lower) DO UPDATE SET \
         artist_name = excluded.artist_name, state = excluded.state, \
         requested_at = excluded.requested_at, reviewed_by_id = NULL, \
         reviewed_by_name = NULL, reviewed_at = NULL, \
         batch_id = excluded.batch_id, source = excluded.source \
         WHERE auto_download_approvals.state != ?9";
    /// Withdraw user `?1`'s ask for artist `?2` while in state `?3`.
    pub(crate) WITHDRAW_APPROVAL = "DELETE FROM auto_download_approvals WHERE user_id = ?1 \
         AND artist_mbid_lower = ?2 AND state = ?3";
    /// One ask in state `?3`: `(mbid, name, user name, requested at)`.
    pub(crate) APPROVAL_IN_STATE = "SELECT a.artist_mbid, a.artist_name, u.display_name, \
         a.requested_at FROM auto_download_approvals a LEFT JOIN auth_users u ON u.id = a.user_id \
         WHERE a.user_id = ?1 AND a.artist_mbid_lower = ?2 AND a.state = ?3";
    /// Move one ask to state `?3`, stamping the reviewer.
    pub(crate) DECIDE_APPROVAL = "UPDATE auto_download_approvals SET state = ?3, \
         reviewed_by_id = ?4, reviewed_by_name = ?5, reviewed_at = ?6 \
         WHERE user_id = ?1 AND artist_mbid_lower = ?2";
    /// One batch's rows in state `?2`: `(user id, mbid, name)`.
    pub(crate) BATCH_ROWS = "SELECT user_id, artist_mbid, artist_name \
         FROM auto_download_approvals WHERE batch_id = ?1 AND state = ?2 ORDER BY artist_name";
    /// Move one batch's rows in state `?6` to `?2`, stamping the reviewer.
    pub(crate) DECIDE_BATCH = "UPDATE auto_download_approvals SET state = ?2, \
         reviewed_by_id = ?3, reviewed_by_name = ?4, reviewed_at = ?5 \
         WHERE batch_id = ?1 AND state = ?6";
    /// Pending units: single asks plus one per pending batch.
    pub(crate) PENDING_UNITS = "SELECT (SELECT COUNT(*) FROM auto_download_approvals \
         WHERE state = 'pending' AND batch_id IS NULL) + \
         (SELECT COUNT(*) FROM (SELECT batch_id, user_id FROM auto_download_approvals \
         WHERE state = 'pending' AND batch_id IS NOT NULL GROUP BY batch_id, user_id))";
    /// Pending singles (`?1` false) or batch members (`?1` true), oldest
    /// first: `(user id, user name, mbid, name, requested at, batch id)`.
    pub(crate) PENDING_APPROVALS = "SELECT a.user_id, u.display_name, a.artist_mbid, \
         a.artist_name, a.requested_at, a.batch_id FROM auto_download_approvals a \
         LEFT JOIN auth_users u ON u.id = a.user_id \
         WHERE a.state = 'pending' AND (a.batch_id IS NOT NULL) = ?1 \
         ORDER BY a.requested_at ASC, a.artist_name ASC";

    // The follow poll.

    /// Followed artists due at `?1`, never-polled first, at most `?2`.
    pub(crate) CURSORS_DUE = "SELECT f.artist_mbid_lower, MIN(f.artist_mbid), \
         MIN(f.artist_name), MAX(c.baselined), MAX(c.cursor_date), MAX(c.next_poll_at), \
         MAX(c.pending) \
         FROM user_followed_artists f \
         LEFT JOIN acquire_follow_cursors c ON c.artist_mbid_lower = f.artist_mbid_lower \
         GROUP BY f.artist_mbid_lower \
         HAVING MAX(c.next_poll_at) IS NULL OR MAX(c.next_poll_at) <= ?1 \
         ORDER BY COALESCE(MAX(c.next_poll_at), 0), f.artist_mbid_lower LIMIT ?2";
    /// One polled artist's cursor, in the shape of [`CURSORS_DUE`].
    pub(crate) CURSOR = "SELECT c.artist_mbid_lower, c.artist_mbid_lower, '', c.baselined, \
         c.cursor_date, c.next_poll_at, c.pending FROM acquire_follow_cursors c \
         WHERE c.artist_mbid_lower = ?1";
    /// One artist's known release groups.
    pub(crate) KNOWN_RELEASES = "SELECT rg_mbid_lower FROM artist_known_releases \
         WHERE artist_mbid_lower = ?1";
    /// Followers of artist `?1` with auto-download on:
    /// `(user id, role, verdict, name)`.
    pub(crate) AUTO_FOLLOWERS = "SELECT f.user_id, u.role, a.state, u.display_name \
         FROM user_followed_artists f \
         JOIN auth_users u ON u.id = f.user_id \
         LEFT JOIN auto_download_approvals a \
           ON a.user_id = f.user_id AND a.artist_mbid_lower = f.artist_mbid_lower \
         WHERE f.artist_mbid_lower = ?1 AND f.auto_download = 1 \
         ORDER BY f.user_id";
    /// Store one cursor.
    pub(crate) PUT_CURSOR = "INSERT OR REPLACE INTO acquire_follow_cursors (artist_mbid_lower, \
         baselined, cursor_date, next_poll_at, pending) \
         VALUES (?1, ?2, ?3, ?4, ?5)";
    /// Remember one known release group.
    pub(crate) ADD_KNOWN_RELEASE = "INSERT OR IGNORE INTO artist_known_releases \
         (artist_mbid_lower, rg_mbid_lower) VALUES (?1, ?2)";
    /// Add one release to the followers' feed; a known one keeps its time.
    pub(crate) ADD_FEED_RELEASE = "INSERT OR IGNORE INTO new_release_feed \
         (release_group_mbid_lower, release_group_mbid, artist_mbid_lower, artist_name, title, \
         primary_type, secondary_types, first_release_date, discovered_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
}

/// Which of `artists` artists one user (the first value) follows.
pub(crate) fn followed_among(artists: ListLen) -> String {
    format!(
        "SELECT artist_mbid_lower FROM user_followed_artists \
         WHERE user_id = ? AND artist_mbid_lower IN ({})",
        vec!["?"; artists.get()].join(", ")
    )
}
