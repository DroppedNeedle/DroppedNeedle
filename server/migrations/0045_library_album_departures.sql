-- 0045_library_album_departures.sql - tracks that left an album, kept past
-- the scan that saw them go.
--
-- A scan's own record of the tracks that changed albums is deleted with the
-- scan, and the history keeps only the last fifty scans. An album retagged
-- in two sittings is seen by two scans, so the second one needs to know
-- which tracks left the old album in the first. One row per track and the
-- album it left, written when a scan ends:
--
--   local_track_id     the track
--   previous_album_id  the album it left
--   departed_at        when the scan that saw it leave began
--   arrived_at         when the scan that saw it reach the album it sits
--                      on now began; it moves on with the track
--
-- A row goes when its track is back on that album, and with the album when
-- the album is merged into another or deleted. Both ids cascade, so a row
-- never keeps a track or an album from being deleted. Times are
-- milliseconds.
--
-- Two repairs ride along:
--   - an album merged into an album that was later merged itself now
--     points at the album the chain ends on, so the first id still opens
--     the album it became. A chain that loops is left as it is.
--   - pairs of album rows recorded as `mostly_missing` or
--     `new_files_added` are deleted: the scan now joins both cases itself,
--     and no released version wrote them.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_album_departures (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    previous_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    departed_at INTEGER NOT NULL,
    arrived_at INTEGER NOT NULL,
    PRIMARY KEY (local_track_id, previous_album_id)
) STRICT;

CREATE INDEX idx_library_album_departures_album
    ON library_album_departures(previous_album_id);

WITH RECURSIVE walk(start, at) AS (
    SELECT id, retired_into_album_id FROM local_albums
    WHERE retired_into_album_id IS NOT NULL
    UNION
    SELECT walk.start, next.retired_into_album_id FROM walk
    JOIN local_albums next ON next.id = walk.at
    WHERE next.retired_into_album_id IS NOT NULL
),
ends AS (
    SELECT walk.start, walk.at FROM walk
    JOIN local_albums last ON last.id = walk.at
    WHERE last.retired_into_album_id IS NULL
)
UPDATE local_albums
SET retired_into_album_id = (SELECT ends.at FROM ends WHERE ends.start = local_albums.id)
WHERE id IN (SELECT start FROM ends)
  AND retired_into_album_id <> (SELECT ends.at FROM ends WHERE ends.start = local_albums.id);

DELETE FROM library_retag_pairs WHERE reason IN ('mostly_missing', 'new_files_added');

-- Migration high-water mark.
PRAGMA user_version = 45;
