//! The statements of the Spotify import: authorize states
//! (`spotify_auth_requests`), links (`user_connections`) and the source
//! refs of imported playlists (`catalog_playlists`).

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
    /// The sealed document of user `?1`'s enabled link to service `?2`.
    pub(super) LINK_DOCUMENT = "SELECT connection_data FROM user_connections \
             WHERE user_id = ?1 AND service = ?2 AND enabled = 1";
    /// User `?1`'s playlists imported from Spotify, as `(source_ref, id)`.
    pub(super) SPOTIFY_SOURCE_REFS = "SELECT source_ref, id FROM catalog_playlists \
             WHERE user_id = ?1 AND source_ref LIKE 'spotify:%'";
}
