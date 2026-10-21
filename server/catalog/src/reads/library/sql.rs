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
    /// Track rowids whose text matches an FTS expression. The FTS rowid
    /// mirrors the track rowid.
    pub(crate) fragment TRACK_TEXT_MATCH =
        "t.rowid IN (SELECT rowid FROM local_tracks_fts WHERE local_tracks_fts MATCH ?)";
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
    /// How many owned albums the artist `r` leads, for the album-count
    /// sort: a probe of the artist's albums.
    pub(crate) fragment LED_ALBUM_TOTAL = (concat!(
        "(SELECT COUNT(*) FROM local_albums lo WHERE lo.album_artist_id = r.id AND ",
        owned_album!("lo"),
        ")"
    ));
    /// How many albums the artist `r` appears on, for the appearance-count
    /// sort: live albums led by someone else where the artist is credited
    /// on an indexed track, reached from the artist's own credits.
    pub(crate) fragment APPEARANCE_ALBUM_TOTAL = (concat!(
        "(SELECT COUNT(DISTINCT at.local_album_id) FROM local_track_artists ata \
         JOIN local_tracks at ON at.id = ata.local_track_id \
         JOIN local_albums aa ON aa.id = at.local_album_id \
         WHERE ata.local_artist_id = r.id AND +at.availability = 'indexed' \
         AND aa.album_artist_id != r.id AND ",
        live_album!("aa"),
        ")"
    ));
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
    /// tracks, with no album credit of their own on that album.
    pub(crate) ARTIST_APPEARS = "SELECT 1 FROM local_artist_external_identities e \
        JOIN local_artists a ON a.id = e.local_artist_id \
        JOIN local_track_artists credit ON credit.local_artist_id = e.local_artist_id \
        JOIN local_tracks t ON t.id = credit.local_track_id \
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
}

/// `?, ?, ...` for a list of `len` values.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// One column of a list's order: its expression and whether it runs
/// descending. Every order ends in the row's id, so it is total and a
/// cursor names one place in it.
pub(crate) type OrderKey = (&'static str, bool);

/// A seeded shuffle key over an alias's rowid: `h = (rowid * a + b) mod p`,
/// then `(k * h^2 + h) mod p`, with `p` the prime 2^31 - 1, above any
/// realistic rowid so every product stays inside 64 bits. The square makes
/// it non-linear, so neighbouring rows land far apart. One seed is one
/// fixed order.
macro_rules! shuffle_key {
    ($alias:literal) => {
        concat!(
            "((((",
            $alias,
            ".rowid * s.sa + s.sb) % 2147483647) * ((",
            $alias,
            ".rowid * s.sa + s.sb) % 2147483647)) % 2147483647 * s.sk + ((",
            $alias,
            ".rowid * s.sa + s.sb) % 2147483647)) % 2147483647"
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
        AlbumSort::Random => vec![(shuffle_key!("a"), false), ("a.id", false)],
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
        TrackSort::Random => vec![(shuffle_key!("t"), false), ("t.id", false)],
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

/// The artist list's order for one sort and direction. The count sorts
/// count per artist in the page's scope and break ties by name, A to Z.
pub(crate) fn artist_keys(sort: ArtistSort, descending: bool) -> Vec<OrderKey> {
    let d = descending;
    match sort {
        ArtistSort::Name => vec![("r.folded_name", d), ("r.id", d)],
        ArtistSort::DateAdded => vec![("r.created_at", d), ("r.id", d)],
        ArtistSort::AlbumCount => vec![
            (LED_ALBUM_TOTAL, d),
            ("r.folded_name", false),
            ("r.id", false),
        ],
        ArtistSort::AppearanceCount => vec![
            (APPEARANCE_ALBUM_TOTAL, d),
            ("r.folded_name", false),
            ("r.id", false),
        ],
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
    let keys = artist_keys(sort, descending);
    format!(
        "SELECT {} FROM local_artists r \
         WHERE r.retired_into_artist_id IS NULL AND ({}) \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\'){} ORDER BY {} LIMIT ?",
        key_columns(&keys),
        artist_scope_predicate(scope),
        after(&keys, after_cursor),
        order_by(&keys)
    )
}

/// One page of artist ids in a scope at an offset, for the player apps:
/// the name filter (bound twice), the limit, the offset.
pub(crate) fn artist_offset_page(scope: ArtistScope, sort: ArtistSort, descending: bool) -> String {
    format!(
        "SELECT r.id FROM local_artists r \
         WHERE r.retired_into_artist_id IS NULL AND ({}) \
         AND (? IS NULL OR r.folded_name LIKE ? ESCAPE '\\') ORDER BY {} LIMIT ? OFFSET ?",
        artist_scope_predicate(scope),
        order_by(&artist_keys(sort, descending))
    )
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
/// artist id, one grouped pass over `ids` artists' credits. An appearance
/// is on an album led by someone else.
pub(crate) fn artist_credit_counts(ids: ListLen) -> String {
    format!(
        "SELECT ata.local_artist_id AS id, \
         COUNT(DISTINCT at.id) AS tracks, \
         COUNT(DISTINCT CASE WHEN aa.album_artist_id != ata.local_artist_id \
         THEN at.local_album_id END) AS appearances, \
         COUNT(DISTINCT CASE WHEN aa.album_artist_id != ata.local_artist_id \
         THEN at.id END) AS appearance_tracks \
         FROM local_track_artists ata \
         JOIN local_tracks at ON at.id = ata.local_track_id \
         JOIN local_albums aa ON aa.id = at.local_album_id \
         WHERE ata.local_artist_id IN ({}) AND at.availability = 'indexed' AND {} \
         GROUP BY 1",
        marks(ids),
        live_album!("aa")
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
