-- Each recycle bin path the app has marked as its bin, by the trimmed
-- path Library Management names. The bin's folder holds the marker file
-- `.droppedneedle-recycle-bin` from then on: a path with a row whose
-- folder lacks the marker (a share not mounted leaves an empty folder)
-- is not the bin, and files wait for it. Rows stay when the path
-- changes, so going back to an old path finds its marker again.

CREATE TABLE library_recycle_bin_marks (
    path      TEXT    NOT NULL PRIMARY KEY,
    marked_at INTEGER NOT NULL
) STRICT;

PRAGMA user_version = 145;
