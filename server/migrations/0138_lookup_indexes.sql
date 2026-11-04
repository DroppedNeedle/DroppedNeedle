-- Indexes for lookups by a column no index led with, one that repeated a
-- primary key, and a column nothing read.

-- Merges and the artist checks find an item's favourites, and their kept
-- names, by item_kind and item_id across every user; the primary keys
-- lead with user_id, so each lookup read every row. Ratings already have
-- this index.
CREATE INDEX idx_catalog_user_favorites_item
    ON catalog_user_favorites(item_kind, item_id);
CREATE INDEX idx_catalog_user_favorite_names_item
    ON catalog_user_favorite_names(item_kind, item_id);

-- The primary key (user_id, item_kind, item_id) already answers every
-- lookup by user_id and item_kind.
DROP INDEX idx_catalog_user_favorites_user_kind;

-- Clearing an album's blocklist looks its rows up by release group; the
-- unique key leads with source.
CREATE INDEX idx_download_quarantine_release_group
    ON download_quarantine(release_group_mbid);

-- A link's track count is counted from its track links whenever it is
-- read, so the stored copy was written and never read. No table refers to
-- catalog_youtube_links and no index or trigger names the column.
ALTER TABLE catalog_youtube_links DROP COLUMN track_count;

PRAGMA user_version = 138;
