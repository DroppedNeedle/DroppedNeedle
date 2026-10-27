//! The statements of the Spotify import: authorize states
//! (`spotify_auth_requests`), links (`user_connections`) and the source
//! refs of imported playlists (`playlists`).

droppedneedle_kernel::sql! {
    /// Drop the states that expired by `?1`.
    pub(super) PRUNE_STATES = "DELETE FROM spotify_auth_requests WHERE expires_at <= ?1";
    /// A new authorize state.
    pub(super) INSERT_STATE = "INSERT INTO spotify_auth_requests \
             (state, user_id, redirect_uri, expires_at) VALUES (?1, ?2, ?3, ?4)";
    /// The user, redirect URI and expiry of state `?1`.
    pub(super) STATE_BY_KEY = "SELECT user_id, redirect_uri, expires_at \
             FROM spotify_auth_requests WHERE state = ?1";
    /// Drop state `?1`.
    pub(super) DELETE_STATE = "DELETE FROM spotify_auth_requests WHERE state = ?1";
    /// Insert or replace user `?1`'s link to service `?2`, sealed as `?3`.
    pub(super) UPSERT_LINK = "INSERT INTO user_connections \
             (user_id, service, connection_data, enabled, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 1, ?4, ?4) \
             ON CONFLICT (user_id, service) DO UPDATE SET \
             connection_data = excluded.connection_data, enabled = 1, \
             updated_at = excluded.updated_at";
    /// Replace the sealed document of user `?1`'s link to service `?2`.
    pub(super) UPDATE_LINK = "UPDATE user_connections SET connection_data = ?3, \
             updated_at = ?4 WHERE user_id = ?1 AND service = ?2";
    /// The sealed document of user `?1`'s enabled link to service `?2`.
    pub(super) LINK_DOCUMENT = "SELECT connection_data FROM user_connections \
             WHERE user_id = ?1 AND service = ?2 AND enabled = 1";
    /// Drop user `?1`'s link to service `?2`.
    pub(super) DELETE_LINK = "DELETE FROM user_connections WHERE user_id = ?1 AND service = ?2";
    /// User `?1`'s playlists imported from Spotify, as `(source_ref, id)`.
    pub(super) SPOTIFY_SOURCE_REFS = "SELECT source_ref, id FROM playlists \
             WHERE user_id = ?1 AND source_ref LIKE 'spotify:%'";
}
