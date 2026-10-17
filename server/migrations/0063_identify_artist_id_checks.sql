-- 0063_identify_artist_id_checks.sql - what MusicBrainz said about the
-- artist ids that disagree on an artist record.
--
--   library_identify_artist_id_checks
--       one row per MusicBrainz artist id identification asked about, or
--       will ask about: an id that sat on an artist record beside another
--       id (its files' ids and a matched album's proofs disagree). The
--       state is pending until MusicBrainz answers, then kept (the id is
--       current, unknown, or its merge is not followed) or merged, with
--       the id MusicBrainz merged it into. A merged row is never deleted:
--       the retired id stays on file as an alias. Times are milliseconds.
--
-- From here on the artist evidence columns 0050 added
-- (local_track_artists.artist_mbid, local_album_artists.artist_mbid) and
-- library_artist_credit_proofs.artist_mbid hold MusicBrainz's current id
-- for an id recorded here as merged, not the id the file or the release
-- document carries. local_tracks keeps what the file says.
--
-- Every matched album is worked through again after the upgrade, so the
-- ids that already disagree are recorded.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_identify_artist_id_checks (
    artist_mbid TEXT PRIMARY KEY
        CHECK(length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid)),
    state TEXT NOT NULL CHECK(state IN ('pending','kept','merged')),
    canonical_mbid TEXT
        CHECK(canonical_mbid IS NULL
              OR (length(canonical_mbid) = 36 AND canonical_mbid = lower(canonical_mbid))),
    nominated_at INTEGER NOT NULL,
    checked_at INTEGER,
    CHECK((state = 'merged') = (canonical_mbid IS NOT NULL)),
    CHECK(canonical_mbid IS NULL OR canonical_mbid <> artist_mbid),
    CHECK((state = 'pending') = (checked_at IS NULL))
) STRICT;

CREATE INDEX idx_identify_artist_id_checks_state
    ON library_identify_artist_id_checks(state, nominated_at);
CREATE INDEX idx_identify_artist_id_checks_canonical
    ON library_identify_artist_id_checks(canonical_mbid) WHERE canonical_mbid IS NOT NULL;

UPDATE library_artist_reconciliation_state SET input_revision = '';

-- Migration high-water mark.
PRAGMA user_version = 63;
