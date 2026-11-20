-- The ISRCs a file's tags carry, beside its other edition tags: a file
-- whose MusicBrainz ids are on none of the release it names is matched by
-- one when a track of that release carries it. NULL until the scan reads
-- the file again; empty for a file read with none; else the uppercased
-- ISRCs joined by commas.

ALTER TABLE local_track_edition_tags ADD COLUMN isrcs TEXT;

PRAGMA user_version = 153;
