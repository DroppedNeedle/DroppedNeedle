-- 0014_serving_read_perf.sql - sorted track pages and planner statistics.
--
-- - local_tracks(year, id): Jellyfin year sorts walk the index instead of
--   sorting the whole catalog.
-- - db_planner_stats: when ANALYZE last ran and over how many streamable
--   tracks, so statistics refresh after a scan moves the catalog and at
--   least daily, across restarts.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE INDEX IF NOT EXISTS idx_local_tracks_year_id
    ON local_tracks(year, id);

CREATE TABLE IF NOT EXISTS db_planner_stats (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    analyzed_at INTEGER NOT NULL,
    indexed_tracks INTEGER NOT NULL
);

-- Migration high-water mark.
PRAGMA user_version = 14;
