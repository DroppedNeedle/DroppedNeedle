-- 0132_catalog_event_start_text.sql - a concert's start is the listing's own
-- text, so its column no longer reads as a point in time.
--
--   catalog_live_event_feed.starts_at becomes start_text, unchanged: the
--   start as the source wrote it (Ticketmaster's absolute time, Skiddle's
--   venue-local one), relayed as given and never compared. A point in time
--   is integer milliseconds in a column ending _at; this is not one.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE catalog_live_event_feed RENAME COLUMN starts_at TO start_text;

-- Migration high-water mark.
PRAGMA user_version = 132;
