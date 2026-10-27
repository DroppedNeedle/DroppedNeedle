//! The local catalog's statements: the library pages, favorites, lookups
//! and the catalog pages' library reads. One projection per entity: an
//! album is [`ALBUM_COLUMNS`] over [`ALBUM_JOINS`], a track
//! [`TRACK_COLUMNS`] over [`TRACK_JOINS`], an artist [`ARTIST_COLUMNS`]
//! over [`ARTIST_JOINS`] with its counts from the grouped passes.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_library::corrections::continuation::CURRENT_TRACK;
use rusqlite::types::Value;

use super::definitions::{
    copy_order, live_album, musicbrainz, owned_album, owned_artist, owned_release_groups,
    recording_mbid,
};
use super::player::{
    AlbumKey, AlbumSequence, AlbumShape, HistoryKey, ItemKind, KeyedBy, PlayedFilter, TextMatch,
    TrackKey, TrackSequence, TrackShape,
};
use super::stores::{AlbumSort, ArtistScope, ArtistSort, TrackSort};

droppedneedle_kernel::sql! {
    /// One album's aggregate columns over [`ALBUM_JOINS`].
    pub(crate) fragment ALBUM_COLUMNS = "a.id AS id, a.title AS title, \
        COALESCE(an.display_name, a.album_artist_name, '') AS artist_name, \
        a.album_artist_id AS artist_id, \
        ae.release_group_mbid AS release_group_mbid, ae.release_mbid AS release_mbid, \
        rte.provider_artist_id AS artist_mbid, \
        (ae.local_album_id IS NOT NULL) AS linked, \
        COUNT(t.id) AS track_count, \
        COALESCE(SUM(t.duration_seconds), 0.0) AS total_duration_seconds, \
        COALESCE(SUM(t.file_size_bytes), 0) AS total_size_bytes, \
        (SELECT t2.file_format FROM local_tracks t2 \
         WHERE t2.local_album_id = a.id AND t2.availability = 'indexed' \
         GROUP BY t2.file_format ORDER BY COUNT(*) DESC, t2.file_format LIMIT 1) AS format, \
        a.year AS year, a.is_compilation AS is_compilation, \
        (w.local_album_id IS NOT NULL) AS cover_available, \
        CASE WHEN w.source IN ('folder','embedded') THEN w.version END AS art_version, \
        a.created_at AS date_added, \
        (SELECT c.id FROM library_contribution_drafts c WHERE c.local_album_id = a.id \
         AND c.state NOT IN ('linked','cancelled','stale')) AS contribution_id, \
        (SELECT c.state FROM library_contribution_drafts c WHERE c.local_album_id = a.id \
         AND c.state NOT IN ('linked','cancelled','stale')) AS contribution_state";
    /// The album projection's joins. Track joins keep indexed tracks only;
    /// a statement over them groups by `a.id`.
    pub(crate) fragment ALBUM_JOINS = (concat!(
        "FROM local_albums a \
         LEFT JOIN local_artists an ON an.id = a.album_artist_id \
         LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id AND ",
        musicbrainz!("ae"),
        " LEFT JOIN local_artist_external_identities rte \
         ON rte.local_artist_id = a.album_artist_id AND ",
        musicbrainz!("rte"),
        " LEFT JOIN local_album_artwork w ON w.local_album_id = a.id \
         LEFT JOIN local_tracks t ON t.local_album_id = a.id AND t.availability = 'indexed'"
    ));
    /// The album list's filters, shared by its page and its count: live
    /// albums, with or without files. Binds: the pattern three times, the
    /// artist twice, the decade three times, the format twice.
    pub(crate) fragment ALBUM_FILTER = (concat!(
        live_album!("a"),
        " AND (? IS NULL OR a.title_folded LIKE ? ESCAPE '\\' \
         OR a.album_artist_name_folded LIKE ? ESCAPE '\\') \
         AND (? IS NULL OR a.album_artist_id = ?) \
         AND (? IS NULL OR (a.year >= ? AND a.year < ? + 10)) \
         AND (? IS NULL OR LOWER((SELECT t2.file_format FROM local_tracks t2 \
         WHERE t2.local_album_id = a.id AND t2.availability = 'indexed' \
         GROUP BY t2.file_format ORDER BY COUNT(*) DESC, t2.file_format LIMIT 1)) = ?)"
    ));
    /// A shuffle seed's three numbers as one row `s` beside the rows a
    /// random list shuffles, from [`shuffle_values`].
    pub(crate) fragment SHUFFLE_SEED = " CROSS JOIN (SELECT ? AS sa, ? AS sb, ? AS sk) s";
    /// One page of the album list's keys, one statement per sort and
    /// direction, from the start or after a cursor.
    family ALBUM_PAGE = album_page;
    /// The album projection for a page of ids.
    family ALBUM_ROWS = album_rows;
    /// The album list's total.
    pub(crate) ALBUM_COUNT = "SELECT COUNT(*) FROM local_albums a WHERE " + ALBUM_FILTER;
    /// One live album by id.
    pub(crate) ALBUM_BY_ID = "SELECT " + ALBUM_COLUMNS + " " + ALBUM_JOINS
        + " WHERE a.id = ? AND " + (live_album!("a")) + " GROUP BY a.id";
    /// Why library management leaves an album's files alone: a person's
    /// exclusion first, else the app's step back.
    pub(crate) MANAGEMENT_HOLD = "SELECT 'your_choice' AS kind, \
        CAST(excluded_at * 1000 AS INTEGER) AS held_at, \
        row_revision AS exclusion_revision, 0 AS handing_back \
        FROM library_management_exclusions WHERE local_album_id = ?1 \
        UNION ALL \
        SELECT cause, noticed_at, NULL, handed_back_at IS NOT NULL \
        FROM library_album_takeovers WHERE local_album_id = ?1 \
        AND NOT EXISTS (SELECT 1 FROM library_management_exclusions \
        WHERE local_album_id = ?1) LIMIT 1";
    /// The live copy one release group names: the oldest with files, else
    /// the oldest.
    pub(crate) ALBUM_BY_GROUP = "SELECT " + ALBUM_COLUMNS + " " + ALBUM_JOINS
        + " WHERE ae.release_group_mbid = ? AND " + (live_album!("a"))
        + " GROUP BY a.id ORDER BY " + (copy_order!("a")) + " LIMIT 1";
    /// Other live albums of an album's release group, oldest first.
    pub(crate) ALBUM_COPIES = "SELECT " + ALBUM_COLUMNS + " " + ALBUM_JOINS
        + " WHERE ae.release_group_mbid = \
        (SELECT i.release_group_mbid FROM local_album_external_identities i \
         WHERE i.local_album_id = ? AND " + (musicbrainz!("i")) + ") \
        AND a.id != ? AND " + (live_album!("a"))
        + " GROUP BY a.id ORDER BY a.created_at ASC, a.id ASC";
    /// A page of the live albums an artist leads, by title.
    family LED_ALBUM_PAGE = led_album_page;
    /// How many live albums an artist leads.
    pub(crate) LED_ALBUM_COUNT = "SELECT COUNT(*) FROM local_albums a \
        WHERE a.album_artist_id = ? AND " + (live_album!("a"));
    /// Live albums an artist (bound twice) appears on without leading,
    /// reached from the artist's credits.
    pub(crate) fragment APPEARANCE_FILTER = (concat!(
        live_album!("a"),
        " AND a.album_artist_id != ? AND a.id IN \
         (SELECT at.local_album_id FROM local_track_artists ata \
         JOIN local_tracks at ON at.id = ata.local_track_id \
         WHERE ata.local_artist_id = ? AND at.availability = 'indexed')"
    ));
    /// A page of an artist's appearances, by title.
    family APPEARANCE_PAGE = appearance_page;
    /// How many albums an artist appears on.
    pub(crate) APPEARANCE_COUNT = "SELECT COUNT(*) FROM local_albums a WHERE "
        + APPEARANCE_FILTER;
    /// The newest owned albums' ids, walking the creation index.
    pub(crate) RECENT_ALBUM_IDS = "SELECT a.id FROM local_albums a WHERE "
        + (owned_album!("a")) + " ORDER BY a.created_at DESC, a.id DESC LIMIT ?";
    /// Owned albums per decade, oldest first; albums without a year sit out.
    pub(crate) DECADES = "SELECT (a.year / 10 * 10) AS decade, COUNT(*) AS album_count \
        FROM local_albums a WHERE a.year IS NOT NULL AND " + (owned_album!("a"))
        + " GROUP BY decade ORDER BY decade ASC";

    /// One track's columns over [`TRACK_JOINS`].
    pub(crate) fragment TRACK_COLUMNS = (concat!(
        "t.id AS id, t.title AS title, \
         t.local_album_id AS album_id, t.album_title AS album_title, \
         COALESCE(t.artist_name, '') AS artist_name, \
         ta.local_artist_id AS artist_id, \
         COALESCE(t.album_artist_name, '') AS album_artist_name, \
         tal.album_artist_id AS album_artist_id, ",
        recording_mbid!("tri", "t"),
        " AS recording_mbid, tgi.release_group_mbid AS release_group_mbid, \
         tai.provider_artist_id AS artist_mbid, tbi.provider_artist_id AS album_artist_mbid, \
         t.disc_number AS disc_number, t.track_number AS track_number, t.year AS year, \
         COALESCE((SELECT g.name FROM local_track_genres g \
         WHERE g.local_track_id = t.id ORDER BY g.position LIMIT 1), t.genre) AS genre, \
         t.duration_seconds AS duration_seconds, t.file_format AS format, \
         t.bit_rate AS bit_rate, t.sample_rate AS sample_rate, \
         t.bit_depth AS bit_depth, t.channels AS channels, \
         t.file_size_bytes AS file_size_bytes, t.imported_at AS date_added, \
         (w.local_album_id IS NOT NULL) AS cover_available"
    ));
    /// The track projection's joins. A statement adds the availability
    /// filter. Every join after the first credit is a primary-key lookup per
    /// row, so pages keep walking their driving index. The track artist's
    /// MusicBrainz id falls back to the album artist's when the track
    /// carries no credit.
    pub(crate) fragment TRACK_JOINS = "FROM local_tracks t \
        LEFT JOIN local_track_artists ta \
            ON ta.local_track_id = t.id AND ta.position = 0 \
        LEFT JOIN local_album_artwork w ON w.local_album_id = t.local_album_id \
        LEFT JOIN local_albums tal ON tal.id = t.local_album_id \
        LEFT JOIN local_track_external_identities tri \
            ON tri.local_track_id = t.id AND tri.provider = 'musicbrainz' \
        LEFT JOIN local_album_external_identities tgi \
            ON tgi.local_album_id = t.local_album_id AND tgi.provider = 'musicbrainz' \
        LEFT JOIN local_artist_external_identities tai \
            ON tai.local_artist_id = COALESCE(ta.local_artist_id, tal.album_artist_id) \
            AND tai.provider = 'musicbrainz' \
        LEFT JOIN local_artist_external_identities tbi \
            ON tbi.local_artist_id = tal.album_artist_id AND tbi.provider = 'musicbrainz'";
    /// The album, artist and genre filters of the track list, after its text
    /// filter: the album twice, the artist twice, the genre three times.
    pub(crate) fragment TRACK_SCOPE = " AND (? IS NULL OR t.local_album_id = ?) \
        AND (? IS NULL OR EXISTS (SELECT 1 FROM local_track_artists x \
        WHERE x.local_track_id = t.id AND x.local_artist_id = ?)) \
        AND (? IS NULL OR EXISTS (SELECT 1 FROM local_track_genres g \
        WHERE g.local_track_id = t.id AND g.folded_name = ?) \
        OR t.genre_folded = ?)";
    /// The track list's text filter as a substring match: the pattern four
    /// times.
    pub(crate) fragment TRACK_LIKE = "t.availability = 'indexed' \
        AND (? IS NULL OR t.title_folded LIKE ? ESCAPE '\\' \
        OR t.artist_name_folded LIKE ? ESCAPE '\\' \
        OR t.album_title_folded LIKE ? ESCAPE '\\')";
    /// Tracks whose text matches an FTS expression. The FTS rowid is the
    /// track's `search_key`.
    pub(crate) fragment TRACK_TEXT_MATCH =
        "t.search_key IN (SELECT rowid FROM local_tracks_fts WHERE local_tracks_fts MATCH ?)";
    /// One short word that must appear in the title, artist or album.
    pub(crate) fragment TRACK_WORD_LIKE = " AND (t.title_folded LIKE ? ESCAPE '\\' \
        OR t.artist_name_folded LIKE ? ESCAPE '\\' OR t.album_title_folded LIKE ? ESCAPE '\\')";
    /// True when any track text matches an FTS expression.
    pub(crate) fragment TRACK_MISS_ORACLE =
        "SELECT EXISTS(SELECT 1 FROM local_tracks_fts WHERE local_tracks_fts MATCH ?)";
    /// The miss oracle: no match means an empty page whatever else narrows.
    pub(crate) TRACK_MISS = TRACK_MISS_ORACLE;
    /// A page of the track list on the substring match.
    family TRACK_PAGE_LIKE = track_page_like;
    /// A page of the track list on the FTS index, with its short words.
    family TRACK_PAGE_WORDS = track_page_words;
    /// The track list's total on the substring match.
    pub(crate) TRACK_COUNT_LIKE = "SELECT COUNT(*) FROM local_tracks t WHERE " + TRACK_LIKE
        + TRACK_SCOPE;
    /// The track list's total on the FTS index.
    family TRACK_COUNT_WORDS = track_count_words;
    /// Every indexed track, from the maintained per-format totals.
    pub(crate) TRACK_TOTAL = "SELECT COALESCE(SUM(indexed_tracks), 0) \
        FROM library_track_format_stats";
    /// One indexed track by the id it has now.
    pub(crate) TRACK_BY_ID = "SELECT " + TRACK_COLUMNS + " " + TRACK_JOINS
        + " WHERE t.id = (" + CURRENT_TRACK + ") AND t.availability = 'indexed'";
    /// Whether one indexed track's file carries lyrics.
    pub(crate) TRACK_HAS_LYRICS = "SELECT EXISTS (SELECT 1 FROM local_tracks t \
        JOIN local_track_lyrics l ON l.local_track_id = t.id \
        WHERE t.id = (" + CURRENT_TRACK + ") AND t.availability = 'indexed')";
    /// A page of an album's indexed tracks, disc and track order.
    pub(crate) ALBUM_TRACKS = "SELECT " + TRACK_COLUMNS + " " + TRACK_JOINS
        + " WHERE t.local_album_id = ? AND t.availability = 'indexed' \
        ORDER BY t.disc_number ASC, t.track_number ASC, t.id ASC LIMIT ? OFFSET ?";
    /// How many indexed tracks an album holds.
    pub(crate) ALBUM_TRACK_COUNT = "SELECT COUNT(*) FROM local_tracks t \
        WHERE t.local_album_id = ? AND t.availability = 'indexed'";
    /// The indexed tracks of several albums, by album, disc and track.
    family ALBUM_TRACKS_IN = album_tracks_in;
    /// One genre (bound twice) on indexed tracks.
    pub(crate) fragment GENRE_FILTER = "t.availability = 'indexed' AND \
        (EXISTS (SELECT 1 FROM local_track_genres g \
         WHERE g.local_track_id = t.id AND g.folded_name = ?) \
         OR t.genre_folded = ?)";
    /// A page of a genre's tracks, artist, album, disc and track order.
    pub(crate) GENRE_TRACKS = "SELECT " + TRACK_COLUMNS + " " + TRACK_JOINS + " WHERE "
        + GENRE_FILTER + " ORDER BY t.artist_name_folded ASC, t.album_title_folded ASC, \
        t.disc_number ASC, t.track_number ASC, t.id ASC LIMIT ? OFFSET ?";
    /// How many tracks a genre holds.
    pub(crate) GENRE_TRACK_COUNT = "SELECT COUNT(*) FROM local_tracks t WHERE " + GENRE_FILTER;
    /// Genres by track count, name tiebreak.
    pub(crate) GENRES = "WITH tagged(track_id, folded, name) AS ( \
        SELECT local_track_id, folded_name, name FROM local_track_genres \
        UNION \
        SELECT id, genre_folded, genre FROM local_tracks \
        WHERE genre_folded IS NOT NULL AND genre_folded != '') \
        SELECT g.folded AS folded_name, MIN(g.name) AS name, \
        COUNT(DISTINCT t.id) AS track_count, \
        COUNT(DISTINCT t.local_album_id) AS album_count \
        FROM tagged g JOIN local_tracks t ON t.id = g.track_id \
        AND t.availability = 'indexed' \
        GROUP BY g.folded ORDER BY track_count DESC, name ASC";
    /// The newest imports.
    pub(crate) NEWEST_TRACKS = "SELECT " + TRACK_COLUMNS + " " + TRACK_JOINS
        + " WHERE t.availability = 'indexed' ORDER BY t.imported_at DESC, t.id ASC LIMIT ?";
    /// The oldest imports.
    pub(crate) OLDEST_TRACKS = "SELECT " + TRACK_COLUMNS + " " + TRACK_JOINS
        + " WHERE t.availability = 'indexed' ORDER BY t.imported_at ASC, t.id ASC LIMIT ?";
    /// A uniform random pick of indexed track ids, optionally from one
    /// decade (bound three times).
    pub(crate) RANDOM_TRACK_IDS = "SELECT t.id FROM local_tracks t \
        WHERE t.availability = 'indexed' \
        AND (? IS NULL OR (t.year >= ? AND t.year < ? + 10)) \
        ORDER BY RANDOM() LIMIT ?";
    /// The track projection for picked ids.
    family TRACKS_IN = tracks_in;

    /// One artist's identity columns over [`ARTIST_JOINS`]. Counts come
    /// from the grouped passes, so a page pays one pass, not a correlated
    /// subquery per row.
    pub(crate) fragment ARTIST_COLUMNS = "r.id AS id, r.display_name AS name, \
        e.provider_artist_id AS artist_mbid, (e.local_artist_id IS NOT NULL) AS linked, \
        r.created_at AS date_added";
    /// The artist projection's joins.
    pub(crate) fragment ARTIST_JOINS = (concat!(
        "FROM local_artists r \
         LEFT JOIN local_artist_external_identities e ON e.local_artist_id = r.id AND ",
        musicbrainz!("e")
    ));
    /// Artists leading at least one owned album.
    pub(crate) fragment LED_PREDICATE = (concat!(
        "EXISTS (SELECT 1 FROM local_albums la WHERE la.album_artist_id = r.id AND ",
        owned_album!("la"),
        ")"
    ));
    /// Artists credited on an owned album or on an indexed track of a live
    /// one, or leading an owned album. The unary plus on `availability`
    /// keeps the track probe on the artist's credits, not a walk over
    /// every indexed track.
    pub(crate) fragment CREDITED_PREDICATE = (concat!(
        "(EXISTS (SELECT 1 FROM local_album_artists laa \
         JOIN local_albums lca ON lca.id = laa.local_album_id \
         WHERE laa.local_artist_id = r.id AND ",
        owned_album!("lca"),
        ") OR EXISTS (SELECT 1 FROM local_track_artists lta \
         JOIN local_tracks lct ON lct.id = lta.local_track_id \
         JOIN local_albums lcb ON lcb.id = lct.local_album_id \
         WHERE lta.local_artist_id = r.id AND +lct.availability = 'indexed' AND ",
        live_album!("lcb"),
        ") OR ",
        "EXISTS (SELECT 1 FROM local_albums la WHERE la.album_artist_id = r.id AND ",
        owned_album!("la"),
        "))"
    ));
    /// Artists leading an album or credited on an indexed track, with the
    /// leading half written `(LED)`.
    pub(crate) fragment WITH_MUSIC_PREDICATE = "((LED) OR EXISTS (SELECT 1 \
        FROM local_track_artists lta JOIN local_tracks lt ON lt.id = lta.local_track_id \
        WHERE lta.local_artist_id = r.id AND lt.availability = 'indexed'))";
    /// One page of artist keys in a scope, one statement per scope, sort
    /// and direction, from the start or after a cursor.
    family ARTIST_PAGE = artist_page;
    /// One page of artist ids at an offset, for the player apps, which
    /// page by offset.
    family ARTIST_OFFSET_PAGE = artist_offset_page;
    /// The artist projection for a page of ids.
    family ARTIST_ROWS = artist_rows;
    /// Owned albums led per artist id.
    family ARTIST_ALBUM_COUNTS = artist_album_counts;
    /// Indexed credited tracks, appearance albums and appearance tracks per
    /// artist id.
    family ARTIST_CREDIT_COUNTS = artist_credit_counts;
    /// The artist list's totals: credited, leading and contributing artists
    /// matching the name filter (bound twice).
    pub(crate) ARTIST_TOTALS = "SELECT \
        COALESCE(SUM(CASE WHEN " + CREDITED_PREDICATE + " THEN 1 ELSE 0 END), 0) AS total, \
        COALESCE(SUM(CASE WHEN " + LED_PREDICATE + " THEN 1 ELSE 0 END), 0) AS led, \
        COALESCE(SUM(CASE WHEN NOT (" + LED_PREDICATE + ") AND (" + CREDITED_PREDICATE
        + ") THEN 1 ELSE 0 END), 0) AS contributors " + ARTIST_JOINS
        + " WHERE r.retired_into_artist_id IS NULL \
        AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\')";
    /// How many artists of one scope match the name filter.
    family ARTIST_SCOPE_COUNT = artist_scope_count;
    /// One live artist by id.
    pub(crate) ARTIST_BY_ID = "SELECT " + ARTIST_COLUMNS + " " + ARTIST_JOINS
        + " WHERE r.id = ? AND r.retired_into_artist_id IS NULL";

    /// Library totals: owned albums, every credited artist, and the
    /// maintained track totals.
    pub(crate) STATS = "SELECT (SELECT COUNT(*) FROM local_albums a WHERE "
        + (owned_album!("a")) + ") AS albums, \
        (SELECT COUNT(*) FROM local_artists r \
         WHERE r.retired_into_artist_id IS NULL AND " + CREDITED_PREDICATE + ") AS artists, \
        (SELECT COALESCE(SUM(indexed_tracks), 0) \
         FROM library_track_format_stats) AS tracks, \
        (SELECT COALESCE(SUM(indexed_bytes), 0) \
         FROM library_track_format_stats) AS size_bytes";
    /// Indexed tracks, and the folders holding at least one, across every
    /// library folder. `rtrim` with the path's own non-slash characters
    /// cuts the file name off, leaving the folder.
    pub(crate) PLAYABLE_COUNTS = "SELECT COALESCE(SUM(tracks), 0), COUNT(*) FROM \
        (SELECT COUNT(*) AS tracks FROM local_tracks WHERE availability = 'indexed' \
         GROUP BY root_id, rtrim(relative_path, replace(relative_path, '/', '')))";
    /// Indexed tracks per format, the zero rows left out as GROUP BY would.
    pub(crate) STATS_FORMATS = "SELECT file_format AS format, indexed_tracks AS total \
        FROM library_track_format_stats WHERE indexed_tracks > 0";
    /// Review, local-only and last-scan totals. Local-only albums are owned
    /// albums with no identity row; the last scan is the newest completed
    /// run.
    pub(crate) STATS_EXTRAS = "SELECT \
        (SELECT COUNT(*) FROM library_identify_reviews WHERE state = 'pending') AS reviews, \
        (SELECT COUNT(*) FROM local_albums a WHERE " + (owned_album!("a"))
        + " AND NOT EXISTS (SELECT 1 FROM local_album_external_identities i \
        WHERE i.local_album_id = a.id AND " + (musicbrainz!("i")) + ")) AS local_only, \
        (SELECT MAX(terminal_at) FROM library_scan_runs WHERE state = 'completed') AS last_scan";

    /// Which of a page of ids a user favorited, for one kind.
    family FAVORITES_IN = favorites_in;
    /// A user's favorite counts per kind.
    pub(crate) FAVORITE_COUNTS = "SELECT item_kind AS kind, COUNT(*) AS total \
        FROM library_user_favorites WHERE user_id = ? GROUP BY item_kind";

    /// The live album a local id or alias names now: the album itself while
    /// it is live, else the album its alias points at (every merge points
    /// older aliases at the survivor), else the album it was retired into.
    pub(crate) CURRENT_ALBUM = "SELECT id FROM ( \
        SELECT id, 0 AS rank FROM local_albums WHERE id = ?1 AND retired_into_album_id IS NULL \
        UNION ALL SELECT b.id, 1 FROM local_album_aliases x \
         JOIN local_albums b ON b.id = x.local_album_id \
         WHERE x.alias = lower(?1) AND b.retired_into_album_id IS NULL \
        UNION ALL SELECT b.id, 2 FROM local_albums a \
         JOIN local_albums b ON b.id = a.retired_into_album_id \
         WHERE a.id = ?1 AND b.retired_into_album_id IS NULL) \
        ORDER BY rank LIMIT 1";
    /// [`CURRENT_ALBUM`] for a list of identifiers at once: each asked
    /// identifier with the album it names now, or NULL.
    family CURRENT_ALBUMS = current_albums;
    /// The live artist a local id or alias names now, in the same order as
    /// [`CURRENT_ALBUM`]. An artist merge never repoints
    /// `retired_into_artist_id` of artists merged earlier, so the alias
    /// comes before it.
    pub(crate) CURRENT_ARTIST = "SELECT id FROM ( \
        SELECT id, 0 AS rank FROM local_artists WHERE id = ?1 AND retired_into_artist_id IS NULL \
        UNION ALL SELECT r.id, 1 FROM local_artist_aliases x \
         JOIN local_artists r ON r.id = x.local_artist_id \
         WHERE x.alias = lower(?1) AND r.retired_into_artist_id IS NULL \
        UNION ALL SELECT r.id, 2 FROM local_artists a \
         JOIN local_artists r ON r.id = a.retired_into_artist_id \
         WHERE a.id = ?1 AND r.retired_into_artist_id IS NULL) \
        ORDER BY rank LIMIT 1";
    /// The track a local id names now, or the id itself.
    pub(crate) CURRENT_TRACK_ID = CURRENT_TRACK;
    /// The live artist linked to one MusicBrainz artist id, by the lowercase
    /// expression index.
    pub(crate) ARTIST_FOR_MBID = "SELECT r.id FROM local_artist_external_identities e \
        JOIN local_artists r ON r.id = e.local_artist_id \
        WHERE e.provider = 'musicbrainz' AND lower(e.provider_artist_id) = lower(?1) \
        AND r.retired_into_artist_id IS NULL ORDER BY r.id LIMIT 1";
    /// Owned albums holding one of the release groups: album id and the
    /// lowercase group.
    family OWNERS_BY_GROUP = owners_by_group;
    /// Owned albums identified as one of the releases: album id and the
    /// lowercase release.
    family OWNERS_BY_RELEASE = owners_by_release;
    /// The live copy of one release group (`?1`, lowercase) when no copy is
    /// owned: the oldest copy with files, else the oldest copy.
    pub(crate) LIVE_COPY_BY_GROUP = "SELECT b.id FROM local_album_external_identities e \
        JOIN local_albums b ON b.id = e.local_album_id \
        WHERE lower(e.release_group_mbid) = ?1 AND " + (musicbrainz!("e")) + " AND "
        + (live_album!("b")) + " ORDER BY " + (copy_order!("b")) + " LIMIT 1";

    /// Every identified artist credited on an owned album: lowercase MBID
    /// and display name, by name.
    pub(crate) IDENTIFIED_ARTISTS = "SELECT lower(e.provider_artist_id), \
        MIN(a.display_name) FROM local_artist_external_identities e \
        JOIN local_artists a ON a.id = e.local_artist_id \
        JOIN local_album_artists credit ON credit.local_artist_id = e.local_artist_id \
        JOIN local_albums b ON b.id = credit.local_album_id \
        WHERE a.retired_into_artist_id IS NULL AND " + (musicbrainz!("e")) + " AND "
        + (owned_album!("b")) + " GROUP BY lower(e.provider_artist_id) ORDER BY 2";
    /// Every identified owned album: lowercase release group, title and
    /// album artist name.
    pub(crate) IDENTIFIED_ALBUMS = "SELECT lower(e.release_group_mbid), MIN(b.title), \
        MIN(COALESCE(b.album_artist_name, '')) \
        FROM local_album_external_identities e \
        JOIN local_albums b ON b.id = e.local_album_id WHERE " + (musicbrainz!("e"))
        + " AND " + (owned_album!("b")) + " GROUP BY lower(e.release_group_mbid) ORDER BY 3, 2";
    /// Owned artists among a list of lowercase MBIDs.
    family OWNED_ARTISTS_IN = owned_artists_in;
    /// Whether an artist (lowercase MBID) only appears on the library's
    /// tracks, with no album credit of their own on that album. Tracks are
    /// reached from the artist's credits by key: with no planner statistics
    /// a walk of every indexed track looks as good.
    pub(crate) ARTIST_APPEARS = "SELECT 1 FROM local_artist_external_identities e \
        JOIN local_artists a ON a.id = e.local_artist_id \
        JOIN local_track_artists credit ON credit.local_artist_id = e.local_artist_id \
        JOIN local_tracks t INDEXED BY sqlite_autoindex_local_tracks_1 \
        ON t.id = credit.local_track_id \
        JOIN local_albums b ON b.id = t.local_album_id \
        WHERE lower(e.provider_artist_id) = ? AND " + (musicbrainz!("e"))
        + " AND a.retired_into_artist_id IS NULL AND " + (live_album!("b"))
        + " AND t.availability = 'indexed' AND NOT EXISTS ( \
        SELECT 1 FROM local_album_artists album_credit \
        WHERE album_credit.local_album_id = t.local_album_id \
        AND album_credit.local_artist_id = credit.local_artist_id) LIMIT 1";
    /// Every owned release group, lowercase and sorted.
    pub(crate) OWNED_GROUPS = "SELECT DISTINCT * FROM (" + (owned_release_groups!())
        + ") ORDER BY 1";
    /// Request statuses that count as requested on catalog pages: open, not
    /// finished.
    pub(crate) fragment OPEN_REQUEST = "request_kind = 'album' \
        AND status IN ('pending','downloading','queued','awaiting_approval')";
    /// Every album MBID with an open request, sorted.
    pub(crate) REQUESTED_GROUPS = "SELECT DISTINCT musicbrainz_id_lower FROM request_history \
        WHERE " + OPEN_REQUEST + " ORDER BY 1";
    /// The album MBIDs among a list with an open request.
    family REQUESTED_IN = requested_in;
    /// The live local artist behind one lowercase MBID.
    pub(crate) LOCAL_ARTIST = "SELECT a.display_name FROM local_artist_external_identities e \
        JOIN local_artists a ON a.id = e.local_artist_id \
        WHERE e.provider = 'musicbrainz' AND a.retired_into_artist_id IS NULL \
        AND lower(e.provider_artist_id) = ? LIMIT 1";
    /// The identified owned albums one artist MBID leads, newest first.
    pub(crate) LOCAL_ARTIST_ALBUMS = "SELECT e.release_group_mbid, b.title, b.year \
        FROM local_artist_external_identities ae \
        JOIN local_albums b ON b.album_artist_id = ae.local_artist_id \
        JOIN local_album_external_identities e ON e.local_album_id = b.id AND "
        + (musicbrainz!("e")) + " WHERE lower(ae.provider_artist_id) = ? AND "
        + (musicbrainz!("ae")) + " AND " + (owned_album!("b"))
        + " ORDER BY b.year IS NULL, b.year DESC, b.title_folded";
    /// The live copy of one release group (lowercase), with or without
    /// files, for the catalog pages: its id, title, tagged album artist,
    /// year, album artist MBID and whether it has files.
    pub(crate) LOCAL_ALBUM = "SELECT b.id, b.title, COALESCE(b.album_artist_name, ''), b.year, \
        (SELECT ae.provider_artist_id FROM local_artist_external_identities ae \
         WHERE ae.local_artist_id = b.album_artist_id AND " + (musicbrainz!("ae"))
        + "), b.has_indexed_track \
        FROM local_album_external_identities e \
        JOIN local_albums b ON b.id = e.local_album_id \
        WHERE lower(e.release_group_mbid) = ? AND " + (musicbrainz!("e")) + " AND "
        + (live_album!("b")) + " ORDER BY " + (copy_order!("b")) + " LIMIT 1";
    /// The catalog pages' tracks of one album: disc, track and title order.
    pub(crate) LOCAL_ALBUM_TRACKS = "SELECT " + TRACK_COLUMNS + " " + TRACK_JOINS
        + " WHERE t.local_album_id = ? AND t.availability = 'indexed' \
        ORDER BY t.disc_number, t.track_number, t.title_folded";
    /// The catalog revision: moves with every write to the catalog, so a
    /// total counted at one revision holds until it moves.
    pub(crate) CATALOG_REVISION = "SELECT COALESCE((SELECT value \
        FROM library_catalog_revision WHERE singleton = 1), 0)";
    /// The release group a library album, or the album it merged into, is
    /// identified as.
    pub(crate) GROUP_FOR_ALBUM = "SELECT e.release_group_mbid FROM local_albums b \
        JOIN local_album_external_identities e \
          ON e.local_album_id = COALESCE(b.retired_into_album_id, b.id) AND "
        + (musicbrainz!("e")) + " WHERE b.id = ? LIMIT 1";

    /// A player track's columns over [`PLAYER_TRACK_JOINS`]: every tag a
    /// player shows, all genres joined by the unit separator.
    pub(crate) fragment PLAYER_TRACK_COLUMNS = (concat!(
        "t.id AS id, t.title AS title, t.local_album_id AS album_id, \
         t.album_title AS album_title, COALESCE(t.artist_name, '') AS artist_name, \
         ta.local_artist_id AS artist_id, a.album_artist_id AS album_artist_id, \
         COALESCE(an.display_name, t.album_artist_name, a.album_artist_name) AS album_artist_name, \
         t.disc_number AS disc_number, t.track_number AS track_number, t.year AS year, \
         (SELECT group_concat(name, char(31)) FROM (SELECT g.name AS name FROM local_track_genres g \
         WHERE g.local_track_id = t.id ORDER BY g.position)) AS genres, \
         t.genre AS tag_genre, t.duration_seconds AS duration_seconds, \
         LOWER(t.file_format) AS format, t.bit_rate AS bit_rate, t.sample_rate AS sample_rate, \
         t.bit_depth AS bit_depth, t.channels AS channels, t.file_size_bytes AS file_size_bytes, \
         t.relative_path AS relative_path, \
         t.imported_at AS date_added, t.title_sort AS sort_name, t.disc_subtitle AS disc_subtitle, ",
        recording_mbid!("te", "t"),
        " AS recording_mbid, \
         tae.provider_artist_id AS artist_mbid, aae.provider_artist_id AS album_artist_mbid, \
         ae.release_group_mbid AS release_group_mbid, t.release_type AS release_type, \
         t.replaygain_track_gain AS rg_track_gain, t.replaygain_album_gain AS rg_album_gain, \
         t.replaygain_track_peak AS rg_track_peak, t.replaygain_album_peak AS rg_album_peak, \
         EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id) \
         AS cover_available, \
         (SELECT w.version FROM local_album_artwork w WHERE w.local_album_id = a.id \
         AND w.source IN ('folder','embedded')) AS art_version, \
         EXISTS (SELECT 1 FROM local_track_lyrics l WHERE l.local_track_id = t.id) AS has_lyrics"
    ));
    /// The player track projection's joins.
    pub(crate) fragment PLAYER_TRACK_JOINS = (concat!(
        "FROM local_tracks t \
         JOIN local_albums a ON a.id = t.local_album_id \
         LEFT JOIN local_artists an ON an.id = a.album_artist_id \
         LEFT JOIN local_track_artists ta ON ta.local_track_id = t.id AND ta.position = 0 \
         LEFT JOIN local_track_external_identities te ON te.local_track_id = t.id AND ",
        musicbrainz!("te"),
        " LEFT JOIN local_artist_external_identities tae \
         ON tae.local_artist_id = ta.local_artist_id AND ",
        musicbrainz!("tae"),
        " LEFT JOIN local_artist_external_identities aae \
         ON aae.local_artist_id = a.album_artist_id AND ",
        musicbrainz!("aae"),
        " LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id AND ",
        musicbrainz!("ae")
    ));
    /// The player projection for some streamable track ids.
    family PLAYER_TRACKS_IN = player_tracks_in;
    /// One genre on track `t`, folded: the genre twice.
    pub(crate) fragment TRACK_GENRE_MATCH = "(EXISTS (SELECT 1 FROM local_track_genres g \
        WHERE g.local_track_id = t.id AND g.folded_name = ?) OR t.genre_folded = ?)";
    /// The player track filters that never steer a plan, written only when
    /// one of them is set, each skipped when its first bind is NULL: the
    /// credited artists as a JSON list twice, the genre three times, the
    /// exact artist name three times.
    pub(crate) fragment PLAYER_TRACK_SCOPE = " AND (? IS NULL OR EXISTS (SELECT 1 \
        FROM local_track_artists x WHERE x.local_track_id = t.id \
        AND x.local_artist_id IN (SELECT value FROM json_each(?)))) \
        AND (? IS NULL OR " + TRACK_GENRE_MATCH + ") \
        AND (? IS NULL OR t.artist_name_folded = ? OR t.album_artist_name_folded = ?)";
    /// The short words of a search (a JSON list of LIKE patterns), each in
    /// the title, artist or album. A word one column leaves unknown (NULL)
    /// and the others miss is missing, as it is in [`TRACK_WORD_LIKE`].
    pub(crate) fragment PLAYER_SHORT_WORDS = " AND NOT EXISTS (SELECT 1 FROM json_each(?) w \
        WHERE (t.title_folded LIKE w.value ESCAPE '\\' \
        OR t.artist_name_folded LIKE w.value ESCAPE '\\' \
        OR t.album_title_folded LIKE w.value ESCAPE '\\') IS NOT 1)";
    /// A seeded shuffle key over the track `t`, as the library list's
    /// shuffle works it out, with the seed's three numbers from
    /// [`shuffle_values`] bound as the statement's first three values, so
    /// the key reads no other row.
    pub(crate) fragment PLAYER_SHUFFLE_KEY = "((((t.search_key * ?1 + ?2) % 2147483647) \
        * ((t.search_key * ?1 + ?2) % 2147483647)) % 2147483647 * ?3 \
        + ((t.search_key * ?1 + ?2) % 2147483647)) % 2147483647";
    /// Every streamable track in insertion (`search_key`) order, one page
    /// at an offset: the full-library sync. The keys come from the index
    /// over availability and the time a track went missing, which holds
    /// them in order for each time; nearly every streamable track has
    /// none, and the few that have one are merged in. Only the page's rows
    /// are read for their ids. Binds: limit, offset.
    pub(crate) fragment PLAYER_NATURAL_PAGE = "SELECT pt.id FROM (\
        SELECT t.search_key AS k, t.rowid AS r FROM local_tracks t \
        WHERE t.availability = 'indexed' AND t.missing_since IS NULL \
        UNION ALL SELECT t.search_key, t.rowid FROM local_tracks t \
        WHERE t.availability = 'indexed' AND t.missing_since IS NOT NULL \
        ORDER BY 1 LIMIT ? OFFSET ?) p CROSS JOIN local_tracks pt \
        WHERE pt.rowid = p.r ORDER BY p.k";
    /// One page of player track ids in a catalog order, over the whole
    /// library.
    family PLAYER_TRACK_PAGE = player_track_page;
    /// One page of one album's player track ids, with or without an exact
    /// title.
    family PLAYER_ALBUM_TRACK_PAGE = player_album_track_page;
    /// One page of the player track ids with an exact title.
    family PLAYER_TITLE_TRACK_PAGE = player_title_track_page;
    /// How many tracks pass the player filters, over the whole library.
    family PLAYER_TRACK_COUNT = player_track_count;
    /// How many tracks of one album or one exact title pass the filters.
    family PLAYER_KEYED_TRACK_COUNT = player_keyed_track_count;
    /// How many of a JSON list of track ids are streamable.
    pub(crate) PLAYER_LISTED_TRACK_COUNT = "SELECT COUNT(*) FROM json_each(?) p \
        CROSS JOIN local_tracks t WHERE t.id = p.value AND +t.availability = 'indexed'";
    /// The listener's played tracks; see [`player_track_values`]. Binds: the
    /// listener three times.
    pub(crate) PLAYER_PLAYED_TRACKS = "SELECT h.local_track_id AS id FROM " + PLAYS
        + " WHERE h.user_id = ? AND h.local_track_id IS NOT NULL \
        EXCEPT SELECT m.local_track_id FROM " + MARKS
        + " WHERE m.user_id = ? AND m.marked_unplayed_at IS NOT NULL \
        AND NOT EXISTS (SELECT 1 FROM " + PLAYS + " WHERE h.user_id = m.user_id \
        AND h.local_track_id = m.local_track_id AND " + AFTER_MARK + ") \
        UNION SELECT m.local_track_id FROM " + MARKS
        + " WHERE m.user_id = ? AND m.marked_played_at IS NOT NULL";
    /// The listener's played values for some track ids.
    family PLAYER_TRACK_VALUES = player_track_values;
    /// A play-history page of the played tracks.
    family PLAYER_TRACKS_PLAYED = player_tracks_played;
    /// A play-history page of the unplayed tracks, over the whole library.
    family PLAYER_TRACKS_UNPLAYED = player_tracks_unplayed;
    /// A play-history page of the unplayed tracks of one album or one exact
    /// title.
    family PLAYER_KEYED_TRACKS_UNPLAYED = player_keyed_tracks_unplayed;
    /// How many played tracks pass the filters.
    family PLAYER_TRACKS_PLAYED_COUNT = player_tracks_played_count;
    /// How many unplayed tracks pass the filters, over the whole library.
    family PLAYER_TRACKS_UNPLAYED_COUNT = player_tracks_unplayed_count;
    /// How many unplayed tracks of one album or one exact title pass the
    /// filters.
    family PLAYER_KEYED_TRACKS_UNPLAYED_COUNT = player_keyed_tracks_unplayed_count;

    /// The listener's marks as `m`, read through their primary key: with
    /// statistics over one listener's marks the planner would otherwise walk
    /// the table, or reach a mark from its track's index.
    pub(crate) fragment MARKS =
        "library_user_play_marks m INDEXED BY sqlite_autoindex_library_user_play_marks_1";
    /// The plays, as `h`, through the index over listener, track and time.
    pub(crate) fragment PLAYS = "library_play_history h INDEXED BY idx_play_history_user_track";
    /// The time of the play `h` in whole seconds. `played_at` is ISO text
    /// whose form varies (v2 rows), so it is read as seconds, never compared
    /// as text; one SQLite cannot read gives NULL. `unixepoch` gives what
    /// `CAST(strftime('%s', ...) AS INTEGER)` gives at a fraction of the
    /// cost.
    pub(crate) fragment PLAY_SECONDS = "unixepoch(h.played_at)";
    /// Whether the play `h` is later than the unplayed mark `m`, in whole
    /// seconds. A time SQLite cannot read is never later.
    pub(crate) fragment AFTER_MARK = "unixepoch(h.played_at) > m.marked_unplayed_at / 1000";
    /// One listener's plays per album, one row each: `id`, `plays` and
    /// `played_at`, the latest. Subsonic's recently and most played album
    /// lists read them here; marks do not weigh in. Binds: the listener
    /// twice.
    ///
    /// A play counts for the album its track is on now, streamable or not,
    /// so a track that moved takes its plays with it; a play that names an
    /// album and no track counts for that album; a play that names neither
    /// counts for none. The album id stored on a play with a track is what
    /// it was filed under at the time and is not read.
    ///
    /// Two halves, each from its own index, so the cost is the plays that
    /// count for an album, never the listener's whole history. The plays
    /// with a track come from the index over listener, track and time
    /// alone, grouped per track, then each track's album. The plays with an
    /// album and no track come from the small index that holds only those.
    pub(crate) fragment ALBUM_PLAYS = "SELECT id, SUM(plays) AS plays, \
        MAX(played_at) AS played_at FROM (\
        SELECT pt.local_album_id AS id, p.plays AS plays, p.played_at AS played_at \
        FROM (SELECT local_track_id, COUNT(*) AS plays, MAX(played_at) AS played_at \
        FROM library_play_history INDEXED BY idx_play_history_user_track \
        WHERE user_id = ? AND local_track_id IS NOT NULL GROUP BY local_track_id) p \
        CROSS JOIN local_tracks pt WHERE pt.id = p.local_track_id \
        UNION ALL \
        SELECT local_album_id, COUNT(*), MAX(played_at) \
        FROM library_play_history INDEXED BY idx_play_history_user_album_no_track \
        WHERE user_id = ? AND local_track_id IS NULL AND local_album_id IS NOT NULL \
        GROUP BY local_album_id) GROUP BY id";

    /// Album extras appended to the shared album columns. The release date
    /// is kept only when every indexed track has the same one, as Navidrome
    /// does.
    pub(crate) fragment ALBUM_EXTRAS = "a.primary_genre AS primary_genre, \
        a.album_artist_sort_name AS album_sort_name, \
        a.original_release_date AS original_release_date, \
        CASE WHEN COUNT(h.release_date) = COUNT(t.id) \
         AND MIN(h.release_date) = MAX(h.release_date) \
         THEN MIN(h.release_date) END AS release_date";
    /// The edition hints of the tracks [`ALBUM_JOINS`] joins, at most one
    /// row each, for the release date in [`ALBUM_EXTRAS`].
    pub(crate) fragment ALBUM_EXTRA_JOINS =
        "LEFT JOIN local_track_edition_tags h ON h.local_track_id = t.id";
    /// The player projection for some owned album ids.
    family PLAYER_ALBUMS_IN = player_albums_in;
    /// The player album filters that never steer a plan, written only when
    /// one of them is set, each skipped when its first bind is NULL: the
    /// title or artist pattern three times, the lowest and highest year
    /// twice each.
    pub(crate) fragment PLAYER_ALBUM_SCOPE = " AND (? IS NULL \
        OR a.title_folded LIKE ? ESCAPE '\\' OR a.album_artist_name_folded LIKE ? ESCAPE '\\') \
        AND (? IS NULL OR a.year >= ?) AND (? IS NULL OR a.year <= ?)";
    /// Albums some artists (a JSON list, bound twice) are credited on
    /// without leading.
    pub(crate) fragment PLAYER_APPEARS_ON = " AND a.album_artist_id NOT IN \
        (SELECT value FROM json_each(?)) AND EXISTS (SELECT 1 FROM local_tracks t \
        JOIN local_track_artists x ON x.local_track_id = t.id \
        WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
        AND x.local_artist_id IN (SELECT value FROM json_each(?)))";
    /// Albums with a streamable track of one genre (bound twice).
    pub(crate) fragment PLAYER_ALBUM_GENRE = " AND EXISTS (SELECT 1 FROM local_tracks t \
        WHERE t.local_album_id = a.id AND t.availability = 'indexed' AND "
        + TRACK_GENRE_MATCH + ")";
    /// One page of player album ids in a catalog order.
    family PLAYER_ALBUM_PAGE = player_album_page;
    /// How many albums pass the player filters.
    family PLAYER_ALBUM_COUNT = player_album_count;
    /// The listener's played albums, from the album play summary (migration
    /// 0065).
    pub(crate) PLAYER_PLAYED_ALBUMS = "SELECT local_album_id FROM library_user_album_plays \
        WHERE user_id = ? AND played = 1";
    /// The albums with a play count above 0 for the listener.
    pub(crate) PLAYER_ALBUM_PLAYED_IDS = "SELECT local_album_id FROM library_user_album_plays \
        WHERE user_id = ?";
    /// The listener's values for some album ids, from the album play
    /// summary.
    family PLAYER_ALBUM_VALUES = player_album_values;
    /// A play-history page of the played albums.
    family PLAYER_ALBUMS_PLAYED = player_albums_played;
    /// A play-history page of the unplayed albums.
    family PLAYER_ALBUMS_UNPLAYED = player_albums_unplayed;
    /// How many played albums pass the filters.
    family PLAYER_ALBUMS_PLAYED_COUNT = player_albums_played_count;
    /// How many unplayed albums pass the filters.
    family PLAYER_ALBUMS_UNPLAYED_COUNT = player_albums_unplayed_count;
    /// Subsonic's recently or most played albums.
    family PLAYER_HISTORY_ALBUMS = player_history_albums;
    /// One listener's play count and last play for some items of one kind.
    family PLAYER_PLAY_STATS = player_play_stats;
    /// Which of some ids of one kind exist for a player.
    family PLAYER_EXISTING = player_existing;
    /// The accepted and tag ids of some albums.
    family PLAYER_ALBUM_IDS = player_album_ids;
    /// The accepted MusicBrainz ids of some artists.
    family PLAYER_ARTIST_IDS = player_artist_ids;
    /// The tag ids of some artists.
    family ARTIST_TAG_IDS = artist_tag_ids;
    /// One page of album artist ids by name or in insertion order.
    family PLAYER_ALBUM_ARTIST_PAGE = player_album_artist_page;
    /// Every year an owned album has, ascending.
    pub(crate) PLAYER_ALBUM_YEARS = "SELECT DISTINCT a.year FROM local_albums a \
        WHERE a.year IS NOT NULL AND " + (owned_album!("a")) + " ORDER BY a.year";
    /// The summed length and count of the streamable tracks crediting some
    /// artists.
    family PLAYER_ARTIST_DURATIONS = player_artist_durations;
}

