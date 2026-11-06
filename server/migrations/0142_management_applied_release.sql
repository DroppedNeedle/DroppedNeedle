-- The release and release track each managed file was written from. A
-- match taken off and chosen again starts its row revision again at 1, so
-- the revisions alone cannot tell a file written from another release:
-- these ids can. NULL on every carried row, which never counts as the
-- same release, so those files are read again once.

ALTER TABLE library_track_management_identity ADD COLUMN applied_release_mbid TEXT;
ALTER TABLE library_track_management_identity ADD COLUMN applied_release_track_mbid TEXT;

PRAGMA user_version = 142;
