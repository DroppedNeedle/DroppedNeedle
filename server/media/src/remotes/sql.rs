//! The per-user links' and Navidrome folder preferences' statements.

droppedneedle_kernel::sql! {
    /// The `user_connections` columns a `ConnectionRow` is read from.
    fragment CONNECTION_COLUMNS = "SELECT service, enabled, connection_data FROM user_connections";
    /// One user's link to one service.
    pub(super) CONNECTION = CONNECTION_COLUMNS + " WHERE user_id = ?1 AND service = ?2";
    /// Every link of one user, by service.
    pub(super) CONNECTIONS = CONNECTION_COLUMNS + " WHERE user_id = ?1 ORDER BY service";
    /// Store or replace one user's link to one service, enabled.
    pub(super) SAVE_CONNECTION = "INSERT INTO user_connections \
         (user_id, service, connection_data, enabled, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 1, ?4, ?4) \
         ON CONFLICT (user_id, service) DO UPDATE SET \
         connection_data = excluded.connection_data, enabled = 1, \
         updated_at = excluded.updated_at";
    /// Remove one user's link to one service.
    pub(super) DELETE_CONNECTION = "DELETE FROM user_connections \
         WHERE user_id = ?1 AND service = ?2";
    /// Remove every link holding a value sealed under the data key. A
    /// document sealed whole is walked as an empty array, because
    /// `json_tree` fails the whole statement on text that is not JSON.
    pub(super) FORGET_SEALED = "DELETE FROM user_connections \
         WHERE connection_data LIKE 'v3:%' OR EXISTS ( \
         SELECT 1 FROM json_tree(CASE WHEN json_valid(user_connections.connection_data) \
         THEN user_connections.connection_data ELSE '[]' END) AS tree \
         WHERE tree.type = 'text' AND tree.value LIKE 'v3:%')";
    /// One user's Navidrome folder preference.
    pub(super) FOLDER_PREFERENCE = "SELECT mode, selected_ids_json, server_identity \
         FROM user_navidrome_folder_preferences WHERE user_id = ?1";
    /// Store or replace one user's Navidrome folder preference.
    pub(super) SAVE_FOLDER_PREFERENCE = "INSERT INTO user_navidrome_folder_preferences \
         (user_id, mode, selected_ids_json, server_identity, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id) DO UPDATE SET mode = excluded.mode, \
         selected_ids_json = excluded.selected_ids_json, \
         server_identity = excluded.server_identity, \
         updated_at = excluded.updated_at";
}
