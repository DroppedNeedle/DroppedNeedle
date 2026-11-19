-- Library entries were stored under two older names the web player does
-- not know: `droppedneedle-local`, which Subsonic and Jellyfin apps
-- wrote, and `howler`, v2's name. Both become `local`, in `source_type`
-- and in the `available_sources` list, which keeps each name once, in
-- its first place. The v2 export makes the same change.

UPDATE catalog_playlist_tracks SET source_type = 'local'
WHERE source_type IN ('droppedneedle-local', 'howler');

UPDATE catalog_playlist_tracks SET available_sources = (
    SELECT json_group_array(source) FROM (
        SELECT CASE WHEN value IN ('droppedneedle-local', 'howler') THEN 'local'
            ELSE value END AS source, MIN(key) AS first
        FROM json_each(catalog_playlist_tracks.available_sources)
        GROUP BY source ORDER BY first))
WHERE json_valid(available_sources) AND json_type(available_sources) = 'array'
    AND EXISTS (SELECT 1 FROM json_each(available_sources)
        WHERE value IN ('droppedneedle-local', 'howler'));

PRAGMA user_version = 150;
