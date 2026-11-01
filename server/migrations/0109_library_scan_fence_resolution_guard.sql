-- 0109_library_scan_fence_resolution_guard.sql - mark only writes that fence.
--
-- Marking a write handled moved the scan fence revision even when the
-- write had no journal row a scan reads (only files added with no track),
-- a change no scan can see. It now moves only for a write it stamps.
--
--   trg_library_publish_resolution_fence (replaced)
--       as in 0108, now only when the bundle has a journal row that names
--       a track or removes a file.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TRIGGER IF EXISTS trg_library_publish_resolution_fence;

CREATE TRIGGER trg_library_publish_resolution_fence
AFTER INSERT ON library_publish_resolutions
WHEN EXISTS (
    SELECT 1 FROM library_publish_journal
    WHERE bundle_id = NEW.bundle_id
      AND (track_id IS NOT NULL OR kind = 'remove')
)
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE bundle_id = NEW.bundle_id
          AND (track_id IS NOT NULL OR kind = 'remove');
END;

-- Migration high-water mark.
PRAGMA user_version = 109;
