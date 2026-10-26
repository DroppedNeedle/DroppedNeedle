-- 0086_library_scan_fence.sql - the fence between scans and library writes.
--
-- A scan judges a track gone when its walk finished without seeing the
-- file. A publish that moved the file after the walk passed it, or that is
-- still open, must not count. Every write that names a track moves one
-- library-wide revision and records it on its journal row, so a scan can
-- tell which tracks were touched after its walk began.
--
--   library_scan_fences (new, STRICT)
--       one row holding the revision every journal write that names a
--       track moves.
--   library_publish_journal.fence_revision (new)
--       the revision this row last moved to. NULL on rows written before
--       this migration.
--   library_scan_run_scopes.walk_revision (new)
--       the revision read just before the scope's walk began. NULL for a
--       scope walked before this migration, which never counts as walked
--       past.
--
-- A removal's row names its track through library_publish_replacements,
-- so its rows move the revision too. A plugin's added file names no track
-- and moves nothing. The scan's own writes have no journal rows.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_scan_fences (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision  INTEGER NOT NULL
) STRICT;

INSERT INTO library_scan_fences (singleton, revision) VALUES (1, 0);

ALTER TABLE library_publish_journal ADD COLUMN fence_revision INTEGER;

ALTER TABLE library_scan_run_scopes ADD COLUMN walk_revision INTEGER;

CREATE INDEX IF NOT EXISTS idx_library_publish_journal_track
    ON library_publish_journal(track_id) WHERE track_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_library_publish_replacements_track
    ON library_publish_replacements(replaced_track_id)
    WHERE replaced_track_id IS NOT NULL;

CREATE TRIGGER IF NOT EXISTS trg_library_publish_journal_fence_insert
AFTER INSERT ON library_publish_journal
WHEN NEW.track_id IS NOT NULL OR NEW.kind = 'remove'
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS trg_library_publish_journal_fence_state
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.state IS NOT OLD.state AND (NEW.track_id IS NOT NULL OR NEW.kind = 'remove')
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE id = NEW.id;
END;

-- Migration high-water mark.
PRAGMA user_version = 86;
