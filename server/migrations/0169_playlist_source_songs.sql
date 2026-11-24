-- The songs a playlist took from its outside source (a Spotify playlist or
-- Liked Songs), one row per source song, so a song is added once: a row
-- stays when its entry is removed by hand, which is how a removed song is
-- never added back. The album facts are what the source said about the
-- song's album, kept so the album can be looked for again without asking
-- the source. `album_pick` says how the entry's album was chosen when it is
-- not the source's own filing: the artist's own album the sync moved it to,
-- or the source's album a person kept; NULL is as filed. `followed_up_at` is
-- when the sync was done with the song (requested it, or found nothing to
-- ask for), and `filed_kind` and `filed_key` the request it filed for it,
-- when it filed one: an album or a song, by MusicBrainz id. `look_again_at`
-- and `looks` pace the search for an album nothing has proven yet.

CREATE TABLE catalog_playlist_source_songs (
    playlist_id     TEXT    NOT NULL REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    source_song_id  TEXT    NOT NULL CHECK (length(source_song_id) BETWEEN 1 AND 64),
    entry_id        TEXT    REFERENCES catalog_playlist_tracks(id) ON DELETE SET NULL,
    seen_at         INTEGER NOT NULL,
    album_source_id TEXT    CHECK (album_source_id IS NULL OR length(album_source_id) BETWEEN 1 AND 64),
    album_title     TEXT    NOT NULL DEFAULT '',
    album_artist    TEXT    NOT NULL DEFAULT '',
    album_kind      TEXT    CHECK (album_kind IN ('album', 'single', 'compilation')),
    album_tracks    INTEGER CHECK (album_tracks IS NULL OR album_tracks >= 1),
    album_pick      TEXT    CHECK (album_pick IN ('own_album', 'kept_spotify')),
    followed_up_at  INTEGER,
    filed_kind      TEXT    CHECK (filed_kind IN ('album', 'track')),
    filed_key       TEXT    CHECK (filed_key IS NULL OR length(filed_key) = 36),
    look_again_at   INTEGER,
    looks           INTEGER NOT NULL DEFAULT 0 CHECK (looks >= 0),
    CHECK ((filed_kind IS NULL) = (filed_key IS NULL)),
    PRIMARY KEY (playlist_id, source_song_id)
) STRICT;

CREATE INDEX idx_catalog_playlist_source_songs_entry
    ON catalog_playlist_source_songs(entry_id) WHERE entry_id IS NOT NULL;

CREATE INDEX idx_catalog_playlist_source_songs_open
    ON catalog_playlist_source_songs(playlist_id, look_again_at)
    WHERE followed_up_at IS NULL AND entry_id IS NOT NULL;

PRAGMA user_version = 169;
