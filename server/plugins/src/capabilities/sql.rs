//! The statements of the plugin library write allowance.

droppedneedle_kernel::sql! {
    /// Plugin `?1`'s writes after a time `?2`, in milliseconds.
    pub(super) PLUGIN_WRITES_SINCE = "SELECT bundle_id, bytes FROM plugin_library_writes \
        WHERE plugin = ?1 AND written_at > ?2";
    /// Drop the writes made by a time, in milliseconds.
    pub(super) DELETE_WRITES_BEFORE = "DELETE FROM plugin_library_writes \
        WHERE written_at <= ?1";
    /// Record one plugin write.
    pub(super) INSERT_WRITE = "INSERT INTO plugin_library_writes \
        (bundle_id, plugin, bytes, written_at) VALUES (?1, ?2, ?3, ?4)";

    /// One write of the day: its recorded change and its size.
    pub(super) row PluginWrite for [PLUGIN_WRITES_SINCE] {
        bundle_id: String,
        bytes: i64,
    }
}
