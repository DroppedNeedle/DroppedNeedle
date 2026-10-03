-- 0003_plugin_tick_state.sql - stage-10 plugin scheduler tick state.
--
-- v2 kept tick state in files under each plugin's directory: unregistered
-- durability that vanished with an uninstall and bypassed every backup.
-- v3 moves that state onto this table (D11), keyed by plugin name, so it
-- survives restarts and reinstalls and travels with the database.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS plugin_tick_state (
    plugin TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY (plugin, key)
);
CREATE INDEX IF NOT EXISTS idx_plugin_tick_state_plugin
    ON plugin_tick_state(plugin);

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 3;
