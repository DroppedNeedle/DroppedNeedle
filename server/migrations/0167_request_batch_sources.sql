-- Where a request batch came from: Discover's "Download all" or a
-- playlist's Request missing. Every batch made before is Discover's. An
-- album of a playlist batch also counts the playlist's songs it carries,
-- and the generation of the request the batch made for it, so a later
-- request for the same album is never taken for the batch's. Discover's
-- albums count no songs, and albums filed before carry neither.

ALTER TABLE acquire_discovery_batches ADD COLUMN source TEXT NOT NULL DEFAULT 'discover'
    CHECK (source IN ('discover', 'playlist'));

ALTER TABLE acquire_discovery_batch_items ADD COLUMN song_count INTEGER
    CHECK (song_count IS NULL OR song_count >= 1);

ALTER TABLE acquire_discovery_batch_items ADD COLUMN request_generation INTEGER
    CHECK (request_generation IS NULL OR request_generation >= 0);

PRAGMA user_version = 167;