/// `?, ?, ...` for a list of `len` values.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// One column of a list's order: its expression and whether it runs
/// descending. Every order ends in the row's id, so it is total and a
/// cursor names one place in it.
pub(crate) type OrderKey = (&'static str, bool);

/// A seeded shuffle key over a row's integer key (an album's rowid, a
/// track's `search_key`): `h = (key * a + b) mod p`, then
/// `(k * h^2 + h) mod p`, with `p` the prime 2^31 - 1, above any realistic
/// key so every product stays inside 64 bits. The square makes it
/// non-linear, so neighbouring rows land far apart. One seed is one fixed
/// order.
macro_rules! shuffle_key {
    ($key:literal) => {
        concat!(
            "((((",
            $key,
            " * s.sa + s.sb) % 2147483647) * ((",
            $key,
            " * s.sa + s.sb) % 2147483647)) % 2147483647 * s.sk + ((",
            $key,
            " * s.sa + s.sb) % 2147483647)) % 2147483647"
        )
    };
}

/// The binds of [`SHUFFLE_SEED`] for one seed.
pub(crate) fn shuffle_values(seed: u32) -> [Value; 3] {
    let prime: i64 = 2_147_483_647;
    let seed = i64::from(seed);
    [
        Value::Integer(seed % (prime - 1) + 1),
        Value::Integer((seed * 7919) % prime),
        Value::Integer((seed * 104_729) % (prime - 1) + 1),
    ]
}

/// The key columns a page selects, `k0`, `k1`, ..., in order.
fn key_columns(keys: &[OrderKey]) -> String {
    keys.iter()
        .enumerate()
        .map(|(index, (expr, _))| format!("{expr} AS k{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `ORDER BY` list of `keys`.
fn order_by(keys: &[OrderKey]) -> String {
    keys.iter()
        .map(|(expr, descending)| format!("{expr} {}", if *descending { "DESC" } else { "ASC" }))
        .collect::<Vec<_>>()
        .join(", ")
}

/// True when every key runs one way.
fn one_direction(keys: &[OrderKey]) -> bool {
    keys.iter().all(|(_, descending)| *descending == keys[0].1)
}

/// True when the order is led by a `COALESCE(...)` expression. An index on
/// an expression is sought only by a plain comparison, never by a row
/// value, so such an order also takes a leading bound on that key.
fn expression_led(keys: &[OrderKey]) -> bool {
    keys[0].0.starts_with("COALESCE(")
}

/// The rows after a position in `keys` order, as ` AND ...`, or nothing for
/// the first page. One direction throughout is one row-value comparison,
/// which an index over those columns serves; mixed directions are spelled
/// out, with a leading bound on the first key. Bind [`after_values`].
fn after(keys: &[OrderKey], after_cursor: bool) -> String {
    if !after_cursor {
        return String::new();
    }
    if one_direction(keys) {
        let columns: Vec<&str> = keys.iter().map(|(expr, _)| *expr).collect();
        let (first, descending) = keys[0];
        let op = if descending { "<" } else { ">" };
        let bound = if expression_led(keys) {
            format!(" AND {first} {op}= ?")
        } else {
            String::new()
        };
        return format!(
            "{bound} AND ({}) {op} ({})",
            columns.join(", "),
            vec!["?"; keys.len()].join(", ")
        );
    }
    let (first, descending) = keys[0];
    format!(
        " AND {first} {} ? AND {}",
        if descending { "<=" } else { ">=" },
        spelled(keys)
    )
}

/// `k1 > ? OR (k1 = ? AND (k2 > ? OR ...))`, each key in its direction.
fn spelled(keys: &[OrderKey]) -> String {
    let (expr, descending) = keys[0];
    let op = if descending { "<" } else { ">" };
    if keys.len() == 1 {
        return format!("{expr} {op} ?");
    }
    format!(
        "({expr} {op} ? OR ({expr} = ? AND {}))",
        spelled(&keys[1..])
    )
}

/// The binds of [`after`] for the last row's `key`.
pub(crate) fn after_values(keys: &[OrderKey], key: &[Value]) -> Vec<Value> {
    if one_direction(keys) {
        let mut values = Vec::with_capacity(key.len() + 1);
        if expression_led(keys) {
            values.extend(key.first().cloned());
        }
        values.extend_from_slice(key);
        return values;
    }
    let mut values = Vec::with_capacity(key.len() * 2);
    values.extend(key.first().cloned());
    for (index, value) in key.iter().enumerate() {
        values.push(value.clone());
        if index + 1 < key.len() {
            values.push(value.clone());
        }
    }
    values
}

/// The album list's order for one sort and direction. Ties fall the way
/// the order runs.
pub(crate) fn album_keys(sort: AlbumSort, descending: bool) -> Vec<OrderKey> {
    let d = descending;
    match sort {
        AlbumSort::Name => vec![("a.title_folded", d), ("a.id", d)],
        AlbumSort::DateAdded | AlbumSort::Rediscover => {
            vec![("a.created_at", d), ("a.id", d)]
        }
        AlbumSort::Year => vec![
            ("(a.year IS NULL)", false),
            ("COALESCE(a.year, 0)", d),
            ("a.id", d),
        ],
        AlbumSort::Artist => vec![
            ("COALESCE(a.album_artist_name_folded, '')", d),
            ("a.title_folded", d),
            ("a.id", d),
        ],
        AlbumSort::Random => vec![(shuffle_key!("a.rowid"), false), ("a.id", false)],
    }
}

/// One page of album keys: filtering and ordering touch albums only, so the
/// track aggregation runs over the page, never the catalog. A random page
/// binds the seed first, then the filter, the cursor and the limit.
pub(crate) fn album_page(sort: AlbumSort, descending: bool, after_cursor: bool) -> String {
    let keys = album_keys(sort, descending);
    let seed = if sort == AlbumSort::Random {
        SHUFFLE_SEED
    } else {
        ""
    };
    format!(
        "SELECT {} FROM local_albums a{seed} WHERE {ALBUM_FILTER}{} ORDER BY {} LIMIT ?",
        key_columns(&keys),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// The order of an artist's led albums and appearances: by title.
pub(crate) fn title_keys() -> Vec<OrderKey> {
    vec![("a.title_folded", false), ("a.id", false)]
}

/// A page of the live albums an artist leads: the artist, the cursor, the
/// limit.
pub(crate) fn led_album_page(after_cursor: bool) -> String {
    let keys = title_keys();
    format!(
        "SELECT {} FROM local_albums a WHERE a.album_artist_id = ? AND {}{} \
         ORDER BY {} LIMIT ?",
        key_columns(&keys),
        live_album!("a"),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// A page of an artist's appearances: the artist twice, the cursor, the
/// limit.
pub(crate) fn appearance_page(after_cursor: bool) -> String {
    let keys = title_keys();
    format!(
        "SELECT {} FROM local_albums a WHERE {APPEARANCE_FILTER}{} ORDER BY {} LIMIT ?",
        key_columns(&keys),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// The album projection for `ids` albums.
pub(crate) fn album_rows(ids: ListLen) -> String {
    format!(
        "SELECT {ALBUM_COLUMNS} {ALBUM_JOINS} WHERE a.id IN ({}) GROUP BY a.id",
        marks(ids)
    )
}

/// The track list's order for one sort and direction. Within an album,
/// tracks keep disc and track order whichever way the albums run.
pub(crate) fn track_keys(sort: TrackSort, descending: bool) -> Vec<OrderKey> {
    let d = descending;
    match sort {
        TrackSort::Title => vec![("t.title_folded", d), ("t.id", d)],
        TrackSort::DateAdded => vec![("t.imported_at", d), ("t.id", d)],
        TrackSort::Artist => vec![
            ("COALESCE(t.artist_name_folded, '')", d),
            ("t.album_title_folded", d),
            ("t.local_album_id", d),
            ("t.disc_number", false),
            ("t.track_number", false),
            ("t.id", false),
        ],
        TrackSort::Album => vec![
            ("t.album_title_folded", d),
            ("t.local_album_id", d),
            ("t.disc_number", false),
            ("t.track_number", false),
            ("t.id", false),
        ],
        TrackSort::Random => vec![(shuffle_key!("t.search_key"), false), ("t.id", false)],
    }
}

/// The track joins, with the seed row for a random order.
fn track_from(sort: TrackSort) -> String {
    if sort == TrackSort::Random {
        format!("{TRACK_JOINS}{SHUFFLE_SEED}")
    } else {
        TRACK_JOINS.to_owned()
    }
}

/// The track list's text filter on the FTS index: the FTS expression, then
/// each of `short` words three times.
fn track_words(short: ListLen) -> String {
    format!(
        "t.availability = 'indexed' AND {TRACK_TEXT_MATCH}{}",
        TRACK_WORD_LIKE.repeat(short.get())
    )
}

/// A page of the track list on the substring match: the seed of a random
/// order, the filter, the cursor, the limit.
pub(crate) fn track_page_like(sort: TrackSort, descending: bool, after_cursor: bool) -> String {
    let keys = track_keys(sort, descending);
    format!(
        "SELECT {TRACK_COLUMNS}, {} {} WHERE {TRACK_LIKE}{TRACK_SCOPE}{} ORDER BY {} LIMIT ?",
        key_columns(&keys),
        track_from(sort),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// A page of the track list on the FTS index with `short` short words.
pub(crate) fn track_page_words(
    short: ListLen,
    sort: TrackSort,
    descending: bool,
    after_cursor: bool,
) -> String {
    let keys = track_keys(sort, descending);
    format!(
        "SELECT {TRACK_COLUMNS}, {} {} WHERE {}{TRACK_SCOPE}{} ORDER BY {} LIMIT ?",
        key_columns(&keys),
        track_from(sort),
        track_words(short),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// The track list's total on the FTS index with `short` short words.
pub(crate) fn track_count_words(short: ListLen) -> String {
    format!(
        "SELECT COUNT(*) FROM local_tracks t WHERE {}{TRACK_SCOPE}",
        track_words(short)
    )
}

/// The indexed tracks of `ids` albums, by album, disc and track.
pub(crate) fn album_tracks_in(ids: ListLen) -> String {
    format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
         WHERE t.local_album_id IN ({}) AND t.availability = 'indexed' \
         ORDER BY t.local_album_id, t.disc_number, t.track_number, t.id",
        marks(ids)
    )
}

/// The track projection for `ids` indexed tracks.
pub(crate) fn tracks_in(ids: ListLen) -> String {
    format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
         WHERE t.availability = 'indexed' AND t.id IN ({})",
        marks(ids)
    )
}

/// The predicate of one artist scope, with `(LED)` and `(CREDITED)` spelled
/// out.
fn artist_scope_predicate(scope: ArtistScope) -> String {
    let predicate = match scope {
        ArtistScope::All => CREDITED_PREDICATE,
        ArtistScope::AlbumArtists => LED_PREDICATE,
        ArtistScope::Contributors => "(NOT (LED) AND (CREDITED))",
        ArtistScope::WithMusic => WITH_MUSIC_PREDICATE,
    };
    predicate
        .replace("(LED)", LED_PREDICATE)
        .replace("(CREDITED)", CREDITED_PREDICATE)
}

/// True when a page of `scope` sorted by `sort` walks the kept artist
/// counts alone: the count sorts in every scope those counts list.
fn counts_walk(scope: ArtistScope, sort: ArtistSort) -> bool {
    matches!(sort, ArtistSort::AlbumCount | ArtistSort::AppearanceCount)
        && !matches!(scope, ArtistScope::WithMusic)
}

/// The artist list's order for one scope, sort and direction. The count
/// sorts break ties by name, A to Z, in both directions. Over the kept
/// counts every key runs one way, so most first reads the negated count;
/// elsewhere the counts are joined to the artist, zero with no row.
pub(crate) fn artist_keys(scope: ArtistScope, sort: ArtistSort, descending: bool) -> Vec<OrderKey> {
    let d = descending;
    let walk = counts_walk(scope, sort);
    let count = match (sort, d, walk) {
        (ArtistSort::Name, ..) => return vec![("r.folded_name", d), ("r.id", d)],
        (ArtistSort::DateAdded, ..) => return vec![("r.created_at", d), ("r.id", d)],
        (ArtistSort::AlbumCount, true, true) => "s.album_count_neg",
        (ArtistSort::AlbumCount, false, true) => "s.album_count",
        (ArtistSort::AppearanceCount, true, true) => "s.appearance_count_neg",
        (ArtistSort::AppearanceCount, false, true) => "s.appearance_count",
        (ArtistSort::AlbumCount, _, false) => "COALESCE(s.album_count, 0)",
        (ArtistSort::AppearanceCount, _, false) => "COALESCE(s.appearance_count, 0)",
    };
    if walk {
        vec![
            (count, false),
            ("s.folded_name", false),
            ("s.local_artist_id", false),
        ]
    } else {
        vec![(count, d), ("r.folded_name", false), ("r.id", false)]
    }
}

/// The scope of a walk over the kept counts, whose rows are the credited
/// artists. A bound on the walked count is written on that column, so the
/// first page seeks it. A lower bound is left out after a cursor: the
/// cursor came from a row in scope and the walk only moves up from it, so
/// the cursor alone seeks.
fn counts_scope(
    scope: ArtistScope,
    sort: ArtistSort,
    descending: bool,
    after: bool,
) -> &'static str {
    match (scope, sort, descending) {
        (ArtistScope::AlbumArtists, ArtistSort::AlbumCount, false) if after => "1",
        (ArtistScope::AlbumArtists, ArtistSort::AlbumCount, false) => "s.album_count > 0",
        (ArtistScope::AlbumArtists, ArtistSort::AlbumCount, true) => "s.album_count_neg < 0",
        (ArtistScope::Contributors, ArtistSort::AlbumCount, false) => "s.album_count <= 0",
        (ArtistScope::Contributors, ArtistSort::AlbumCount, true) if after => "1",
        (ArtistScope::Contributors, ArtistSort::AlbumCount, true) => "s.album_count_neg >= 0",
        (ArtistScope::AlbumArtists, ..) => "+s.album_count > 0",
        (ArtistScope::Contributors, ..) => "+s.album_count = 0",
        _ => "1",
    }
}

/// One page of artist keys in a scope: the name filter (bound twice), the
/// cursor, the limit.
pub(crate) fn artist_page(
    scope: ArtistScope,
    sort: ArtistSort,
    descending: bool,
    after_cursor: bool,
) -> String {
    let keys = artist_keys(scope, sort, descending);
    if counts_walk(scope, sort) {
        return format!(
            "SELECT {} FROM library_artist_counts s \
             WHERE {} AND (? IS NULL OR s.folded_name LIKE ? ESCAPE '\\'){} ORDER BY {} LIMIT ?",
            key_columns(&keys),
            counts_scope(scope, sort, descending, after_cursor),
            after(&keys, after_cursor),
            order_by(&keys)
        );
    }
    format!(
        "SELECT {} FROM local_artists r{} \
         WHERE r.retired_into_artist_id IS NULL AND ({}) \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\'){} ORDER BY {} LIMIT ?",
        key_columns(&keys),
        counts_join(sort),
        artist_scope_predicate(scope),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// One page of artist ids in a scope at an offset, for the player apps:
/// the name filter (bound twice), the limit, the offset.
pub(crate) fn artist_offset_page(scope: ArtistScope, sort: ArtistSort, descending: bool) -> String {
    format!(
        "SELECT r.id FROM local_artists r{} \
         WHERE r.retired_into_artist_id IS NULL AND ({}) \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\') ORDER BY {} LIMIT ? OFFSET ?",
        counts_join(sort),
        artist_scope_predicate(scope),
        order_by(&artist_keys(ArtistScope::WithMusic, sort, descending))
    )
}

/// The kept counts joined to the artist `r`, for a count sort that does
/// not walk them.
fn counts_join(sort: ArtistSort) -> &'static str {
    match sort {
        ArtistSort::AlbumCount | ArtistSort::AppearanceCount => {
            " LEFT JOIN library_artist_counts s ON s.local_artist_id = r.id"
        }
        ArtistSort::Name | ArtistSort::DateAdded => "",
    }
}

/// How many artists of one scope match the name filter (bound twice).
pub(crate) fn artist_scope_count(scope: ArtistScope) -> String {
    format!(
        "SELECT COUNT(*) {ARTIST_JOINS} \
         WHERE r.retired_into_artist_id IS NULL AND ({}) \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\')",
        artist_scope_predicate(scope)
    )
}

/// The artist projection for `ids` artists.
pub(crate) fn artist_rows(ids: ListLen) -> String {
    format!(
        "SELECT {ARTIST_COLUMNS} {ARTIST_JOINS} WHERE r.id IN ({})",
        marks(ids)
    )
}

/// Owned albums led per artist id, one grouped pass over `ids` leaders.
pub(crate) fn artist_album_counts(ids: ListLen) -> String {
    format!(
        "SELECT la.album_artist_id AS id, COUNT(*) AS total \
         FROM local_albums la WHERE la.album_artist_id IN ({}) AND {} GROUP BY 1",
        marks(ids),
        owned_album!("la")
    )
}

/// Indexed credited tracks, appearance albums and appearance tracks per
/// artist id, from the kept artist and album pairs of live albums. An
/// appearance is on an album led by someone else.
pub(crate) fn artist_credit_counts(ids: ListLen) -> String {
    format!(
        "SELECT p.local_artist_id AS id, SUM(p.indexed_tracks) AS tracks, \
         SUM(p.appearance) AS appearances, \
         SUM(p.appearance * p.indexed_tracks) AS appearance_tracks \
         FROM library_artist_album_credits p \
         WHERE p.local_artist_id IN ({}) AND p.live = 1 GROUP BY 1",
        marks(ids)
    )
}

/// Which of `ids` items of one kind a user favorited: the user, the kind,
/// then the ids.
pub(crate) fn favorites_in(ids: ListLen) -> String {
    format!(
        "SELECT item_id FROM library_user_favorites \
         WHERE user_id = ? AND item_kind = ? AND item_id IN ({})",
        marks(ids)
    )
}

/// Each of `ids` identifiers with the live album it names now, or NULL, in
/// [`CURRENT_ALBUM`]'s order: the live album itself, its alias, the album
/// it was retired into.
pub(crate) fn current_albums(ids: ListLen) -> String {
    let asked = vec!["(?)"; ids.get()].join(", ");
    format!(
        "WITH asked(identifier) AS (VALUES {asked}) \
         SELECT q.identifier, COALESCE( \
         (SELECT a.id FROM local_albums a WHERE a.id = q.identifier \
          AND a.retired_into_album_id IS NULL), \
         (SELECT b.id FROM local_album_aliases x \
          JOIN local_albums b ON b.id = x.local_album_id \
          WHERE x.alias = lower(q.identifier) AND b.retired_into_album_id IS NULL LIMIT 1), \
         (SELECT b.id FROM local_albums a \
          JOIN local_albums b ON b.id = a.retired_into_album_id \
          WHERE a.id = q.identifier AND b.retired_into_album_id IS NULL LIMIT 1)) \
         FROM asked q"
    )
}

/// Owned albums holding one of `ids` lowercase release groups.
pub(crate) fn owners_by_group(ids: ListLen) -> String {
    format!(
        "SELECT e.local_album_id, lower(e.release_group_mbid) \
         FROM local_album_external_identities e JOIN local_albums b ON b.id = e.local_album_id \
         WHERE lower(e.release_group_mbid) IN ({}) AND {} AND {}",
        marks(ids),
        musicbrainz!("e"),
        owned_album!("b")
    )
}

/// Owned albums identified as one of `ids` lowercase releases.
pub(crate) fn owners_by_release(ids: ListLen) -> String {
    format!(
        "SELECT e.local_album_id, lower(e.release_mbid) \
         FROM local_album_external_identities e JOIN local_albums b ON b.id = e.local_album_id \
         WHERE e.release_mbid IS NOT NULL AND lower(e.release_mbid) IN ({}) AND {} AND {}",
        marks(ids),
        musicbrainz!("e"),
        owned_album!("b")
    )
}

/// The owned artists among `ids` lowercase MBIDs.
pub(crate) fn owned_artists_in(ids: ListLen) -> String {
    format!(
        "SELECT DISTINCT lower(e.provider_artist_id) \
         FROM local_artist_external_identities e \
         JOIN local_artists a ON a.id = e.local_artist_id \
         WHERE lower(e.provider_artist_id) IN ({}) AND {} AND {}",
        marks(ids),
        musicbrainz!("e"),
        owned_artist!("a")
    )
}

/// The album MBIDs among `ids` with an open request.
pub(crate) fn requested_in(ids: ListLen) -> String {
    format!(
        "SELECT musicbrainz_id_lower FROM request_history \
         WHERE {OPEN_REQUEST} AND musicbrainz_id_lower IN ({})",
        marks(ids)
    )
}

/// The streamable-track test, with a unary plus when the page walks the
/// table or another index (insertion order, year, a play-history order):
/// driven from an availability index, which looks free because nearly
/// every track is streamable, such a page would sort the whole catalog.
fn streamable(row_check: bool) -> &'static str {
    if row_check {
        "+t.availability = 'indexed'"
    } else {
        "t.availability = 'indexed'"
    }
}

/// The player track filters over `t` (and `a` when the shape reads the
/// album), for the tracks `key` names. Binds, in order: the FTS expression and each short word three
/// times (or, past two, all of them as a JSON list), or the LIKE pattern
/// three times; the folded title, the album,
/// the album artists as a JSON list, the lowest and highest year, the
/// [`PLAYER_TRACK_SCOPE`] binds when scoped, the played ids as a JSON list.
fn player_track_filter(shape: TrackShape, key: TrackKey, row_check: bool) -> String {
    // One album's, one exact title's or some album artists' tracks are
    // reached through their key, never by a walk of an availability index,
    // which with no statistics yet looks as good as the key.
    let row_check = row_check || key != TrackKey::Open || shape.album_artists;
    let mut text = streamable(row_check).to_owned();
    match shape.text {
        TextMatch::Off => {}
        TextMatch::Like => text.push_str(
            " AND (t.title_folded LIKE ? ESCAPE '\\' OR t.artist_name_folded LIKE ? ESCAPE '\\' \
             OR t.album_title_folded LIKE ? ESCAPE '\\')",
        ),
        TextMatch::Words => {
            text.push_str(" AND ");
            text.push_str(TRACK_TEXT_MATCH);
        }
        TextMatch::OneShort | TextMatch::TwoShort => {
            text.push_str(" AND ");
            text.push_str(TRACK_TEXT_MATCH);
            text.push_str(TRACK_WORD_LIKE);
            if shape.text == TextMatch::TwoShort {
                text.push_str(TRACK_WORD_LIKE);
            }
        }
        TextMatch::ManyShort => {
            text.push_str(" AND ");
            text.push_str(TRACK_TEXT_MATCH);
            text.push_str(PLAYER_SHORT_WORDS);
        }
    }
    if key.title() {
        text.push_str(" AND t.title_folded = ?");
    }
    if key.album() {
        text.push_str(" AND t.local_album_id = ?");
    }
    if shape.album_artists {
        text.push_str(" AND a.album_artist_id IN (SELECT value FROM json_each(?))");
    }
    if shape.years {
        text.push_str(" AND t.year >= ? AND t.year <= ?");
    }
    if shape.scoped {
        text.push_str(PLAYER_TRACK_SCOPE);
    }
    text.push_str(match shape.played {
        PlayedFilter::Any => "",
        PlayedFilter::Played => " AND t.id IN (SELECT value FROM json_each(?))",
        PlayedFilter::Unplayed => " AND t.id NOT IN (SELECT value FROM json_each(?))",
    });
    text
}

/// The tracks a player page reads: with their album when the filters or
/// the order read it.
fn player_track_rows(shape: TrackShape, album_order: bool) -> &'static str {
    if shape.album_artists || album_order {
        "local_tracks t JOIN local_albums a ON a.id = t.local_album_id"
    } else {
        "local_tracks t"
    }
}

/// The ORDER BY of a player track page; ids break ties.
fn player_track_order(sequence: TrackSequence) -> String {
    match sequence {
        TrackSequence::Album => "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC, \
             t.disc_number ASC, t.track_number ASC, t.id ASC"
            .to_owned(),
        TrackSequence::Disc => "t.disc_number ASC, t.track_number ASC, t.id ASC".to_owned(),
        TrackSequence::Title => "t.title_folded ASC, t.id ASC".to_owned(),
        TrackSequence::Newest => "t.imported_at DESC, t.id ASC".to_owned(),
        TrackSequence::Random | TrackSequence::Shuffle => "t.search_key ASC".to_owned(),
        TrackSequence::Natural => "t.search_key ASC".to_owned(),
        TrackSequence::AddedUp => "t.imported_at ASC, t.id ASC".to_owned(),
        TrackSequence::AddedDown => "t.imported_at DESC, t.id DESC".to_owned(),
        TrackSequence::TitleDown => "t.title_folded DESC, t.id DESC".to_owned(),
        TrackSequence::YearUp => "t.year ASC, t.id ASC".to_owned(),
        TrackSequence::YearDown => "t.year DESC, t.id DESC".to_owned(),
    }
}

/// One page of player track ids over the whole library.
pub(crate) fn player_track_page(shape: TrackShape, sequence: TrackSequence) -> String {
    track_page(shape, TrackKey::Open, sequence)
}

/// One page of one album's player track ids.
pub(crate) fn player_album_track_page(
    shape: TrackShape,
    key: AlbumKey,
    sequence: TrackSequence,
) -> String {
    track_page(shape, key.into(), sequence)
}

/// One page of the player track ids with an exact title.
pub(crate) fn player_title_track_page(shape: TrackShape, sequence: TrackSequence) -> String {
    track_page(shape, TrackKey::Title, sequence)
}

/// One page of player track ids: the seed of a shuffle, the filters, limit,
/// offset. The ids are paged over the narrow join; the rich rows are read
/// for the page alone.
fn track_page(shape: TrackShape, key: TrackKey, sequence: TrackSequence) -> String {
    let row_check = matches!(
        sequence,
        TrackSequence::Natural | TrackSequence::YearUp | TrackSequence::YearDown
    );
    let rows = player_track_rows(shape, sequence == TrackSequence::Album);
    let filter = player_track_filter(shape, key, row_check);
    let order = match sequence {
        TrackSequence::Shuffle => PLAYER_SHUFFLE_KEY,
        TrackSequence::Random => "RANDOM()",
        TrackSequence::Natural if shape == TrackShape::OPEN && key == TrackKey::Open => {
            return PLAYER_NATURAL_PAGE.to_owned();
        }
        _ => {
            return format!(
                "SELECT t.id FROM {rows} WHERE {filter} ORDER BY {} LIMIT ? OFFSET ?",
                player_track_order(sequence)
            );
        }
    };
    // A random order sorts every track: the keys are worked out from the
    // search keys alone, which a narrow index holds, and only the page's
    // rows are read for their ids.
    format!(
        "SELECT pt.id FROM (SELECT t.search_key AS k, t.rowid AS r, {order} AS o \
         FROM {rows} WHERE {filter} ORDER BY o, k LIMIT ? OFFSET ?) p \
         CROSS JOIN local_tracks pt WHERE pt.rowid = p.r ORDER BY p.o, p.k"
    )
}

/// How many tracks pass the filters, over the whole library.
pub(crate) fn player_track_count(shape: TrackShape) -> String {
    track_count(shape, TrackKey::Open)
}

/// How many tracks of one album or one exact title pass the filters.
pub(crate) fn player_keyed_track_count(shape: TrackShape, key: KeyedBy) -> String {
    track_count(shape, key.into())
}

fn track_count(shape: TrackShape, key: TrackKey) -> String {
    format!(
        "SELECT COUNT(*) FROM {} WHERE {}",
        player_track_rows(shape, false),
        player_track_filter(shape, key, false)
    )
}

/// The player projection for `ids` streamable tracks.
pub(crate) fn player_tracks_in(ids: ListLen) -> String {
    format!(
        "SELECT {PLAYER_TRACK_COLUMNS} {PLAYER_TRACK_JOINS} \
         WHERE t.availability = 'indexed' AND t.id IN ({})",
        marks(ids)
    )
}

/// The listener's played values per track, one row each with a play count
/// above 0: `id`, `plays` and `played_at` (whole seconds). `ids` limits it
/// to some tracks (binds: the listener, the ids, the listener, the ids);
/// without, every track the listener played or marked (binds: the listener
/// twice).
///
/// A track's counted plays are the listener's plays of it later than its
/// unplayed mark, in whole seconds, or all of them when it has none. Its
/// play count is that number, or 1 when it is marked played and has none;
/// its last play is the latest counted play, else the played mark.
///
/// The plays are grouped per track from the index over listener, track and
/// time, and each track's mark is then looked up by key, so the cost is one
/// probe per played track. Only a track marked unplayed has its plays read
/// again, from the same index, for those after the mark. A track marked
/// played with no play at all comes from the marks by key.
///
/// This rule for a track also lives in the refresh of migration 0065, which
/// keeps the album values in `library_user_album_plays`. Changing it takes a
/// migration that recreates that refresh and runs its backfill again.
fn track_values(ids: Option<ListLen>) -> String {
    let (plays, marked) = match ids {
        None => (
            format!("FROM {PLAYS} WHERE h.user_id = ? AND h.local_track_id IS NOT NULL"),
            format!("FROM {MARKS} WHERE m.user_id = ?"),
        ),
        Some(ids) => {
            let ids = marks(ids);
            (
                format!("FROM {PLAYS} WHERE h.user_id = ? AND h.local_track_id IN ({ids})"),
                format!("FROM {MARKS} WHERE m.user_id = ? AND m.local_track_id IN ({ids})"),
            )
        }
    };
    let after_mark = |aggregate: &str| {
        format!(
            "(SELECT {aggregate} FROM {PLAYS} \
             WHERE h.user_id = p.user AND h.local_track_id = p.id AND {AFTER_MARK})"
        )
    };
    format!(
        "SELECT id, CASE WHEN plays > 0 THEN plays ELSE 1 END AS plays, \
         COALESCE(played_at, marked) AS played_at FROM (\
         SELECT p.id AS id, \
         CASE WHEN m.marked_unplayed_at IS NULL THEN p.plays ELSE {count} END AS plays, \
         CASE WHEN m.marked_unplayed_at IS NULL THEN p.played_at ELSE {latest} END \
         AS played_at, m.marked_played_at / 1000 AS marked \
         FROM (SELECT h.user_id AS user, h.local_track_id AS id, COUNT(*) AS plays, \
         MAX({PLAY_SECONDS}) AS played_at {plays} GROUP BY h.local_track_id) p \
         LEFT JOIN {MARKS} ON m.user_id = p.user AND m.local_track_id = p.id \
         UNION ALL \
         SELECT m.local_track_id, 0, NULL, m.marked_played_at / 1000 {marked} \
         AND m.marked_played_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM {PLAYS} \
         WHERE h.user_id = m.user_id AND h.local_track_id = m.local_track_id)) \
         WHERE plays > 0 OR marked IS NOT NULL",
        count = after_mark("COUNT(*)"),
        latest = after_mark(&format!("MAX({PLAY_SECONDS})")),
    )
}

/// The listener's played values for `ids` tracks; see [`track_values`].
pub(crate) fn player_track_values(ids: ListLen) -> String {
    track_values(Some(ids))
}

/// What a play-history order pages over: the listener's values per row as
/// `v` (`id`, `plays`, `played_at`), the ids of the rows that have any,
/// and the catalog rows the filters read, aliased.
struct History {
    values: String,
    played_ids: &'static str,
    rows: &'static str,
    alias: &'static str,
    filter: String,
}

impl History {
    /// Tracks, whose filters read `t` (and `a`). The values bind the
    /// listener twice, the played ids three times.
    fn tracks(shape: TrackShape, key: TrackKey) -> Self {
        Self {
            values: track_values(None),
            played_ids: PLAYER_PLAYED_TRACKS.text(),
            rows: player_track_rows(shape, false),
            alias: "t",
            filter: player_track_filter(shape, key, true),
        }
    }

    /// Albums, as `a`, from the album play summary (migration 0065): its
    /// rows are the albums with a play count above 0, and its order indexes
    /// let a played page walk the listener's rows in order and stop at the
    /// page. `played_at` is the stored milliseconds, which order as the
    /// seconds do. The values and the played ids bind the listener once.
    fn albums(shape: AlbumShape) -> Self {
        Self {
            values: "SELECT local_album_id AS id, plays, last_played_at AS played_at \
                     FROM library_user_album_plays WHERE user_id = ?"
                .to_owned(),
            played_ids: PLAYER_ALBUM_PLAYED_IDS.text(),
            rows: "local_albums a",
            alias: "a",
            filter: player_album_filter(shape),
        }
    }

    /// One page of the played rows, ordered by `key`; ids break ties in the
    /// same direction. The CROSS JOIN keeps the listener's values as the
    /// outer loop, so this costs by their history and never by the size of
    /// the library. Binds: the listener, the filters, limit, offset.
    fn played(&self, key: HistoryKey, descending: bool) -> String {
        let Self {
            values,
            rows,
            alias,
            filter,
            ..
        } = self;
        let column = match key {
            HistoryKey::LastPlayed => "v.played_at",
            HistoryKey::PlayCount => "v.plays",
        };
        let direction = if descending { "DESC" } else { "ASC" };
        format!(
            "SELECT v.id FROM ({values}) v CROSS JOIN {rows} WHERE {alias}.id = v.id \
             AND {filter} ORDER BY {column} {direction}, v.id {direction} LIMIT ? OFFSET ?"
        )
    }

    /// How many rows the played half holds: the played ids that pass the
    /// filters. Binds: the list of played ids, then the filters.
    fn played_count(&self) -> String {
        let Self {
            rows,
            alias,
            filter,
            ..
        } = self;
        format!(
            "SELECT COUNT(*) FROM json_each(?) p CROSS JOIN {rows} \
             WHERE {alias}.id = p.value AND {filter}"
        )
    }

    /// The rows with a play count of 0 that pass the filters: those not in
    /// the played ids, read here as one list built once, or (`listed`)
    /// handed in as a JSON list. Binds: the filters, then the listener or
    /// the list.
    fn unplayed_from(&self, listed: bool) -> String {
        let Self {
            rows,
            alias,
            played_ids,
            filter,
            ..
        } = self;
        let played = if listed {
            "SELECT value FROM json_each(?)"
        } else {
            played_ids
        };
        format!("FROM {rows} WHERE {filter} AND {alias}.id NOT IN ({played})")
    }

    /// One page of the rows with a play count of 0, in id order: a walk of
    /// an index on the id that stops at the page. Binds: the filters, the
    /// listener or the list (`listed`), limit, offset.
    fn unplayed(&self, descending: bool, listed: bool) -> String {
        let alias = self.alias;
        let direction = if descending { "DESC" } else { "ASC" };
        format!(
            "SELECT {alias}.id {} ORDER BY {alias}.id {direction} LIMIT ? OFFSET ?",
            self.unplayed_from(listed)
        )
    }

    /// How many rows the unplayed half holds. Binds: the filters, then the
    /// list of played ids.
    fn unplayed_count(&self) -> String {
        format!("SELECT COUNT(*) {}", self.unplayed_from(true))
    }
}

/// A page of the played tracks in a play-history order.
pub(crate) fn player_tracks_played(
    shape: TrackShape,
    key: TrackKey,
    order: HistoryKey,
    descending: bool,
) -> String {
    History::tracks(shape, key).played(order, descending)
}

/// A page of the unplayed tracks in id order, over the whole library.
pub(crate) fn player_tracks_unplayed(shape: TrackShape, descending: bool, listed: bool) -> String {
    History::tracks(shape, TrackKey::Open).unplayed(descending, listed)
}

/// A page of the unplayed tracks of one album or one exact title.
pub(crate) fn player_keyed_tracks_unplayed(
    shape: TrackShape,
    key: KeyedBy,
    descending: bool,
    listed: bool,
) -> String {
    History::tracks(shape, key.into()).unplayed(descending, listed)
}

/// How many played tracks pass the filters.
pub(crate) fn player_tracks_played_count(shape: TrackShape, key: TrackKey) -> String {
    History::tracks(shape, key).played_count()
}

/// How many unplayed tracks pass the filters, over the whole library.
pub(crate) fn player_tracks_unplayed_count(shape: TrackShape) -> String {
    History::tracks(shape, TrackKey::Open).unplayed_count()
}

/// How many unplayed tracks of one album or one exact title pass the
/// filters.
pub(crate) fn player_keyed_tracks_unplayed_count(shape: TrackShape, key: KeyedBy) -> String {
    History::tracks(shape, key.into()).unplayed_count()
}

/// The player album filters over `a`. Player apps see owned albums only;
/// the definition's unary plus keeps the planner off the retired index, so
/// the ordered pages walk the title and created-at indexes and stop early,
/// and on `has_indexed_track` it keeps the planner from guessing the column
/// leaves few albums, which with statistics turns the id walk of an
/// unplayed page into a sort. Binds, in order: the album artist, the album
/// artists as a JSON list, the [`PLAYER_ALBUM_SCOPE`] binds when scoped,
/// the appears-on artists twice, the genre twice, the played ids as a JSON
/// list.
fn player_album_filter(shape: AlbumShape) -> String {
    let mut text = owned_album!("a").to_owned();
    if shape.artist {
        text.push_str(" AND a.album_artist_id = ?");
    }
    if shape.album_artists {
        text.push_str(" AND a.album_artist_id IN (SELECT value FROM json_each(?))");
    }
    if shape.scoped {
        text.push_str(PLAYER_ALBUM_SCOPE);
    }
    if shape.appears {
        text.push_str(PLAYER_APPEARS_ON);
    }
    if shape.genre {
        text.push_str(PLAYER_ALBUM_GENRE);
    }
    text.push_str(match shape.played {
        PlayedFilter::Any => "",
        PlayedFilter::Played => " AND a.id IN (SELECT value FROM json_each(?))",
        PlayedFilter::Unplayed => " AND a.id NOT IN (SELECT value FROM json_each(?))",
    });
    text
}

/// The ORDER BY of a player album page; ids break ties.
fn player_album_order(sequence: AlbumSequence) -> String {
    match sequence {
        AlbumSequence::Newest => "a.created_at DESC, a.id ASC".to_owned(),
        AlbumSequence::Title => "a.title_folded ASC, a.id ASC".to_owned(),
        AlbumSequence::Artist => {
            "a.album_artist_name_folded ASC, a.title_folded ASC, a.id ASC".to_owned()
        }
        AlbumSequence::Year => {
            "(a.year IS NULL) ASC, a.year ASC, a.title_folded ASC, a.id ASC".to_owned()
        }
        AlbumSequence::YearDescending => {
            "(a.year IS NULL) ASC, a.year DESC, a.title_folded ASC, a.id ASC".to_owned()
        }
        AlbumSequence::Shuffle => format!("{}, a.rowid", shuffle_key!("a.rowid")),
        AlbumSequence::Natural => "a.rowid ASC".to_owned(),
        AlbumSequence::AddedUp => "a.created_at ASC, a.id ASC".to_owned(),
        AlbumSequence::AddedDown => "a.created_at DESC, a.id DESC".to_owned(),
        AlbumSequence::TitleDown => "a.title_folded DESC, a.id DESC".to_owned(),
        AlbumSequence::YearUp => "a.year ASC, a.id ASC".to_owned(),
        AlbumSequence::YearDown => "a.year DESC, a.id DESC".to_owned(),
    }
}

/// One page of player album ids: the seed of a shuffle, the filters, limit,
/// offset.
pub(crate) fn player_album_page(shape: AlbumShape, sequence: AlbumSequence) -> String {
    let seed = if sequence == AlbumSequence::Shuffle {
        SHUFFLE_SEED
    } else {
        ""
    };
    format!(
        "SELECT a.id FROM local_albums a{seed} WHERE {} ORDER BY {} LIMIT ? OFFSET ?",
        player_album_filter(shape),
        player_album_order(sequence)
    )
}

/// How many albums pass the filters.
pub(crate) fn player_album_count(shape: AlbumShape) -> String {
    format!(
        "SELECT COUNT(*) FROM local_albums a WHERE {}",
        player_album_filter(shape)
    )
}

/// The player projection for `ids` albums. An album that is merged away or
/// has no files answers as an unknown id.
pub(crate) fn player_albums_in(ids: ListLen) -> String {
    format!(
        "SELECT {ALBUM_COLUMNS}, {ALBUM_EXTRAS} {ALBUM_JOINS} {ALBUM_EXTRA_JOINS} \
         WHERE a.id IN ({}) AND {} GROUP BY a.id",
        marks(ids),
        owned_album!("a")
    )
}

/// The listener's values for `ids` albums, from the album play summary:
/// `id`, `plays`, `played_at` (whole seconds) and `played`. Binds: the
/// listener, then the ids.
pub(crate) fn player_album_values(ids: ListLen) -> String {
    format!(
        "SELECT local_album_id AS id, plays, last_played_at / 1000 AS played_at, played \
         FROM library_user_album_plays WHERE user_id = ? AND local_album_id IN ({})",
        marks(ids)
    )
}

/// A page of the played albums in a play-history order.
pub(crate) fn player_albums_played(shape: AlbumShape, key: HistoryKey, descending: bool) -> String {
    History::albums(shape).played(key, descending)
}

/// A page of the unplayed albums in id order.
pub(crate) fn player_albums_unplayed(shape: AlbumShape, descending: bool, listed: bool) -> String {
    History::albums(shape).unplayed(descending, listed)
}

/// How many played albums pass the filters.
pub(crate) fn player_albums_played_count(shape: AlbumShape) -> String {
    History::albums(shape).played_count()
}

/// How many unplayed albums pass the filters.
pub(crate) fn player_albums_unplayed_count(shape: AlbumShape) -> String {
    History::albums(shape).unplayed_count()
}

/// The albums with a streamable track the listener played most often
/// (`frequent`; latest play breaks ties) or most recently, then by id.
/// Binds: the listener twice, limit, offset.
pub(crate) fn player_history_albums(frequent: bool) -> String {
    let order = if frequent {
        "h.plays DESC, h.played_at DESC"
    } else {
        "h.played_at DESC"
    };
    format!(
        "SELECT a.id FROM ({ALBUM_PLAYS}) h CROSS JOIN local_albums a WHERE a.id = h.id \
         AND {} ORDER BY {order}, a.id LIMIT ? OFFSET ?",
        owned_album!("a")
    )
}

/// One listener's play count and last play for `ids` items of `kind`.
/// Binds: the listener, then the ids; for albums, both a second time.
///
/// An album's plays are the plays of the tracks on it now, found through
/// the index over listener and track, and the plays filed under the album
/// with no track. Those have a small index of their own (listener, album,
/// time, only rows with an album and no track), and the statement names
/// it: with statistics the planner otherwise walks every play of the
/// listener that names no track, and an upgraded history can hold very
/// many.
pub(crate) fn player_play_stats(kind: ItemKind, ids: ListLen) -> String {
    let ids = marks(ids);
    let plays = match kind {
        ItemKind::Artist => format!(
            "SELECT local_artist_id AS id, played_at FROM library_play_history \
             WHERE user_id = ? AND local_artist_id IN ({ids})"
        ),
        ItemKind::Track => format!(
            "SELECT local_track_id AS id, played_at FROM library_play_history \
             WHERE user_id = ? AND local_track_id IN ({ids})"
        ),
        ItemKind::Album => format!(
            "SELECT pt.local_album_id AS id, h.played_at AS played_at \
             FROM local_tracks pt CROSS JOIN library_play_history h \
             WHERE h.user_id = ? AND h.local_track_id = pt.id AND pt.local_album_id IN ({ids}) \
             UNION ALL \
             SELECT h.local_album_id, h.played_at \
             FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track \
             WHERE h.user_id = ? AND h.local_track_id IS NULL AND h.local_album_id IN ({ids})"
        ),
    };
    format!(
        "SELECT id, COUNT(*) AS plays, \
         CAST(strftime('%s', MAX(played_at)) AS REAL) AS last_played FROM ({plays}) GROUP BY id"
    )
}

/// Which of `ids` exist for a player: live artists, owned albums,
/// streamable tracks.
pub(crate) fn player_existing(kind: ItemKind, ids: ListLen) -> String {
    let (table, extra) = match kind {
        ItemKind::Artist => ("local_artists", "retired_into_artist_id IS NULL"),
        ItemKind::Album => ("local_albums", owned_album!("local_albums")),
        ItemKind::Track => ("local_tracks", "availability = 'indexed'"),
    };
    format!(
        "SELECT id FROM {table} WHERE {extra} AND id IN ({})",
        marks(ids)
    )
}

/// For `ids` albums, the accepted match's release and release group, or,
/// for an album with none, each indexed track's tag ids: an accepted album
/// is one row with no tracks joined.
pub(crate) fn player_album_ids(ids: ListLen) -> String {
    format!(
        "SELECT a.id, ae.release_mbid, ae.release_group_mbid, \
         t.embedded_release_mbid, t.embedded_release_group_mbid \
         FROM local_albums a \
         LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id AND {} \
         LEFT JOIN local_tracks t ON ae.local_album_id IS NULL \
         AND t.local_album_id = a.id AND t.availability = 'indexed' \
         WHERE a.id IN ({})",
        musicbrainz!("ae"),
        marks(ids)
    )
}

/// The accepted MusicBrainz ids of `ids` artists.
pub(crate) fn player_artist_ids(ids: ListLen) -> String {
    format!(
        "SELECT e.local_artist_id, e.provider_artist_id \
         FROM local_artist_external_identities e \
         WHERE e.local_artist_id IN ({}) AND {}",
        marks(ids),
        musicbrainz!("e")
    )
}

/// Tag ids of `ids` artists (the ids bound twice): the id each credit of
/// an indexed track or a live album took from its file, set by the scan
/// only where the file pairs its artist names and ids one for one. The
/// credits lead, so a chunk never walks the tracks.
pub(crate) fn artist_tag_ids(ids: ListLen) -> String {
    let ids = marks(ids);
    format!(
        "SELECT ta.local_artist_id, ta.artist_mbid \
         FROM local_track_artists ta CROSS JOIN local_tracks t \
         WHERE ta.local_artist_id IN ({ids}) AND ta.artist_mbid IS NOT NULL \
         AND t.id = ta.local_track_id AND t.availability = 'indexed' \
         UNION ALL \
         SELECT aa.local_artist_id, aa.artist_mbid \
         FROM local_album_artists aa CROSS JOIN local_albums a \
         WHERE aa.local_artist_id IN ({ids}) AND aa.artist_mbid IS NOT NULL \
         AND a.id = aa.local_album_id AND a.retired_into_album_id IS NULL"
    )
}

/// One page of album artist ids: name order, or insertion order when
/// `natural`. Binds: the folded name pattern twice, limit, offset.
pub(crate) fn player_album_artist_page(natural: bool) -> String {
    let order = if natural {
        "r.rowid ASC"
    } else {
        "r.folded_name ASC, r.id ASC"
    };
    format!(
        "SELECT r.id FROM local_artists r \
         WHERE r.retired_into_artist_id IS NULL AND {LED_PREDICATE} \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\') \
         ORDER BY {order} LIMIT ? OFFSET ?"
    )
}

/// For `ids` artists (bound twice), the summed length and the number of
/// the streamable tracks crediting each as track artist or album artist.
/// UNION keeps one row per (artist, track), so a track that credits its
/// album artist is counted once; the credits lead, so each track is
/// reached by key.
pub(crate) fn player_artist_durations(ids: ListLen) -> String {
    let ids = marks(ids);
    format!(
        "SELECT c.artist_id, COALESCE(SUM(t.duration_seconds), 0.0), COUNT(*) \
         FROM (SELECT ta.local_artist_id AS artist_id, ta.local_track_id AS track_id \
         FROM local_track_artists ta WHERE ta.local_artist_id IN ({ids}) \
         UNION \
         SELECT a.album_artist_id, lt.id FROM local_albums a \
         JOIN local_tracks lt ON lt.local_album_id = a.id \
         WHERE a.album_artist_id IN ({ids})) c \
         CROSS JOIN local_tracks t CROSS JOIN local_albums a \
         WHERE t.id = c.track_id AND t.availability = 'indexed' \
         AND a.id = t.local_album_id AND {} \
         GROUP BY c.artist_id",
        live_album!("a")
    )
}
