-- 0071_config_sections.sql - runtime settings move into the database.
--
--   config_sections (new)
--       one row per settings section (and the instance id): its JSON
--       value, a revision that starts at 1 and goes up by one with each
--       change, and when the row last changed, in unix milliseconds
--
--   config_ingests (new)
--       at most one row: what happened to the config.json a server found
--       the first time it started on this database. 'ingested' copied its
--       sections in; 'skipped_new_key' left it unread because the key file
--       was minted by that same start, so it was not a file this server
--       had written. Holds the file's SHA-256 and, as JSON arrays, the
--       keys copied and the keys left behind.
--
-- Nothing is carried over here. The server copies config.json in once, at
-- its first start on this database, and never changes the file.
--
-- No down migration. Rollback is restoring a pre-upgrade backup, which has
-- neither table, so config.json is copied in again on the next start.

CREATE TABLE config_sections (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE config_ingests (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    outcome TEXT NOT NULL CHECK (outcome IN ('ingested', 'skipped_new_key')),
    file_sha256 TEXT NOT NULL,
    ingested_at INTEGER NOT NULL,
    keys_copied TEXT NOT NULL CHECK (json_valid(keys_copied)),
    keys_left TEXT NOT NULL CHECK (json_valid(keys_left))
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 71;
