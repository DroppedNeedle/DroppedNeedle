-- 0011_library_identify_state.sql - durable identification state.
--
-- Accepted identities, release pins, and album and artist aliases use the
-- 0001 tables (local_*_external_identities, album_release_pins,
-- local_*_aliases). This migration adds what has no home there:
--
-- - library_identify_jobs: the identification queue. One row per job with
--   its attempts, backoff (not_before_ms), worker lease, and an idempotency
--   key (album id plus the album's input revision). At most one live job
--   holds a key. Jobs follow their album away.
-- - library_identify_reviews: ambiguous cases waiting on a curator, with
--   the scored candidates the curator chooses from.
-- - library_identify_credit_proofs: provider proof that a local artist is
--   a MusicBrainz artist, banked per supported track of an identified
--   album, valid while the identity revisions it names still hold.
-- - library_identify_track_credits: provider artist credits per track.
-- - local_track_aliases: retired track ids that keep resolving.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_identify_jobs (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('automatic','manual','historical')),
    priority INTEGER NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('queued','running','deferred','succeeded','failed','attention')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    not_before_ms INTEGER NOT NULL DEFAULT 0,
    lease_expires_ms INTEGER,
    input_revision TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    requested_by_user_id TEXT,
    failure_code TEXT,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_identify_jobs_live_key
    ON library_identify_jobs(idempotency_key)
    WHERE state IN ('queued','running','deferred');

CREATE INDEX IF NOT EXISTS idx_identify_jobs_claim
    ON library_identify_jobs(state, priority, not_before_ms);

CREATE INDEX IF NOT EXISTS idx_identify_jobs_album
    ON library_identify_jobs(local_album_id);

CREATE TABLE IF NOT EXISTS library_identify_reviews (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason_code TEXT NOT NULL,
    candidates_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','approved','rejected')),
    resolved_by_user_id TEXT,
    selected_candidate_key TEXT,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_identify_reviews_album
    ON library_identify_reviews(local_album_id, state);

CREATE TABLE IF NOT EXISTS library_identify_credit_proofs (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    source_local_artist_id TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    release_mbid TEXT NOT NULL,
    album_identity_revision INTEGER NOT NULL,
    track_identity_revision INTEGER NOT NULL,
    PRIMARY KEY(local_track_id, source_local_artist_id, artist_mbid)
);

CREATE INDEX IF NOT EXISTS idx_identify_credit_proofs_artist
    ON library_identify_credit_proofs(source_local_artist_id);

CREATE TABLE IF NOT EXISTS library_identify_track_credits (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position >= 0),
    artist_mbid TEXT NOT NULL,
    canonical_name TEXT NOT NULL,
    credited_name TEXT NOT NULL,
    PRIMARY KEY(local_track_id, position)
);

CREATE TABLE IF NOT EXISTS local_track_aliases (
    alias TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('merged_track')),
    created_at REAL NOT NULL
);

-- Migration high-water mark.
PRAGMA user_version = 11;
