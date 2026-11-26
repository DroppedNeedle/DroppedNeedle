-- Two more ways a person tunes Discover, and the group a statement was
-- made for.
--
-- `more_from_seed` is the other side of `less_from_seed`: one of their
-- artists weighs more wherever the pool sums support. `phase` ("That was a
-- phase") says an artist's plays and imported listens no longer speak for
-- it; hearts, follows and the rest still do.
--
-- A statement about a whole taste group is kept as one row for each artist
-- the group held when it was said, so that it outlives the groups being
-- worked out again. Each row carries the group's anchor artist, by
-- MusicBrainz id (lowercased) in `group_anchor_mbid`, which keys the
-- statement, and the group's name in `group_label`, for the list. A
-- statement about one artist has neither.
--
-- Every row carries across with no group. Nothing references the table, so
-- one transaction does.

CREATE TABLE catalog_discover_tunings_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('less_from_seed', 'more_from_seed', 'phase',
        'less_of_kind', 'not_this_source')),
    target TEXT NOT NULL,
    target_name TEXT NOT NULL,
    group_anchor_mbid TEXT,
    group_label TEXT,
    created_at INTEGER NOT NULL,
    CHECK ((group_anchor_mbid IS NULL) = (group_label IS NULL)),
    PRIMARY KEY (user_id, kind, target)
) STRICT;

INSERT INTO catalog_discover_tunings_new
    (rowid, user_id, kind, target, target_name, group_anchor_mbid, group_label, created_at)
SELECT rowid, user_id, kind, target, target_name, NULL, NULL, created_at
FROM catalog_discover_tunings;

DROP TABLE catalog_discover_tunings;

ALTER TABLE catalog_discover_tunings_new RENAME TO catalog_discover_tunings;

PRAGMA user_version = 180;
