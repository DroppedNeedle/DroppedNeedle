-- 0099_acquire_discovery_batches.sql - discovery batches move to acquire,
-- which files their requests, rebuilt to the schema conventions.
--
--   acquire_discovery_batches (from discovery_batches)
--       one "Download all" a user pressed. The id is the create's
--       idempotency key. Carried, with created_at in integer
--       milliseconds. Batches of users who no longer exist go.
--   acquire_discovery_batch_items (from discovery_batch_items)
--       what the batch did with each album. Carried, with added_at in
--       integer milliseconds. Items whose batch is gone, or whose outcome
--       is outside the known set, go.
--
-- A time that does not read as ISO 8601 carries as 0. A batch and its
-- items go with their user, and the items with their batch.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE acquire_discovery_batches (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    source_section TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_acquire_discovery_batches_user
    ON acquire_discovery_batches(user_id, created_at DESC);

CREATE TABLE acquire_discovery_batch_items (
    batch_id TEXT NOT NULL REFERENCES acquire_discovery_batches(id) ON DELETE CASCADE,
    release_group_mbid TEXT NOT NULL,
    artist_mbid TEXT NOT NULL DEFAULT '',
    album_name TEXT NOT NULL DEFAULT '',
    artist_name TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL CHECK (outcome IN
        ('requested', 'skipped_in_library', 'skipped_duplicate', 'not_requested')),
    added_at INTEGER NOT NULL,
    PRIMARY KEY (batch_id, release_group_mbid)
) STRICT;

INSERT INTO acquire_discovery_batches (id, user_id, name, source_section, created_at)
SELECT id, user_id, name, source_section,
    COALESCE(CAST(round((julianday(created_at) - 2440587.5) * 86400000.0) AS INTEGER), 0)
FROM discovery_batches
WHERE user_id IN (SELECT id FROM auth_users);

INSERT INTO acquire_discovery_batch_items (batch_id, release_group_mbid, artist_mbid,
    album_name, artist_name, outcome, added_at)
SELECT batch_id, release_group_mbid, artist_mbid, album_name, artist_name, outcome,
    COALESCE(CAST(round((julianday(added_at) - 2440587.5) * 86400000.0) AS INTEGER), 0)
FROM discovery_batch_items
WHERE batch_id IN (SELECT id FROM acquire_discovery_batches)
  AND outcome IN ('requested', 'skipped_in_library', 'skipped_duplicate', 'not_requested');

DROP TABLE discovery_batch_items;
DROP TABLE discovery_batches;

-- Migration high-water mark.
PRAGMA user_version = 99;
