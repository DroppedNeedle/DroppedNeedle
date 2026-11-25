//! The statements of the backup checks, which read a backup file
//! read-only, and the support bundle's counts.

droppedneedle_kernel::sql! {
    /// Rows that break a foreign key.
    pub(super) FOREIGN_KEY_VIOLATIONS = "SELECT COUNT(*) FROM pragma_foreign_key_check";
    /// The support bundle's library and held-file counts.
    pub(super) BUNDLE_COUNTS = "SELECT (SELECT COUNT(*) FROM local_artists) AS artists, \
         (SELECT COUNT(*) FROM local_albums) AS albums, \
         (SELECT COUNT(*) FROM local_tracks) AS tracks, \
         (SELECT COUNT(*) FROM catalog_playlists) AS playlists, \
         (SELECT COUNT(*) FROM acquire_held_entries) AS held_entries";
    /// Accounts by role.
    pub(super) BUNDLE_USERS_BY_ROLE = "SELECT role AS name, COUNT(*) AS total FROM auth_users GROUP BY role ORDER BY role";
    /// Requests by state.
    pub(super) BUNDLE_REQUESTS_BY_STATE = "SELECT status AS name, COUNT(*) AS total FROM request_history GROUP BY status ORDER BY status";
    /// The support bundle's counts.
    pub(super) row BundleCountsRow for [BUNDLE_COUNTS] {
        artists: i64,
        albums: i64,
        tracks: i64,
        playlists: i64,
        held_entries: i64,
    }
    /// One name and how many rows hold it.
    pub(super) row GroupCountRow for [BUNDLE_USERS_BY_ROLE, BUNDLE_REQUESTS_BY_STATE] {
        name: String,
        total: i64,
    }
}
