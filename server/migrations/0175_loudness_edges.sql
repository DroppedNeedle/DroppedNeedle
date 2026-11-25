-- Where each measured track's sound starts, where its fade-out starts and
-- where its sound ends, in milliseconds from the start of the file, worked
-- out by the loudness loop in the same pass as its loudness. All three are
-- set or none: none on an unmeasurable row, on a track too short or too
-- quiet to tell, and on every row measured before. `measure_version` says
-- which measurement wrote the row; rows from before are version 1, and the
-- loop measures a measured row below its current version again.

ALTER TABLE library_loudness_measurements ADD COLUMN sound_start_ms INTEGER;
ALTER TABLE library_loudness_measurements ADD COLUMN fade_start_ms INTEGER;
ALTER TABLE library_loudness_measurements ADD COLUMN sound_end_ms INTEGER
    CHECK ((sound_start_ms IS NULL AND fade_start_ms IS NULL AND sound_end_ms IS NULL)
        OR (outcome = 'measured' AND sound_start_ms IS NOT NULL AND fade_start_ms IS NOT NULL
            AND sound_end_ms IS NOT NULL AND 0 <= sound_start_ms
            AND sound_start_ms <= fade_start_ms AND fade_start_ms <= sound_end_ms));
ALTER TABLE library_loudness_measurements ADD COLUMN measure_version INTEGER NOT NULL DEFAULT 1
    CHECK (measure_version >= 1);

PRAGMA user_version = 175;
