-- 0054_artist_credit_proofs.sql - what a matched album proves about the
-- artists credited on it.
--
--   library_artist_credit_proofs
--       one row per credit position an album's accepted MusicBrainz match
--       proves to be a MusicBrainz artist: the album's own credits, and
--       each mapped track's. Written by identification, replaced whenever
--       the album's match, its track mappings or its credits change. A
--       proof counts only while the album, its match and the credit it
--       proves are still as they were when it was written.
--   library_artist_reconciliation_state
--       one row per album with a MusicBrainz match: where proving its
--       artists stands (waiting for an accepted match, credits that do
--       not line up with the release's, or proven), and the stamp of the
--       match and track mappings it was worked out from.
--
-- Both tables come from the baseline and have never held rows: they are
-- dropped and made again, strict, with a proof and a state going with
-- their album or track. An artist with a proof still cannot be deleted.
-- Times are milliseconds. Every matched album is worked through again
-- after the upgrade.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE library_artist_credit_proofs;
DROP TABLE library_artist_reconciliation_state;

CREATE TABLE library_artist_credit_proofs (
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('album','track')),
    subject_id TEXT NOT NULL,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE CASCADE,
    credit_position INTEGER NOT NULL CHECK(credit_position >= 0),
    source_local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    artist_mbid TEXT NOT NULL
        CHECK(length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid)),
    canonical_name TEXT NOT NULL,
    credited_name TEXT NOT NULL,
    sort_name TEXT NOT NULL DEFAULT '',
    join_phrase TEXT NOT NULL DEFAULT '',
    release_mbid TEXT NOT NULL,
    release_track_mbid TEXT,
    album_identity_revision INTEGER NOT NULL
        CHECK(album_identity_revision BETWEEN 1 AND 9223372036854775807),
    track_identity_revision INTEGER
        CHECK(track_identity_revision IS NULL OR track_identity_revision
              BETWEEN 1 AND 9223372036854775807),
    evidence_hash TEXT NOT NULL CHECK(
        length(evidence_hash) = 64 AND evidence_hash = lower(evidence_hash)
        AND evidence_hash NOT GLOB '*[^0-9a-f]*'
    ),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(subject_kind, subject_id, credit_position),
    CHECK(
        (subject_kind = 'album' AND subject_id = local_album_id AND local_track_id IS NULL
         AND release_track_mbid IS NULL AND track_identity_revision IS NULL)
        OR
        (subject_kind = 'track' AND subject_id = local_track_id AND local_track_id IS NOT NULL
         AND release_track_mbid IS NOT NULL AND track_identity_revision IS NOT NULL)
    )
) STRICT;

CREATE INDEX idx_artist_credit_proof_source
    ON library_artist_credit_proofs(source_local_artist_id, artist_mbid);
CREATE INDEX idx_artist_credit_proof_resolved
    ON library_artist_credit_proofs(local_artist_id, artist_mbid);
CREATE INDEX idx_artist_credit_proof_album
    ON library_artist_credit_proofs(local_album_id, subject_kind, subject_id);
CREATE INDEX idx_artist_credit_proof_track
    ON library_artist_credit_proofs(local_track_id);
CREATE INDEX idx_artist_credit_proof_mbid
    ON library_artist_credit_proofs(artist_mbid, source_local_artist_id);

CREATE TABLE library_artist_reconciliation_state (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    input_revision TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN (
        'waiting_for_identity','ambiguous_credit_structure','projected'
    )),
    reason_code TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_artist_reconciliation_state_status
    ON library_artist_reconciliation_state(state);

-- Migration high-water mark.
PRAGMA user_version = 54;
