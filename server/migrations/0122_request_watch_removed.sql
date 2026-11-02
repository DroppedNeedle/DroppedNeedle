-- 0122_request_watch_removed.sql - a removed wanted watch stays removed.
--
--   request_history.watch_removed_generation (new column)
--       The row's generation when a person removed the album's watch. A
--       failed or short request is enrolled as a watch again unless this
--       still matches: any later write to the row, such as a new request or
--       retry, moves the generation and makes the album watchable again.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE request_history ADD COLUMN watch_removed_generation INTEGER;

PRAGMA user_version = 122;
