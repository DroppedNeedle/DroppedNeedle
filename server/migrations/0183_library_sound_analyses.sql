-- What each library track sounds like, worked out by the library's sound
-- analysis loop from the file's audio once an admin turns it on. A row
-- counts only while its stat revision is the track's: a changed file is
-- analysed again. Each row carries the analysis version it was made at,
-- and a new version keeps the old rows until it covers the library.
-- `vector` is the mean of the model's 200 outputs over the track's
-- patches, as 200 little-endian 32-bit floats; `tags` is the 50 tag
-- scores, a byte each. A file that could not be analysed keeps a row
-- saying why, with neither.

CREATE TABLE library_sound_analyses (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    analysis_version INTEGER NOT NULL,
    stat_revision TEXT NOT NULL,
    model TEXT NOT NULL,
    outcome TEXT NOT NULL
        CHECK (outcome IN ('analysed', 'undecodable', 'too_short', 'over_time')),
    vector BLOB,
    tags BLOB,
    analysed_seconds REAL NOT NULL,
    analysed_at INTEGER NOT NULL,
    PRIMARY KEY (local_track_id, analysis_version),
    CHECK ((outcome = 'analysed' AND vector IS NOT NULL AND length(vector) = 800
            AND tags IS NOT NULL AND length(tags) = 50)
        OR (outcome <> 'analysed' AND vector IS NULL AND tags IS NULL))
) STRICT;

PRAGMA user_version = 183;
