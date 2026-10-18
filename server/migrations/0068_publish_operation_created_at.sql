-- 0068_publish_operation_created_at.sql - when each managed write was made.
--
-- A managed write that keeps failing is listed as needing a look once it
-- has gone unsettled for 24 hours. The operation row kept only the day it
-- was made, which made that 24 to 48 hours. It now keeps the time, in
-- unix milliseconds, set when the write is recorded, before anything is
-- staged. A healthy write settles in seconds, so a write still unsettled
-- has been failing since about then.
--
-- Existing rows take the start of their day, so none waits longer than it
-- did before. created_day stays for its other readers.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_publish_operations
    ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;

UPDATE library_publish_operations SET created_at = created_day * 86400000;

-- Migration high-water mark.
PRAGMA user_version = 68;
