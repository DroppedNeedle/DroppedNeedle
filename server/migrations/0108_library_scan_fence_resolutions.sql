-- 0108_library_scan_fence_resolutions.sql - closing a write moves the fence.
--
-- A write that needs attention holds its tracks back from a scan's gone
-- judgement until an administrator marks it handled. Marking it handled
-- changes what a scan may judge, so it moves the scan fence revision as a
-- journal write does: a scope whose walk began before the mark still
-- keeps the write's tracks, and the next scope judges them as they are.
--
--   trg_library_publish_resolution_fence (new)
--       on a new library_publish_resolutions row, moves
--       library_scan_fences.revision and stamps it on the bundle's journal
--       rows that name a track.
--   idx_library_publish_journal_fence (new)
--       the journal rows by fence revision, so a scan reads the rows
--       written after its walk began without reading the whole journal.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TRIGGER IF NOT EXISTS trg_library_publish_resolution_fence
AFTER INSERT ON library_publish_resolutions
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE bundle_id = NEW.bundle_id
          AND (track_id IS NOT NULL OR kind = 'remove');
END;

CREATE INDEX IF NOT EXISTS idx_library_publish_journal_fence
    ON library_publish_journal(fence_revision) WHERE fence_revision IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 108;
