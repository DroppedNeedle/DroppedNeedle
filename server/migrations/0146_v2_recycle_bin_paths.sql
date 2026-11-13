-- Each recycle bin path a v2 install used, by the trimmed path Library
-- Management names, recorded by the upgrade. Until such a path is marked,
-- its folder is taken for the bin only while it holds an entry folder: an
-- empty one may be a share that is not mounted, so files wait for it or
-- for an admin to accept the folder.

CREATE TABLE library_recycle_bin_v2_paths (
    path       TEXT    NOT NULL PRIMARY KEY,
    carried_at INTEGER NOT NULL
) STRICT;

PRAGMA user_version = 146;
