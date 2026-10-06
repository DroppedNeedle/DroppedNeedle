-- 0013_library_stable_ids.sql - album and track ids that survive moves.
--
-- Albums used to be found by a hash of root, folder, album title and
-- album artist, so moving or organizing a folder filed its tracks under
-- a new album and left the curator's identity on an empty one. Albums
-- are now found by a persistent key kept in local_albums.grouping_key:
--
-- - 'mbid:<release mbid>' when the files carry a MusicBrainz release id;
-- - 'tag:<album artist>' || char(31) || '<album title>' (both folded)
--   when both names come from tags;
-- - the old folder-based key otherwise, since names parsed from the path
--   only mean something next to that path.
--
-- The key carries an album along when its files move; copies of one
-- album in different folders still stay separate albums.
--
-- Existing albums get their key from their tracks here. The scan also
-- matches a file that appeared to a track whose file went away (same
-- album, disc and track number, plus the recording MBID or the title and
-- duration) and keeps the old track id, so it needs an index on the
-- recording MBID. library_scan_album_moves records which album a re-indexed track
-- left during a run, so a whole album that was retagged together keeps
-- its album row. It is its own table so this file stays safe to apply
-- twice, like every migration.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_scan_album_moves (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    local_track_id TEXT NOT NULL,
    previous_album_id TEXT NOT NULL,
    PRIMARY KEY (run_id, root_id, relative_path)
);

CREATE INDEX IF NOT EXISTS idx_library_scan_album_moves_from
    ON library_scan_album_moves(run_id, previous_album_id);

UPDATE local_albums SET grouping_key = COALESCE(
    (SELECT 'mbid:' || lower(trim(t.embedded_release_mbid))
       FROM local_tracks t
      WHERE t.local_album_id = local_albums.id
        AND trim(COALESCE(t.embedded_release_mbid, '')) <> ''
      ORDER BY t.id LIMIT 1),
    (SELECT 'tag:' || local_albums.album_artist_name_folded || char(31)
            || local_albums.title_folded
      WHERE local_albums.album_artist_name_folded IS NOT NULL
        AND EXISTS (SELECT 1 FROM local_tracks t
                     WHERE t.local_album_id = local_albums.id
                       AND t.album_title_provenance = 'tag'
                       AND t.album_artist_provenance = 'tag')),
    grouping_key);

CREATE INDEX IF NOT EXISTS idx_local_albums_grouping_key
    ON local_albums(grouping_key);

CREATE INDEX IF NOT EXISTS idx_local_tracks_recording
    ON local_tracks(embedded_recording_mbid)
    WHERE embedded_recording_mbid IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 13;
