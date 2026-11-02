//! Unified search's local reads.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;

use crate::local::definitions::{musicbrainz, owned_album, owned_artist, recording_mbid};
use crate::local::sql::{TRACK_TEXT_MATCH, TRACK_WORD_LIKE};

droppedneedle_kernel::sql! {
    /// One page of artist hits: live rows whose folded name contains the
    /// pattern, exact first, then prefix, then alphabetical. Binds: the
    /// contains pattern, the folded query, the prefix pattern, limit, offset.
    pub(super) ARTIST_HITS = (concat!(
        "SELECT a.id AS id, a.display_name AS display_name, a.folded_name AS folded_name, \
         e.provider_artist_id AS mbid, (",
        owned_artist!("a"),
        ") AS owned FROM local_artists a \
         LEFT JOIN local_artist_external_identities e ON e.local_artist_id = a.id AND ",
        musicbrainz!("e"),
        " WHERE a.retired_into_artist_id IS NULL \
         AND a.folded_name LIKE ? ESCAPE '\\' \
         ORDER BY CASE WHEN a.folded_name = ? THEN 0 \
         WHEN a.folded_name LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
         a.folded_name ASC LIMIT ? OFFSET ?"
    ));
    pub(super) row ArtistHit for [ARTIST_HITS] {
        id: String,
        display_name: String,
        folded_name: String,
        mbid: Option<String>,
        owned: bool,
    }
    /// One page of album hits: owned albums whose folded title or album
    /// artist contains the pattern, exact title first. Binds: the contains
    /// pattern twice, the folded query, the prefix pattern, limit, offset.
    pub(super) ALBUM_HITS = (concat!(
        "SELECT b.id AS id, b.title AS title, b.title_folded AS title_folded, \
         b.album_artist_name AS album_artist_name, b.year AS year, \
         e.release_group_mbid AS mbid, (w.local_album_id IS NOT NULL) AS own_art \
         FROM local_albums b \
         LEFT JOIN local_album_artwork w ON w.local_album_id = b.id \
         AND w.source IN ('folder','embedded') \
         LEFT JOIN local_album_external_identities e ON e.local_album_id = b.id AND ",
        musicbrainz!("e"),
        " WHERE ",
        owned_album!("b"),
        " AND (b.title_folded LIKE ? ESCAPE '\\' \
         OR COALESCE(b.album_artist_name_folded, '') LIKE ? ESCAPE '\\') \
         ORDER BY CASE WHEN b.title_folded = ? THEN 0 \
         WHEN b.title_folded LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
         b.title_folded ASC LIMIT ? OFFSET ?"
    ));
    pub(super) row AlbumHit for [ALBUM_HITS] {
        id: String,
        title: String,
        title_folded: String,
        album_artist_name: Option<String>,
        year: Option<i32>,
        mbid: Option<String>,
        own_art: bool,
    }

    /// A song hit's columns and the page it is cut from. Binds: the folded
    /// query, the prefix pattern and the contains pattern for the tier, then
    /// those of the match that follows.
    fragment SONG_START = (concat!(
        "SELECT t.id AS id, t.title AS title, t.title_folded AS title_folded, \
         t.artist_name AS artist_name, t.year AS year, t.local_album_id AS local_album_id, \
         t.album_title AS album_title, t.disc_number AS disc_number, \
         t.track_number AS track_number, t.duration_seconds AS duration_seconds, \
         t.file_format AS file_format, ",
        recording_mbid!("e", "t"),
        " AS mbid, ae.release_group_mbid AS release_group_mbid, \
         (w.local_album_id IS NOT NULL) AS cover_available, \
         COALESCE(w.source IN ('folder','embedded'), 0) AS own_art, \
         (SELECT ta.local_artist_id FROM local_track_artists ta \
          WHERE ta.local_track_id = t.id ORDER BY ta.position LIMIT 1) AS artist_id \
         FROM (SELECT t.rowid AS hit, \
               CASE WHEN t.title_folded = ? THEN 0 \
               WHEN t.title_folded LIKE ? ESCAPE '\\' THEN 1 \
               WHEN t.title_folded LIKE ? ESCAPE '\\' THEN 2 ELSE 3 END AS tier \
               FROM local_tracks t \
               WHERE +t.availability = 'indexed' AND "
    ));
    /// Closes the page, limit and offset bound, and joins what its rows
    /// show: the identities, the artwork and the track artist.
    fragment SONG_END = (concat!(
        " ORDER BY tier, t.title_folded, t.artist_name_folded, t.id LIMIT ? OFFSET ?) p \
         JOIN local_tracks t ON t.rowid = p.hit \
         LEFT JOIN local_track_external_identities e \
         ON e.local_track_id = t.id AND ",
        musicbrainz!("e"),
        " LEFT JOIN local_album_external_identities ae \
         ON ae.local_album_id = t.local_album_id AND ",
        musicbrainz!("ae"),
        " LEFT JOIN local_album_artwork w ON w.local_album_id = t.local_album_id \
         ORDER BY p.tier, t.title_folded, t.artist_name_folded, t.id"
    ));
    /// Song hits for a query with no word of 3+ characters: a title prefix
    /// or an exact artist name, each through its own index. The prefix walk
    /// stops after the page's last row (bound as limit plus offset), since
    /// every prefix match outranks every other match and the walk runs in
    /// rank order.
    pub(super) SONG_HITS_SHORT = SONG_START
        + "t.rowid IN (SELECT rowid FROM (SELECT rowid FROM local_tracks \
         WHERE availability = 'indexed' AND title_folded >= ? AND title_folded < ? \
         ORDER BY title_folded, artist_name_folded, id LIMIT ?) \
         UNION SELECT rowid FROM local_tracks \
         WHERE availability = 'indexed' AND COALESCE(artist_name_folded, '') = ?)"
        + SONG_END;
    pub(super) row SongHit for [SONG_HITS_SHORT] {
        id: String,
        title: String,
        title_folded: String,
        artist_name: Option<String>,
        year: Option<i32>,
        local_album_id: String,
        album_title: String,
        disc_number: i64,
        track_number: i64,
        duration_seconds: Option<f64>,
        file_format: String,
        mbid: Option<String>,
        release_group_mbid: Option<String>,
        cover_available: bool,
        own_art: bool,
        artist_id: Option<String>,
    }
    /// Song hits through the FTS index, in `SongHit`'s columns.
    family SONG_HITS_WORDS = song_hits_words;
}

/// `SONG_HITS_WORDS` for a query with `short` words under 3 characters.
pub(super) fn song_hits_words(short: ListLen) -> String {
    format!(
        "{SONG_START}{TRACK_TEXT_MATCH}{}{SONG_END}",
        TRACK_WORD_LIKE.repeat(short.get())
    )
}
