-- 0105_library_leftover_checks.sql - the leftover-file report, kept.
--
-- The leftover check looks through each library folder for hidden working
-- files that no publish journal row names. Its last finding per library
-- folder is kept here, so the report survives a restart and a folder that
-- could not be read keeps what was last found in it. One row per root:
--
--   root_id     the library folder's root
--   checked_at  when the folder was last looked through, unix ms
--   file_count  how many leftover files that look found
--   files_json  a JSON array of the first of them, as paths under the root
--
-- Losing the rows costs nothing but the report being empty until the next
-- check. Root ids live in settings, so there is no foreign key; the check
-- removes the row of a root that is no longer checked.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_leftover_checks (
    root_id TEXT PRIMARY KEY,
    checked_at INTEGER NOT NULL,
    file_count INTEGER NOT NULL CHECK (file_count >= 0),
    files_json TEXT NOT NULL CHECK (json_valid(files_json) AND json_type(files_json) = 'array')
) STRICT, WITHOUT ROWID;

-- Migration high-water mark.
PRAGMA user_version = 105;
