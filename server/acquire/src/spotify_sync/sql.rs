//! The statements of Spotify sync: one row per synced playlist in
//! `acquire_playlist_syncs`, and the reads of the playlists and owners
//! they belong to.

use super::models::{SyncFailure, SyncHold};
use crate::requests::ledger::ApprovalState;

droppedneedle_kernel::sql! {
    /// Playlist `?1`'s sync row.
    pub(super) SYNC_ROW = "SELECT enabled, whole_albums, snapshot_id, read_at, checked_at, \
         next_check_at, failures, failure, hold, grant_state \
         FROM acquire_playlist_syncs WHERE playlist_id = ?1";
    /// One playlist's sync, as stored.
    pub(super) row SyncColumns for [SYNC_ROW] {
        enabled: bool,
        whole_albums: bool,
        snapshot_id: Option<String>,
        read_at: Option<i64>,
        checked_at: Option<i64>,
        next_check_at: i64,
        failures: i64,
        failure: Option<SyncFailure>,
        hold: Option<SyncHold>,
        grant_state: Option<ApprovalState>,
    }
    /// Turn playlist `?1`'s sync on at `?3`, asking for whole albums when
    /// `?2`, due at once.
    pub(super) INSERT_SYNC = "INSERT INTO acquire_playlist_syncs (playlist_id, enabled, \
         whole_albums, enabled_at, next_check_at) VALUES (?1, 1, ?2, ?3, ?3)";
    /// Turn playlist `?1`'s sync on again at `?3`, due at once, with whole
    /// albums `?2`. What the last pass before it was turned off met is
    /// forgotten.
    pub(super) TURN_ON = "UPDATE acquire_playlist_syncs SET enabled = 1, whole_albums = ?2, \
         enabled_at = ?3, next_check_at = ?3, failure = NULL, failures = 0, hold = NULL \
         WHERE playlist_id = ?1";
    /// Ask for whole albums on playlist `?1` when `?2`, sync left as it is.
    pub(super) SET_WHOLE_ALBUMS = "UPDATE acquire_playlist_syncs SET whole_albums = ?2 \
         WHERE playlist_id = ?1";
    /// Turn playlist `?1`'s sync off; its grant stays.
    pub(super) TURN_OFF = "UPDATE acquire_playlist_syncs SET enabled = 0 WHERE playlist_id = ?1";
    /// Ask an admin for playlist `?1`'s grant at `?2`.
    pub(super) ASK_GRANT = "UPDATE acquire_playlist_syncs SET grant_state = 'pending', \
         grant_requested_at = ?2, grant_reviewed_by_id = NULL, grant_reviewed_by_name = NULL, \
         grant_reviewed_at = NULL WHERE playlist_id = ?1";
    /// The next pass of playlist `?1` is due at `?2`.
    pub(super) SET_NEXT_CHECK = "UPDATE acquire_playlist_syncs SET next_check_at = ?2 \
         WHERE playlist_id = ?1";
    /// Playlist `?1`'s songs were read in full at `?3`, at Spotify's
    /// version `?2`.
    pub(super) SET_READ = "UPDATE acquire_playlist_syncs SET snapshot_id = ?2, read_at = ?3 \
         WHERE playlist_id = ?1";
    /// Playlist `?1`'s pass finished at `?2`, its requests waiting for
    /// `?3` when set.
    pub(super) FINISH_PASS = "UPDATE acquire_playlist_syncs SET checked_at = ?2, failures = 0, \
         failure = NULL, hold = ?3 WHERE playlist_id = ?1";
    /// Playlist `?1`'s pass failed with `?2`; the next is due at `?3`.
    pub(super) FAIL_PASS = "UPDATE acquire_playlist_syncs SET failure = ?2, \
         failures = failures + 1, next_check_at = ?3 WHERE playlist_id = ?1";
    /// The synced playlists due by `?1`, soonest first.
    pub(super) DUE = "SELECT playlist_id FROM acquire_playlist_syncs \
         WHERE enabled = 1 AND next_check_at <= ?1 ORDER BY next_check_at, playlist_id";
    /// The Spotify sources of user `?1`'s synced playlists.
    pub(super) SYNCED_SOURCES = "SELECT p.source_ref FROM acquire_playlist_syncs s \
         JOIN catalog_playlists p ON p.id = s.playlist_id \
         WHERE p.user_id = ?1 AND s.enabled = 1 AND p.source_ref LIKE 'spotify:%'";
    /// Make user `?1`'s syncs that failed for want of a working link due at
    /// `?2`, answering their playlists.
    pub(super) WAKE_UNLINKED = "UPDATE acquire_playlist_syncs SET next_check_at = ?2 \
         WHERE enabled = 1 AND failure IN ('not_linked', 'needs_reconnect') \
         AND playlist_id IN (SELECT id FROM catalog_playlists WHERE user_id = ?1) \
         RETURNING playlist_id";
    /// The role and request name of user `?1`.
    pub(super) OWNER = "SELECT role, COALESCE(username_display, username) AS name \
         FROM auth_users WHERE id = ?1";
    /// A playlist owner's role and the name their requests show.
    pub(super) row OwnerColumns for [OWNER] {
        role: String,
        name: Option<String>,
    }
}
