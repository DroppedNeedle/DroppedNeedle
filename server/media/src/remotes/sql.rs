//! The per-user links' and Navidrome folder preferences' statements.

droppedneedle_kernel::sql! {
    /// The `user_connections` columns a `ConnectionRow` is read from.
    fragment CONNECTION_COLUMNS = "SELECT service, enabled, connection_data FROM user_connections";
    /// One user's link to one service.
    pub(super) CONNECTION = CONNECTION_COLUMNS + " WHERE user_id = ?1 AND service = ?2";
    /// Every link of one user, by service.
    pub(super) CONNECTIONS = CONNECTION_COLUMNS + " WHERE user_id = ?1 ORDER BY service";
    /// One user's Navidrome folder preference.
    pub(super) FOLDER_PREFERENCE = "SELECT mode, selected_ids_json, server_identity \
         FROM media_navidrome_folder_preferences WHERE user_id = ?1";
    /// Store or replace one user's Navidrome folder preference.
    pub(super) SAVE_FOLDER_PREFERENCE = "INSERT INTO media_navidrome_folder_preferences \
         (user_id, mode, selected_ids_json, server_identity, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id) DO UPDATE SET mode = excluded.mode, \
         selected_ids_json = excluded.selected_ids_json, \
         server_identity = excluded.server_identity, \
         updated_at = excluded.updated_at";
}
