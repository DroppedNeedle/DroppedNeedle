-- Two indexes for lookups that grew with the rows already present.

-- idx_local_albums_retired holds only retired albums, the rows a lookup by
-- retired_into_album_id = ? (and a parent delete's foreign key check)
-- reads. Before statistics exist the planner took the full index as a key
-- for retired_into_album_id IS NULL, which nearly every album matches, and
-- walked every live album: once per file in a first scan.
DROP INDEX idx_local_albums_retired;
CREATE INDEX idx_local_albums_retired ON local_albums(retired_into_album_id)
    WHERE retired_into_album_id IS NOT NULL;

-- Whether a track moved in a run, read for each track that left an album
-- when the run ends; without it each read walked every move of the run.
CREATE INDEX idx_library_scan_album_moves_track
    ON library_scan_album_moves(run_id, local_track_id);

PRAGMA user_version = 137;
