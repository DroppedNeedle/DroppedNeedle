-- 0074_operation_work_bundle_id.sql - a work item's publish bundle is a
-- column.
--
--   library_operation_work.bundle_id (new, TEXT, nullable)
--       the publish bundle a management work item writes under. It was a
--       `bundle_id` key inside result_json, read back with json_extract by
--       undo, the results page and recovery. Every row whose result_json
--       holds a text `bundle_id` gets it copied here; every other row stays
--       NULL. The key is left in old result_json values and no longer
--       written.
--
--   idx_library_operation_work_bundle_id (new)
--       finds the work item of a bundle; only rows with a bundle are
--       indexed.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_operation_work ADD COLUMN bundle_id TEXT;

UPDATE library_operation_work
SET bundle_id = json_extract(result_json, '$.bundle_id')
WHERE CASE WHEN json_valid(result_json)
           THEN json_type(result_json, '$.bundle_id') = 'text'
           ELSE 0 END;

CREATE INDEX IF NOT EXISTS idx_library_operation_work_bundle_id
    ON library_operation_work(bundle_id) WHERE bundle_id IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 74;
