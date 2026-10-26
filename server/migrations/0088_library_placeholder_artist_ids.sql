-- 0088_library_placeholder_artist_ids.sql - drop special-purpose artist ids.
--
-- MusicBrainz's special-purpose artists (Various Artists, [unknown],
-- [anonymous], [traditional], [no artist], [data], [dialogue]) stand for no
-- one artist, so a credit holding one proves nothing. Scans no longer store
-- them; this clears the ones earlier scans stored, the same as reading those
-- files again. No artist row is split or merged, and no trigger reads
-- artist_mbid.
--
--   library_artist_reconciliation_state.input_revision
--       emptied for each album whose album or track credits change below,
--       so the next artist link sweep works it through again and links the
--       rows these ids kept from being linked.
--   local_track_artists.artist_mbid, local_album_artists.artist_mbid
--       NULL where the id is one of the seven.
--
-- The ids are a frozen copy of PLACEHOLDER_ARTISTS in
-- server/library/src/identify/artist_links.rs as of this migration.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

UPDATE library_artist_reconciliation_state SET input_revision = ''
WHERE local_album_id IN (
    SELECT local_album_id FROM local_album_artists
    WHERE artist_mbid IN (
        '89ad4ac3-39f7-470e-963a-56509c546377',
        '125ec42a-7229-4250-afc5-e057484327fe',
        'f731ccc4-e22a-43af-a747-64213329e088',
        '9be7f096-97ec-4615-8957-8d40b5dcbc41',
        'eec63d3c-3b81-4ad4-b1e4-7c147d4d2b61',
        '33cf029c-63b0-41a0-9855-be2a3665fb3b',
        '314e1c25-dde7-4e4d-b2f4-0a7b9f7c56dc'
    )
    UNION
    SELECT t.local_album_id FROM local_track_artists c
    JOIN local_tracks t ON t.id = c.local_track_id
    WHERE c.artist_mbid IN (
        '89ad4ac3-39f7-470e-963a-56509c546377',
        '125ec42a-7229-4250-afc5-e057484327fe',
        'f731ccc4-e22a-43af-a747-64213329e088',
        '9be7f096-97ec-4615-8957-8d40b5dcbc41',
        'eec63d3c-3b81-4ad4-b1e4-7c147d4d2b61',
        '33cf029c-63b0-41a0-9855-be2a3665fb3b',
        '314e1c25-dde7-4e4d-b2f4-0a7b9f7c56dc'
    )
);

UPDATE local_track_artists SET artist_mbid = NULL
WHERE artist_mbid IN (
    '89ad4ac3-39f7-470e-963a-56509c546377',
    '125ec42a-7229-4250-afc5-e057484327fe',
    'f731ccc4-e22a-43af-a747-64213329e088',
    '9be7f096-97ec-4615-8957-8d40b5dcbc41',
    'eec63d3c-3b81-4ad4-b1e4-7c147d4d2b61',
    '33cf029c-63b0-41a0-9855-be2a3665fb3b',
    '314e1c25-dde7-4e4d-b2f4-0a7b9f7c56dc'
);

UPDATE local_album_artists SET artist_mbid = NULL
WHERE artist_mbid IN (
    '89ad4ac3-39f7-470e-963a-56509c546377',
    '125ec42a-7229-4250-afc5-e057484327fe',
    'f731ccc4-e22a-43af-a747-64213329e088',
    '9be7f096-97ec-4615-8957-8d40b5dcbc41',
    'eec63d3c-3b81-4ad4-b1e4-7c147d4d2b61',
    '33cf029c-63b0-41a0-9855-be2a3665fb3b',
    '314e1c25-dde7-4e4d-b2f4-0a7b9f7c56dc'
);

-- Migration high-water mark.
PRAGMA user_version = 88;
