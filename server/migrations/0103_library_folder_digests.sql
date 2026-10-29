-- 0103_library_folder_digests.sql - what the folder watcher last saw.
--
-- The watcher walks each library folder as the scan does and keeps one
-- digest per folder, so it asks for a scan of just the folders that
-- changed, and a restart does not make it forget what it saw. One row per
-- folder:
--
--   root_id        the library folder's root
--   relative_path  the folder in the root, `.` for the root itself, as on
--                  disk (not NFC-normalized: it names the folder to scan)
--   digest         a hash of the names, sizes and modification times of
--                  the files directly in the folder
--   audio_files    how many of those files are music files
--
-- Rows are written when the watcher first sees a root, and after a scan it
-- asked for has settled. Losing them costs nothing but the watcher taking
-- the tree as it finds it, as at a first start. Root ids live in settings,
-- so there is no foreign key.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_folder_digests (
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL CHECK (relative_path != ''),
    digest INTEGER NOT NULL,
    audio_files INTEGER NOT NULL CHECK (audio_files >= 0),
    PRIMARY KEY (root_id, relative_path)
) STRICT, WITHOUT ROWID;

-- Migration high-water mark.
PRAGMA user_version = 103;
