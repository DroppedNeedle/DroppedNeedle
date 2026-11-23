-- How a person tuned what Discover offers them, and one more fact kept on
-- each path of their pool.
--
-- `catalog_discover_tunings` holds one row a person, kind and target, with
-- the target's name for the list and when it was said. `less_from_seed`
-- (the target is one of their artists, by MusicBrainz id) halves that
-- artist's weight wherever the pool sums support. The kinds are listed
-- whole, since a CHECK cannot change without rebuilding its table:
-- `less_of_kind` and `not_this_source` are written by later work, which
-- gives them their meaning. Forgetting a person's history leaves these
-- rows, as it leaves the rest of what they told Discover; a person's rows
-- go with their account.
--
-- `link_closest_new` on `catalog_discover_candidate_paths` says the
-- candidate's artist was the first in that seed's list the person did not
-- know when the pool was built, every artist above it being one of theirs.
-- Rows built before it read as not.

CREATE TABLE catalog_discover_tunings (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('less_from_seed', 'less_of_kind', 'not_this_source')),
    target TEXT NOT NULL,
    target_name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, kind, target)
) STRICT;

ALTER TABLE catalog_discover_candidate_paths
    ADD COLUMN link_closest_new INTEGER NOT NULL DEFAULT 0 CHECK (link_closest_new IN (0, 1));

PRAGMA user_version = 165;
