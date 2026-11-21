-- Each library track's loudness, measured once by the library's loudness
-- loop from the file's audio. A row counts only while its stat revision is
-- the track's: a changed file is measured again. `blocks` is the track's
-- 400 ms loudness blocks summed into 1 LU buckets, which an album's
-- loudness is worked out from when it is asked for. A file that cannot be
-- decoded is kept as unmeasurable, with no peak and no blocks.

CREATE TABLE library_loudness_measurements (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    stat_revision TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('measured', 'unmeasurable')),
    peak REAL,
    blocks BLOB,
    measured_at INTEGER NOT NULL,
    CHECK ((outcome = 'measured' AND peak IS NOT NULL AND blocks IS NOT NULL)
        OR (outcome = 'unmeasurable' AND peak IS NULL AND blocks IS NULL))
) STRICT;

PRAGMA user_version = 155;
