-- 0082_library_publish_created_folders.sql - folders a publish created.
--
--   library_publish_created_folders (new, STRICT)
--       one row per folder a publish made for its files, written right
--       after the folder is made and before anything goes into it. An
--       unwind removes these folders once they are empty, deepest first;
--       the bundle's catalog commit deletes its rows.
--       bundle_id   the publish (`library_publish_operations`).
--       root_id     the library folder it is under.
--       folder_rel  its path below that folder.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_publish_created_folders (
    bundle_id  TEXT NOT NULL
        REFERENCES library_publish_operations(bundle_id) ON DELETE CASCADE,
    root_id    TEXT NOT NULL,
    folder_rel TEXT NOT NULL CHECK (folder_rel <> ''),
    PRIMARY KEY (bundle_id, root_id, folder_rel)
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 82;
