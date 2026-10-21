-- 0079_library_track_list_orders.sql - the track list's artist and album
-- orders walk an index.
--
--   idx_local_tracks_artist_order (new)
--       indexed tracks by artist (no artist sorts as an empty name), album,
--       album id, disc, track and id: the track list sorted by artist reads
--       a page, first or deep, without sorting the catalog.
--   idx_local_tracks_album_order_folded (new)
--       the same by album, album id, disc, track and id.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE INDEX IF NOT EXISTS idx_local_tracks_artist_order
    ON local_tracks(availability, COALESCE(artist_name_folded, ''), album_title_folded,
                    local_album_id, disc_number, track_number, id);

CREATE INDEX IF NOT EXISTS idx_local_tracks_album_order_folded
    ON local_tracks(availability, album_title_folded, local_album_id, disc_number,
                    track_number, id);

-- Migration high-water mark.
PRAGMA user_version = 79;
