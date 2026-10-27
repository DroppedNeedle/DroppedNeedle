-- Lowered MusicBrainz ids on tracks, so a lookup of a release group or a
-- recording reaches the library's tracks by key. Stored ids keep the case
-- they were written in; the lookups compare them lowered.
CREATE INDEX IF NOT EXISTS idx_local_tracks_release_group_lower
    ON local_tracks(lower(embedded_release_group_mbid))
    WHERE embedded_release_group_mbid IS NOT NULL;

-- Not partial: a recording lookup compares the id with a missing one
-- read as empty, so tracks without an id are found through it too.
CREATE INDEX IF NOT EXISTS idx_local_tracks_recording_lower
    ON local_tracks(lower(COALESCE(embedded_recording_mbid, '')));

CREATE INDEX IF NOT EXISTS idx_local_track_identity_recording_lower
    ON local_track_external_identities(lower(recording_mbid));

-- Migration high-water mark.
PRAGMA user_version = 93;
