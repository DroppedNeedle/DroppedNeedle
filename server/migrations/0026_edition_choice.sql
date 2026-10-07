-- 0026_edition_choice.sql - one source of truth for an album's edition.
--
-- An album's edition is its MusicBrainz identity row
-- (local_album_external_identities). A person's choice is that row with
-- decision_source 'manual'; nothing automatic replaces it. The two older
-- pin tables (library_album_release_pins per album and album_release_pins
-- per release group) only steered what a page showed. This migration turns
-- every pin into a choice on the identity row and empties both tables;
-- nothing reads them any more.
--
-- New tables:
--   library_album_match_state     albums whose match is a best guess
--                                 ('unconfirmed') or that match nothing
--                                 ('unmatched'), with the closest
--                                 candidates. No row means confirmed, or
--                                 never identified.
--   library_edition_choice_undo   what an edition choice replaced, so the
--                                 person can take it back.
--   library_edition_remap_queue   albums whose edition was chosen without
--                                 mapping the files (converted pins, v2
--                                 imports). A worker maps the files onto
--                                 the chosen release when MusicBrainz is
--                                 reachable.
--   local_track_edition_tags      the edition hints file tags carry
--                                 (media, barcode, catalog number, country,
--                                 disc and track totals), for matching.
--
-- Safe to apply twice: every table is created only when missing, and the
-- pin conversion works off the pin tables, which it leaves empty. No down
-- migration: rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_album_match_state (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('unconfirmed', 'unmatched')),
    reason_code TEXT NOT NULL,
    release_mbid TEXT,
    candidates_json TEXT NOT NULL DEFAULT '[]',
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_album_match_state_state
    ON library_album_match_state(state, updated_at DESC);

CREATE TABLE IF NOT EXISTS library_edition_choice_undo (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL UNIQUE REFERENCES local_albums(id) ON DELETE CASCADE,
    action_id TEXT NOT NULL,
    prior_identity_json TEXT,
    prior_tracks_json TEXT NOT NULL DEFAULT '[]',
    prior_match_state_json TEXT,
    expected_identity_revision INTEGER NOT NULL,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_edition_remap_queue (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    release_mbid TEXT NOT NULL,
    chosen_by_user_id TEXT,
    queued_at REAL NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before REAL NOT NULL DEFAULT 0,
    last_code TEXT,
    -- 'album_pin': a person pinned this album; 'group_pin': the album got
    -- the choice from a release-group pin covering every copy.
    origin TEXT NOT NULL DEFAULT 'album_pin' CHECK(origin IN ('album_pin', 'group_pin'))
);

CREATE TABLE IF NOT EXISTS local_track_edition_tags (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    media TEXT,
    barcode TEXT,
    catalog_number TEXT,
    release_country TEXT,
    total_discs INTEGER,
    total_tracks INTEGER
);

-- Release-group pins name no album. Each one becomes a pin on every live
-- album of that group the matcher picked, unless the album has its own pin.
-- The albums that got their choice this way are remembered for the queue.
CREATE TEMP TABLE IF NOT EXISTS edition_group_pinned (local_album_id TEXT PRIMARY KEY);
INSERT OR IGNORE INTO edition_group_pinned (local_album_id)
SELECT e.local_album_id
FROM album_release_pins g
JOIN local_album_external_identities e
    ON lower(e.release_group_mbid) = lower(g.release_group_mbid) AND e.provider = 'musicbrainz'
JOIN local_albums a ON a.id = e.local_album_id
WHERE a.retired_into_album_id IS NULL
  AND e.decision_source IN ('automatic', 'embedded')
  AND e.local_album_id NOT IN (SELECT local_album_id FROM library_album_release_pins);

INSERT OR IGNORE INTO library_album_release_pins (local_album_id, release_group_mbid,
    release_mbid, set_by_user_id, set_at)
SELECT e.local_album_id, g.release_group_mbid, g.release_mbid, g.set_by_user_id, g.set_at
FROM album_release_pins g
JOIN local_album_external_identities e
    ON lower(e.release_group_mbid) = lower(g.release_group_mbid) AND e.provider = 'musicbrainz'
JOIN local_albums a ON a.id = e.local_album_id
WHERE a.retired_into_album_id IS NULL
  AND e.decision_source IN ('automatic', 'embedded');

-- A pin on an album that a person matched by hand ('manual') loses: the
-- manual match is the later, deliberate word. Every other pin becomes the
-- album's chosen edition.
DELETE FROM library_album_release_pins
WHERE local_album_id IN (
    SELECT local_album_id FROM local_album_external_identities
    WHERE provider = 'musicbrainz' AND decision_source = 'manual')
   OR local_album_id NOT IN (SELECT id FROM local_albums WHERE retired_into_album_id IS NULL);

-- Track rows keep their placements until the remap worker places them
-- on the chosen release; nothing is wiped up front.

UPDATE local_album_external_identities
SET release_group_mbid = (SELECT p.release_group_mbid FROM library_album_release_pins p
                          WHERE p.local_album_id = local_album_external_identities.local_album_id),
    release_mbid = (SELECT p.release_mbid FROM library_album_release_pins p
                    WHERE p.local_album_id = local_album_external_identities.local_album_id),
    decision_source = 'manual',
    selected_by_user_id = (SELECT u.id FROM library_album_release_pins p
                           JOIN auth_users u ON u.id = p.set_by_user_id
                           WHERE p.local_album_id = local_album_external_identities.local_album_id),
    selected_at = CAST(strftime('%s', 'now') AS REAL),
    row_revision = row_revision + 1
WHERE provider = 'musicbrainz'
  AND local_album_id IN (SELECT local_album_id FROM library_album_release_pins);

INSERT OR IGNORE INTO local_album_external_identities (local_album_id, provider,
    release_group_mbid, release_mbid, decision_source, selected_by_user_id, selected_at)
SELECT p.local_album_id, 'musicbrainz', p.release_group_mbid, p.release_mbid, 'manual',
    (SELECT u.id FROM auth_users u WHERE u.id = p.set_by_user_id),
    CAST(strftime('%s', 'now') AS REAL)
FROM library_album_release_pins p;

INSERT OR IGNORE INTO library_edition_remap_queue (local_album_id, release_mbid,
    chosen_by_user_id, queued_at, origin)
SELECT p.local_album_id, lower(p.release_mbid),
    (SELECT u.id FROM auth_users u WHERE u.id = p.set_by_user_id),
    CAST(strftime('%s', 'now') AS REAL),
    CASE WHEN p.local_album_id IN (SELECT local_album_id FROM edition_group_pinned)
         THEN 'group_pin' ELSE 'album_pin' END
FROM library_album_release_pins p;

-- A converted choice is a person's word: the album is confirmed.
DELETE FROM library_album_match_state
WHERE local_album_id IN (SELECT local_album_id FROM library_album_release_pins);

DELETE FROM library_album_release_pins;
DELETE FROM album_release_pins;
DROP TABLE IF EXISTS edition_group_pinned;

-- Cached catalog reads refresh.
INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 0)
    ON CONFLICT (singleton) DO NOTHING;
UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1;

-- Migration high-water mark.
PRAGMA user_version = 26;
