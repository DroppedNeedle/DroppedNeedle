//! The year in review's statements: a person's counted plays in a span of
//! time, the artists they first played inside it, whether they have any
//! history before it, and the names and covers of what tops it.
//!
//! Every statement over the plays reads them through [`COUNTED`], the one
//! rule for which plays a year counts, binding the person and the span as
//! `?1`, `?2` and `?3`. The spans are worked out in Rust in the person's
//! time zone and bound as milliseconds; no statement here does dates.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_library::recording_mbid;

use crate::local::sql::{musicbrainz, owned_album};

droppedneedle_kernel::sql! {
    /// The plays that count, as `h`: the person `?1`'s own rows of the plays
    /// table, played from `?2` up to `?3`. Imported listens, how a playback
    /// ended and play marks are other tables and never count.
    fragment COUNTED = "h.user_id = ?1 AND h.played_at >= ?2 AND h.played_at < ?3";
    /// A placeholder artist row ("Various Artists", unknown), over an
    /// artist alias `r` that may be missing: 0 when it is.
    fragment PLACEHOLDER = "COALESCE(r.kind IN ('various_artists', 'unknown'), 0)";

    /// Every counted play in the span, oldest first, with what the review
    /// keys it by.
    ///
    /// The album is the one its track is on now, else the album the play
    /// names: the rule of `ALBUM_PLAYS` in `local/sql.rs`, the phone apps'
    /// most-played lists, so the counts agree. `album_listed` says whether
    /// that album is owned (live with files), the only albums a list may
    /// show. The recording is the track's (its match, else its tags), else
    /// the one the play carries. The length is the play's, else its track's
    /// current length; NULL when neither is above 0.
    pub(super) YEAR_PLAYS = "SELECT h.played_at AS played_at, \
         h.local_track_id AS track_id, t.title AS track_title, h.track_name AS track_name, \
         a.id AS album_id, a.title AS album_title, \
         COALESCE(" + (owned_album!("a")) + ", 0) AS album_listed, \
         h.local_artist_id AS artist_id, r.display_name AS artist_display, \
         " + PLACEHOLDER + " AS artist_placeholder, \
         ai.provider_artist_id AS artist_mbid, h.artist_name AS artist_name, \
         lower(COALESCE(NULLIF(" + (recording_mbid!("tri", "t")) + ", ''), \
           NULLIF(h.recording_mbid, ''))) AS recording, \
         CASE WHEN h.duration_ms > 0 THEN h.duration_ms \
           WHEN t.duration_seconds > 0 THEN CAST(t.duration_seconds * 1000 AS INTEGER) \
         END AS length_ms \
         FROM library_play_history h \
         LEFT JOIN local_tracks t ON t.id = h.local_track_id \
         LEFT JOIN local_track_external_identities tri \
           ON tri.local_track_id = t.id AND " + (musicbrainz!("tri")) + " \
         LEFT JOIN local_albums a ON a.id = COALESCE(t.local_album_id, h.local_album_id) \
         LEFT JOIN local_artists r ON r.id = h.local_artist_id \
         LEFT JOIN local_artist_external_identities ai \
           ON ai.local_artist_id = h.local_artist_id AND " + (musicbrainz!("ai")) + " \
         WHERE " + COUNTED + " ORDER BY h.played_at";
    pub(crate) row YearPlay for [YEAR_PLAYS] {
        played_at: Millis,
        track_id: Option<String>,
        track_title: Option<String>,
        track_name: String,
        album_id: Option<String>,
        album_title: Option<String>,
        album_listed: bool,
        artist_id: Option<String>,
        artist_display: Option<String>,
        artist_placeholder: bool,
        artist_mbid: Option<String>,
        artist_name: String,
        recording: Option<String>,
        length_ms: Option<i64>,
    }

    /// Every counted play in the span with only what tells its artist,
    /// newest first: the year list's counts. The name is read only for a
    /// play no artist row holds.
    pub(super) PLAY_ARTISTS = "SELECT h.played_at AS played_at, \
         h.local_artist_id AS artist_id, " + PLACEHOLDER + " AS artist_placeholder, \
         CASE WHEN h.local_artist_id IS NULL THEN h.artist_name END AS artist_name \
         FROM library_play_history h \
         LEFT JOIN local_artists r ON r.id = h.local_artist_id \
         WHERE " + COUNTED + " ORDER BY h.played_at DESC";
    pub(super) row PlayArtist for [PLAY_ARTISTS] {
        played_at: Millis,
        artist_id: Option<String>,
        artist_placeholder: bool,
        artist_name: Option<String>,
    }

    /// The artists whose first counted play in the span is at `?4` or
    /// later, with that play's time. Bound from the earliest time there is,
    /// so the first play is over the person's whole history. One grouped
    /// pass over their plays; a probe per artist for an earlier play is
    /// many times slower on a long history.
    pub(super) FIRST_PLAYS = "SELECT h.local_artist_id AS artist_id, \
         MIN(h.played_at) AS first_at FROM library_play_history h \
         WHERE " + COUNTED + " AND h.local_artist_id IS NOT NULL \
         GROUP BY h.local_artist_id HAVING MIN(h.played_at) >= ?4";
    pub(super) row FirstPlay for [FIRST_PLAYS] {
        artist_id: String,
        first_at: Millis,
    }

    /// Whether the person has a counted play in the span, or an imported
    /// listen before its end `?3`: any history before a year, bound from
    /// the earliest time there is to the year's start.
    pub(super) HISTORY_BEFORE = "SELECT EXISTS (SELECT 1 FROM library_play_history h \
         WHERE " + COUNTED + ") OR EXISTS (SELECT 1 FROM media_imported_listens l \
         WHERE l.user_id = ?1 AND l.listened_at < ?3)";
    /// Whether one of the person `?1`'s imported listens before `?3` names
    /// the artist id `?2` (lowercase, as the import keeps it), through the
    /// index over person and artist.
    pub(super) HEARD_BEFORE = "SELECT EXISTS (SELECT 1 FROM media_imported_listens \
         INDEXED BY idx_media_imported_listens_user_artist \
         WHERE user_id = ?1 AND artist_mbid = ?2 AND listened_at < ?3)";

    /// One album's artist name and what its cover hangs on: art of its own
    /// and its release group.
    pub(super) ALBUM_DETAIL = "SELECT COALESCE(an.display_name, a.album_artist_name, '') \
         AS artist_name, COALESCE(w.source IN ('folder', 'embedded'), 0) AS cover_available, \
         ae.release_group_mbid AS release_group_mbid FROM local_albums a \
         LEFT JOIN local_artists an ON an.id = a.album_artist_id \
         LEFT JOIN local_album_external_identities ae \
           ON ae.local_album_id = a.id AND " + (musicbrainz!("ae")) + " \
         LEFT JOIN local_album_artwork w ON w.local_album_id = a.id \
         WHERE a.id = ?1";
    pub(crate) row AlbumDetail for [ALBUM_DETAIL] {
        artist_name: String,
        cover_available: bool,
        release_group_mbid: Option<String>,
    }
}
