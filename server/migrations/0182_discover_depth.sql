-- How far from what a person knows their Discover picks go.
--
-- `catalog_discover_prefs.depth` is the person's choice: home ground, next
-- door (the default, and what everyone had before) or far out. It orders
-- their pool and sets the listener floors their picks clear.
--
-- `catalog_discover_pool_builds.depth` is the depth a pool was ordered at,
-- so a set is never dealt from a pool of a depth the person has left.
--
-- `catalog_weekly_sets.depth` is the depth a set was dealt at, written once
-- with the set.
--
-- Pool builds and sets made before these were kept have none, which reads
-- as next door.

ALTER TABLE catalog_discover_prefs ADD COLUMN depth TEXT NOT NULL DEFAULT 'next_door'
    CHECK (depth IN ('home_ground', 'next_door', 'far_out'));

ALTER TABLE catalog_discover_pool_builds ADD COLUMN depth TEXT
    CHECK (depth IN ('home_ground', 'next_door', 'far_out'));

ALTER TABLE catalog_weekly_sets ADD COLUMN depth TEXT
    CHECK (depth IN ('home_ground', 'next_door', 'far_out'));

PRAGMA user_version = 182;
