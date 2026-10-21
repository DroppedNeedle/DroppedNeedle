-- 0077_plugin_library_writes.sql - what each plugin wrote to the library.
--
--   plugin_library_writes (new, STRICT)
--       one row per file a plugin's `host.library.write` recorded, written
--       in the same transaction as the change's operation row, so a write
--       that stopped before it recorded anything leaves no row.
--       bundle_id   the recorded change (`library_publish_operations`).
--       plugin      the plugin's name.
--       bytes       the file's size.
--       written_at  unix milliseconds when the change was recorded.
--       A plugin's daily allowance counts its rows of the last 24 hours
--       whose change was not undone by its unwind; older rows are deleted
--       as new ones are written.
--
--   idx_plugin_library_writes_plugin (new)
--       the rows one plugin's allowance reads, by time.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS plugin_library_writes (
    bundle_id  TEXT    PRIMARY KEY,
    plugin     TEXT    NOT NULL,
    bytes      INTEGER NOT NULL CHECK (bytes >= 0),
    written_at INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS idx_plugin_library_writes_plugin
    ON plugin_library_writes(plugin, written_at);

-- Migration high-water mark.
PRAGMA user_version = 77;
