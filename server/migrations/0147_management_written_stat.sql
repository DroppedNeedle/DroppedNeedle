-- The stat revision (size and modified time, spelled as a scan spells
-- it) of each track's file as a Library Management profile run last
-- placed it from the album's match, over files as the last scan read
-- them; NULL after any other write (a tag edit, an Undo, a restore, an
-- import, a run over a file changed since its scan). A scan that finds a
-- file changed to exactly that stat, while the app's record is still the
-- bytes it placed, has met the app's own write and does not send the
-- album back to identification. NULL on every carried row, so those
-- files are offered as before.

ALTER TABLE library_track_management_state ADD COLUMN written_stat_revision TEXT;

PRAGMA user_version = 147;
