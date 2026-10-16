-- 0051_publish_operation_author.sql - who applied each published operation.
--
-- A curator may undo an operation they applied themselves; an admin may
-- undo any. The account that applied a manual preview is recorded on its
-- operation row. Automatic work, imports, restorations and every operation
-- recorded before this migration keep NULL, so only an admin can undo
-- them. Deleting the account clears the column, which leaves its
-- operations to an admin.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_publish_operations
    ADD COLUMN applied_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_library_publish_operations_applied_by_user_id
    ON library_publish_operations(applied_by_user_id);

-- Migration high-water mark.
PRAGMA user_version = 51;
