-- 0039_review_states.sql - two more ways to settle an identification review.
--
-- library_identify_reviews (0011) knew pending, approved and rejected. A
-- person can now also:
--   dismissed  close the review without a decision (the album stays as it
--              is), or start a fresh identification in its place
--   excluded   take the album's files out of the library; restoring them
--              settles the review as dismissed
--
-- SQLite cannot widen a CHECK in place, so the table is rebuilt with the
-- same columns and index. No other table referenced it before this.
--
-- library_review_exclusions records which files an exclude took out, so a
-- restore puts back exactly those and leaves files excluded some other
-- way alone.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_identify_reviews_new (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason_code TEXT NOT NULL,
    candidates_json TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('pending','approved','rejected','dismissed','excluded')),
    resolved_by_user_id TEXT,
    selected_candidate_key TEXT,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);

INSERT INTO library_identify_reviews_new
    SELECT id, local_album_id, reason_code, candidates_json, state,
           resolved_by_user_id, selected_candidate_key, created_ms, updated_ms
    FROM library_identify_reviews;

DROP TABLE library_identify_reviews;
ALTER TABLE library_identify_reviews_new RENAME TO library_identify_reviews;

CREATE INDEX IF NOT EXISTS idx_identify_reviews_album
    ON library_identify_reviews(local_album_id, state);
CREATE INDEX IF NOT EXISTS idx_identify_reviews_state
    ON library_identify_reviews(state, updated_ms DESC);

CREATE TABLE IF NOT EXISTS library_review_exclusions (
    review_id TEXT NOT NULL REFERENCES library_identify_reviews(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    PRIMARY KEY (review_id, local_track_id)
);

-- Migration high-water mark.
PRAGMA user_version = 39;
