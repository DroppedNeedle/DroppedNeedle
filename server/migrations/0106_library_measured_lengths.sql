-- 0106_library_measured_lengths.sql - whether a track's length was measured.
--
--   local_tracks.duration_measured (new, INTEGER 0 or 1, nullable)
--       1 when the length came from the audio's own count of it, 0 when it
--       is an estimate: an MP3 with no Xing, Info or VBRI frame count, whose
--       length is worked out from its first frame's bitrate and can be
--       seconds off. Only a measured length settles a fingerprint dispute
--       or confirms a live album. NULL means not read yet and reads as
--       estimated.
--
-- Every other format is always measured, so existing rows get 1. MP3 rows
-- stay NULL until the next scan that walks them reads the first frame
-- (read-only) and records which it is.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE local_tracks ADD COLUMN duration_measured INTEGER
    CHECK (duration_measured IN (0, 1));

UPDATE local_tracks SET duration_measured = 1
WHERE lower(COALESCE(file_format, '')) != 'mp3'
  AND relative_path NOT LIKE '%.mp3';

-- Migration high-water mark.
PRAGMA user_version = 106;
