-- 0100_library_review_revisions.sql - a revision on identification
-- reviews, so a person's action on one is refused when the review changed
-- after they read it.
--
--   library_identify_reviews.row_revision
--       starts at 1 and goes up by one whenever the review's state,
--       candidates, reason or album changes. Every write that settles,
--       reopens or moves a review changes one of those, so a trigger
--       keeps the count and no writer can skip it. A review action sends
--       the revision it saw and is refused when it differs.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_identify_reviews
    ADD COLUMN row_revision INTEGER NOT NULL DEFAULT 1;

CREATE TRIGGER library_identify_reviews_revision
AFTER UPDATE OF state, candidates_json, reason_code, local_album_id
ON library_identify_reviews
FOR EACH ROW
WHEN OLD.state IS NOT NEW.state
    OR OLD.candidates_json IS NOT NEW.candidates_json
    OR OLD.reason_code IS NOT NEW.reason_code
    OR OLD.local_album_id IS NOT NEW.local_album_id
BEGIN
    UPDATE library_identify_reviews
    SET row_revision = OLD.row_revision + 1
    WHERE id = NEW.id;
END;

-- Migration high-water mark.
PRAGMA user_version = 100;
