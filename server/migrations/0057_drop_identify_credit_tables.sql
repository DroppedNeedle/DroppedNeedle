-- 0057_drop_identify_credit_tables.sql - drop two identification tables
-- that were never used.
--
--   library_identify_credit_proofs
--       provider proof that a local artist is a MusicBrainz artist, with
--       its index idx_identify_credit_proofs_artist
--   library_identify_track_credits
--       provider artist credits per track
--
-- 0011 added both for an artist proof that was never wired in: nothing
-- wrote a track credit, so no proof was ever stored. Artist proofs live in
-- library_artist_credit_proofs (0054). Both tables are empty in every
-- database, and no other table, trigger or view refers to them.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE library_identify_credit_proofs;
DROP TABLE library_identify_track_credits;

-- Migration high-water mark.
PRAGMA user_version = 57;
