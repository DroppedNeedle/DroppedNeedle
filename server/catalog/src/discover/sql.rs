//! Discover's statements: what its pages read from the library, and its own
//! tables (the page cache, activity, the ignore ledger, the release-group
//! map and the genres builds learned).

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;

use crate::artwork::local::art_tracks_column;
use crate::local::sql::{live_album, musicbrainz, owned_album, owned_artist, track_codec};
use droppedneedle_library::identity::asked_groups;
use droppedneedle_library::{named_group_guess, not_yet_identified, owns_group, recording_mbid};

droppedneedle_kernel::sql! {
    // -- ownership -----------------------------------------------------------

    /// Each asked release group's oldest owned album.
    fragment OLDEST_OWNERS = (concat!(
        "SELECT mbid, local_id FROM (SELECT o.group_mbid AS mbid, o.album_id AS local_id, \
         ROW_NUMBER() OVER (PARTITION BY o.group_mbid ORDER BY a.created_at, a.id) AS pick \
         FROM (",
        owns_group!("IN (SELECT g FROM asked)"),
        ") o JOIN local_albums a ON a.id = o.album_id) WHERE pick = 1"
    ));
    /// The owned local artist carrying an MBID, up to the id list.
    fragment OWNED_ARTISTS_START = "SELECT lower(ai.provider_artist_id) AS mbid, \
         MIN(ai.local_artist_id) AS local_id \
         FROM local_artist_external_identities ai \
         JOIN local_artists r ON r.id = ai.local_artist_id \
         WHERE lower(ai.provider_artist_id) IN (";
    /// What follows the id list.
    fragment OWNED_ARTISTS_END = (concat!(
        ") AND ",
        musicbrainz!("ai"),
        " AND ",
        owned_artist!("r"),
        " GROUP BY lower(ai.provider_artist_id)"
    ));
    /// Owned albums by release group.
    family OWNED_ALBUMS = owned_albums;
    /// Owned artists by MBID.
    family OWNED_ARTISTS = owned_artists;
    pub(super) row OwnedRow for family [owned_albums, owned_artists] {
        mbid: String,
        local_id: String,
    }

    // -- the discover page's library reads ------------------------------------

    /// Identified owned albums, newest changes first. A guess the files'
    /// tags contradict is not one; neither is an album owned through its
    /// tags alone, which has no match to list.
    pub(super) IDENTIFIED_ALBUMS = (concat!(
        "SELECT lower(ae.release_group_mbid) AS release_group_mbid, \
         lower(aie.provider_artist_id) AS artist_mbid \
         FROM local_albums a \
         JOIN local_album_external_identities ae ON ae.local_album_id = a.id AND ",
        musicbrainz!("ae"),
        " LEFT JOIN local_artist_external_identities aie \
           ON aie.local_artist_id = a.album_artist_id AND ",
        musicbrainz!("aie"),
        " WHERE ",
        owned_album!("a"),
        " AND NOT ",
        named_group_guess!("a.id"),
        " GROUP BY a.id ORDER BY a.updated_at DESC, a.id LIMIT ?1"
    ));
    pub(super) row IdentifiedAlbum for [IDENTIFIED_ALBUMS] {
        release_group_mbid: String,
        artist_mbid: Option<String>,
    }
    /// Indexed tracks per genre, up to the place a filter on the genre goes.
    fragment GENRE_CREDITS_START = "WITH genre_tracks AS (SELECT track.id, track.local_album_id, \
         genre.folded_name AS genre_folded FROM local_tracks track \
         JOIN local_track_genres genre ON genre.local_track_id = track.id \
         WHERE +track.availability = 'indexed' ";
    /// Credits those tracks to every artist that appears on them.
    fragment GENRE_CREDITS_END = "), \
         genre_credits(genre_folded, local_artist_id) AS (\
         SELECT gt.genre_folded, lta.local_artist_id FROM genre_tracks gt \
         JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION \
         SELECT gt.genre_folded, laa.local_artist_id FROM genre_tracks gt \
         JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION \
         SELECT gt.genre_folded, a.album_artist_id FROM genre_tracks gt \
         JOIN local_albums a ON a.id = gt.local_album_id) ";
    /// Credited artists per genre.
    fragment GENRE_COUNTS = "SELECT genre_folded, COUNT(DISTINCT local_artist_id) AS cnt \
         FROM genre_credits GROUP BY genre_folded";
    /// The genres with the most credited artists.
    pub(super) TOP_GENRES = GENRE_CREDITS_START + GENRE_CREDITS_END + GENRE_COUNTS
        + " ORDER BY cnt DESC, genre_folded LIMIT ?";
    pub(super) row GenreCount for [TOP_GENRES] {
        genre_folded: String,
        cnt: i64,
    }
    /// Credited artists of some genres, in `GenreCount`'s columns.
    family GENRE_ARTIST_COUNTS = genre_artist_counts;
    /// Lowercased artist MBIDs of some genres.
    family ARTISTS_FOR_GENRES = artists_for_genres;
    pub(super) row GenreArtist for family [artists_for_genres] {
        genre_folded: String,
        artist_mbid: String,
    }
    /// Library artists' names by MBID, up to the id list.
    fragment LIBRARY_ARTIST_NAMES_START = "SELECT lower(aie.provider_artist_id) AS mbid, \
         a.display_name AS name FROM local_artist_external_identities aie \
         JOIN local_artists a ON a.id = aie.local_artist_id \
         WHERE aie.provider = 'musicbrainz' AND lower(aie.provider_artist_id) IN (";
    /// Library artists' names by MBID.
    family LIBRARY_ARTIST_NAMES = library_artist_names;
    pub(super) row LibraryArtistName for family [library_artist_names] {
        mbid: String,
        name: String,
    }
    /// Artists by MBID and the tracks credited to them, up to the id list.
    fragment LIBRARY_ARTIST_GENRES_START = "WITH artists AS (SELECT local_artist_id, \
         lower(provider_artist_id) AS mbid \
         FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND lower(provider_artist_id) IN (";
    /// The tags on those tracks, most used first.
    fragment LIBRARY_ARTIST_GENRES_END = ")), \
         credited(mbid, track_id) AS (\
         SELECT a.mbid, lta.local_track_id FROM artists a \
         JOIN local_track_artists lta ON lta.local_artist_id = a.local_artist_id UNION \
         SELECT a.mbid, t.id FROM artists a \
         JOIN local_album_artists laa ON laa.local_artist_id = a.local_artist_id \
         JOIN local_tracks t ON t.local_album_id = laa.local_album_id UNION \
         SELECT a.mbid, t.id FROM artists a \
         JOIN local_albums al ON al.album_artist_id = a.local_artist_id \
         JOIN local_tracks t ON t.local_album_id = al.id) \
         SELECT c.mbid AS mbid, g.folded_name AS folded, min(g.name) AS name FROM credited c \
         JOIN local_tracks t ON t.id = c.track_id AND +t.availability = 'indexed' \
         JOIN local_track_genres g ON g.local_track_id = c.track_id \
         GROUP BY c.mbid, g.folded_name \
         ORDER BY c.mbid, count(*) DESC, g.folded_name";
    /// The tags on the library's tracks credited to some artists.
    family LIBRARY_ARTIST_GENRES = library_artist_genres;
    /// The genres builds learned for some artists, newest first.
    family LEARNED_ARTIST_GENRES = learned_artist_genres;
    pub(super) row ArtistGenre for family [library_artist_genres, learned_artist_genres] {
        mbid: String,
        folded: String,
        name: String,
    }
    /// A library album on a shelf.
    fragment SHELF_SELECT = (concat!(
        "SELECT a.id AS id, MIN(ae.release_group_mbid) AS release_group_mbid, a.title AS title, \
         a.album_artist_name AS album_artist_name, MIN(aie.provider_artist_id) AS artist_mbid, \
         a.year AS year FROM local_albums a \
         LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id AND ",
        musicbrainz!("ae"),
        " LEFT JOIN local_artist_external_identities aie \
           ON aie.local_artist_id = a.album_artist_id AND ",
        musicbrainz!("aie")
    ));
    /// Live albums tagged with one genre, recently changed first.
    pub(super) ALBUMS_BY_GENRE = SHELF_SELECT + " WHERE " + (live_album!("a"))
        + " AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
           AND +t.availability = 'indexed' AND EXISTS (SELECT 1 FROM local_track_genres g \
           WHERE g.local_track_id = t.id AND g.folded_name = ?1)) \
         GROUP BY a.id ORDER BY a.updated_at DESC, a.title COLLATE NOCASE, a.id LIMIT ?2";
    pub(super) row ShelfAlbumColumns for [ALBUMS_BY_GENRE] {
        id: String,
        release_group_mbid: Option<String>,
        title: String,
        album_artist_name: Option<String>,
        artist_mbid: Option<String>,
        year: Option<i64>,
    }
    /// Owned albums of some years, up to the list of years.
    fragment ANNIVERSARY_ALBUMS_START = SHELF_SELECT + " WHERE " + (owned_album!("a"))
        + " AND a.year IN (";
    /// Owned albums of some years, oldest first, in `ShelfAlbumColumns`'
    /// columns.
    family ANNIVERSARY_ALBUMS = anniversary_albums;
    /// The newest releases from a user's follows that the library lacks.
    pub(super) FOLLOWED_RELEASES = (concat!(
        "SELECT n.release_group_mbid AS release_group_mbid, n.title AS title, \
         n.artist_name AS artist_name, f.artist_mbid AS artist_mbid, \
         n.first_release_date AS first_release_date \
         FROM acquire_new_release_feed n JOIN acquire_followed_artists f \
         ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ?1 \
         WHERE NOT EXISTS (",
        owns_group!("= +n.release_group_mbid_lower"),
        ") ORDER BY n.first_release_date DESC, n.discovered_at DESC LIMIT ?2"
    ));
    pub(super) row FollowedReleaseColumns for [FOLLOWED_RELEASES] {
        release_group_mbid: String,
        title: String,
        artist_name: String,
        artist_mbid: String,
        first_release_date: Option<String>,
    }

    // -- home and genre pages -------------------------------------------------

    /// A live album with its identities and own art.
    fragment HOME_ALBUM_SELECT = "SELECT a.id AS id, a.title AS title, \
         COALESCE(an.display_name, a.album_artist_name) AS artist_name, \
         (SELECT ae.release_group_mbid FROM local_album_external_identities ae \
          WHERE ae.local_album_id = a.id AND ae.provider = 'musicbrainz' \
          AND ae.release_group_mbid IS NOT NULL ORDER BY ae.release_group_mbid LIMIT 1) AS rg, \
         (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
          WHERE aie.local_artist_id = a.album_artist_id AND aie.provider = 'musicbrainz' \
          ORDER BY aie.provider_artist_id LIMIT 1) AS artist_mbid, \
         a.year AS year, \
         (SELECT w.version FROM local_album_artwork w WHERE w.local_album_id = a.id \
          AND w.source IN ('folder','embedded')) AS art_version \
         FROM local_albums a LEFT JOIN local_artists an ON an.id = a.album_artist_id \
         WHERE a.retired_into_album_id IS NULL";
    /// The newest albums with an indexed track.
    pub(super) RECENT_ALBUMS = HOME_ALBUM_SELECT
        + " AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         ORDER BY a.created_at DESC, a.id LIMIT ?1";
    /// Albums with tracks tagged with one genre, recently changed first.
    pub(super) GENRE_ALBUMS = HOME_ALBUM_SELECT
        + " AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = a.id AND +t.availability = 'indexed' \
         AND EXISTS (SELECT 1 FROM local_track_genres g \
         WHERE g.local_track_id = t.id AND g.folded_name = ?1)) \
         ORDER BY a.updated_at DESC, a.title COLLATE NOCASE, a.id LIMIT ?2";
    pub(super) row HomeAlbumColumns for [RECENT_ALBUMS, GENRE_ALBUMS] {
        id: String,
        title: String,
        artist_name: Option<String>,
        rg: Option<String>,
        artist_mbid: Option<String>,
        year: Option<i64>,
        art_version: Option<i64>,
    }
    /// The artists with the most albums.
    pub(super) TOP_ARTISTS = "SELECT ar.id AS id, ar.display_name AS display_name, \
         (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
          WHERE aie.local_artist_id = ar.id AND aie.provider = 'musicbrainz' \
          ORDER BY aie.provider_artist_id LIMIT 1) AS mbid, \
         COUNT(DISTINCT credit.local_album_id) AS album_count \
         FROM local_album_artists credit \
         JOIN local_albums album ON album.id = credit.local_album_id \
           AND album.retired_into_album_id IS NULL \
         JOIN local_artists ar ON ar.id = credit.local_artist_id \
           AND ar.retired_into_artist_id IS NULL \
         WHERE EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = album.id \
           AND t.availability = 'indexed') \
         GROUP BY ar.id \
         ORDER BY album_count DESC, ar.display_name COLLATE NOCASE, ar.id LIMIT ?1";
    /// Artists credited (track, album or album artist) on the albums of
    /// indexed tracks tagged with one genre, by name.
    pub(super) GENRE_ARTISTS = "WITH genre_tracks AS (SELECT track.id, track.local_album_id \
         FROM local_tracks track JOIN local_track_genres genre ON genre.local_track_id = track.id \
         WHERE +track.availability = 'indexed' AND genre.folded_name = ?1), \
         genre_credits(local_artist_id, local_album_id) AS (\
         SELECT lta.local_artist_id, gt.local_album_id FROM genre_tracks gt \
         JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION \
         SELECT laa.local_artist_id, gt.local_album_id FROM genre_tracks gt \
         JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION \
         SELECT a.album_artist_id, gt.local_album_id FROM genre_tracks gt \
         JOIN local_albums a ON a.id = gt.local_album_id) \
         SELECT ar.id AS id, ar.display_name AS display_name, \
         (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
          WHERE aie.local_artist_id = ar.id AND aie.provider = 'musicbrainz' \
          ORDER BY aie.provider_artist_id LIMIT 1) AS mbid, \
         COUNT(DISTINCT gc.local_album_id) AS album_count \
         FROM genre_credits gc JOIN local_artists ar ON ar.id = gc.local_artist_id \
         WHERE ar.retired_into_artist_id IS NULL \
         GROUP BY ar.id ORDER BY ar.display_name COLLATE NOCASE, ar.id LIMIT ?2";
    pub(super) row HomeArtistColumns for [TOP_ARTISTS, GENRE_ARTISTS] {
        id: String,
        display_name: String,
        mbid: Option<String>,
        album_count: i64,
    }
    /// A user's latest plays.
    pub(super) RECENT_PLAYS = "SELECT track_name, artist_name, album_name, recording_mbid, \
         release_group_mbid, played_at FROM library_play_history WHERE user_id = ?1 \
         ORDER BY played_at DESC, id LIMIT ?2";
    pub(super) row PlayColumns for [RECENT_PLAYS] {
        track_name: String,
        artist_name: String,
        album_name: Option<String>,
        recording_mbid: Option<String>,
        release_group_mbid: Option<String>,
        played_at: Millis,
    }
    /// The artwork revision of some folded genres.
    family GENRE_REVISIONS = genre_revisions;
    pub(super) row GenreRevision for family [genre_revisions] {
        genre_folded: String,
        value: i64,
    }
    /// Collage candidates for some genres; a genre with none still answers
    /// one row, for its revision.
    family ART_CANDIDATES = art_candidates;
    pub(super) row ArtCandidateColumns for family [art_candidates] {
        genre_folded: String,
        revision: i64,
        album_id: Option<String>,
        album_title: Option<String>,
        album_artist_name: Option<String>,
        album_artist_id: Option<String>,
        match_count: Option<i64>,
        cover_version: Option<i64>,
        content_hash: Option<String>,
        source: Option<String>,
        path: Option<String>,
        tracks: Option<String>,
    }

    // -- radio ------------------------------------------------------------------

    /// The columns radio reads from [`RADIO_TRACK_SELECT`].
    fragment RADIO_TRACK_COLUMNS = "SELECT id, title, artist_name, album_title, file_format, \
         duration_seconds, recording_mbid, artist_mbid, album_artist_mbid, release_group_mbid ";
    /// Owned, indexed tracks with their MusicBrainz ids. The artist and
    /// album columns prefer the identified ids over the embedded tags.
    fragment RADIO_TRACK_SELECT = "SELECT t.id AS id, t.title AS title, \
         COALESCE(NULLIF(t.artist_name, ''), t.album_artist_name, '') AS artist_name, \
         t.album_title AS album_title, "
         + (track_codec!("t"))
         + " AS file_format, \
         t.duration_seconds AS duration_seconds, t.disc_number AS disc_number, \
         t.track_number AS track_number, "
         + (recording_mbid!("te", "t"))
         + " AS recording_mbid, \
         lower(COALESCE(tae.provider_artist_id, t.embedded_artist_mbid, '')) AS artist_mbid, \
         lower(COALESCE(aae.provider_artist_id, t.embedded_album_artist_mbid, '')) \
             AS album_artist_mbid, \
         lower(COALESCE(ae.release_group_mbid, CASE WHEN "
         + (not_yet_identified!("a"))
         + " THEN t.embedded_release_group_mbid END)) AS release_group_mbid \
         FROM local_tracks t \
         JOIN local_albums a ON a.id = t.local_album_id \
         LEFT JOIN local_track_artists ta ON ta.local_track_id = t.id AND ta.position = 0 \
         LEFT JOIN local_track_external_identities te ON te.local_track_id = t.id \
             AND te.provider = 'musicbrainz' \
         LEFT JOIN local_artist_external_identities tae ON tae.local_artist_id = ta.local_artist_id \
             AND tae.provider = 'musicbrainz' \
         LEFT JOIN local_artist_external_identities aae ON aae.local_artist_id = a.album_artist_id \
             AND aae.provider = 'musicbrainz' \
         LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
             AND ae.provider = 'musicbrainz' \
         WHERE t.availability = 'indexed' AND a.retired_into_album_id IS NULL";
    /// Radio's tracks by track or album artist, in random order.
    family RADIO_TRACKS_BY_ARTISTS = radio_tracks_by_artists;
    /// Radio's tracks of some albums, in disc and track order.
    family RADIO_TRACKS_OF_ALBUMS = radio_tracks_of_albums;
    pub(super) row RadioTrackColumns for family [radio_tracks_by_artists, radio_tracks_of_albums] {
        id: String,
        title: String,
        artist_name: String,
        album_title: Option<String>,
        file_format: Option<String>,
        duration_seconds: Option<f64>,
        recording_mbid: Option<String>,
        artist_mbid: String,
        album_artist_mbid: String,
        release_group_mbid: Option<String>,
    }
    /// A playlist the caller may read: their own or a public one.
    pub(super) VISIBLE_PLAYLIST = "SELECT id FROM catalog_playlists \
         WHERE id = ?1 AND (user_id = ?2 OR is_public = 1)";
    /// A playlist's track artists, in playlist order.
    pub(super) PLAYLIST_TRACK_ARTISTS = "SELECT artist_id FROM catalog_playlist_tracks \
         WHERE playlist_id = ?1 ORDER BY position";

    // -- settings and status ----------------------------------------------------

    /// The sections of a page a user switched off.
    pub(super) SECTIONS_SWITCHED_OFF = "SELECT section_key FROM app_section_prefs \
         WHERE user_id = ?1 AND page = ?2 AND enabled = 0";
    /// Whether local files back playback.
    pub(super) HAS_LOCAL_FILES = "SELECT EXISTS(SELECT 1 FROM local_tracks \
         WHERE availability = 'indexed')";

    // -- the page cache ---------------------------------------------------------

    /// The clear generation now.
    pub(super) PAGE_GENERATION = "SELECT generation FROM catalog_page_generations \
         WHERE singleton = 1";
    /// The clear generation and a user's row, read together.
    pub(super) CACHED_PAGE = "SELECT g.generation AS generation, p.source AS source, \
         p.listenbrainz AS listenbrainz, p.lastfm AS lastfm, p.payload AS payload, \
         p.built_at AS built_at \
         FROM catalog_page_generations g \
         LEFT JOIN catalog_cached_pages p ON p.user_id = ?1 AND p.page = ?2 \
         WHERE g.singleton = 1";
    pub(super) row CachedPageColumns for [CACHED_PAGE] {
        generation: i64,
        source: Option<String>,
        listenbrainz: Option<bool>,
        lastfm: Option<bool>,
        payload: Option<Vec<u8>>,
        built_at: Option<Millis>,
    }
    /// Keep a user's page while the user exists and the generation holds.
    pub(super) SAVE_CACHED_PAGE = "INSERT INTO catalog_cached_pages \
             (user_id, page, source, listenbrainz, lastfm, payload, built_at) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
           AND (SELECT generation FROM catalog_page_generations WHERE singleton = 1) = ?8 \
         ON CONFLICT(user_id, page) DO UPDATE SET \
             source = excluded.source, listenbrainz = excluded.listenbrainz, \
             lastfm = excluded.lastfm, payload = excluded.payload, \
             built_at = excluded.built_at";
    /// Drop one user's page.
    pub(super) FORGET_CACHED_PAGE = "DELETE FROM catalog_cached_pages \
         WHERE user_id = ?1 AND page = ?2";
    /// Drop every page.
    pub(super) CLEAR_CACHED_PAGES = "DELETE FROM catalog_cached_pages";
    /// Move to the next clear generation.
    pub(super) NEXT_PAGE_GENERATION = "UPDATE catalog_page_generations \
         SET generation = generation + 1 WHERE singleton = 1";

    // -- activity ---------------------------------------------------------------

    /// Record that a user used a feature. A source switch resets the
    /// row's schedule.
    pub(super) RECORD_ACTIVITY = "INSERT INTO catalog_discover_activity \
             (user_id, feature, artist_mbid, section, provider, source, used_at) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
         ON CONFLICT(user_id, feature, artist_mbid, section, provider) DO UPDATE SET \
             used_at = excluded.used_at, source = excluded.source, \
             retry_at = CASE WHEN catalog_discover_activity.source != excluded.source \
                 THEN 0 ELSE catalog_discover_activity.retry_at END";
    /// Forget activity last used at or before a time.
    pub(super) PRUNE_ACTIVITY_BEFORE = "DELETE FROM catalog_discover_activity WHERE used_at <= ?1";
    /// Forget all but each user's newest rows.
    pub(super) PRUNE_ACTIVITY_PER_USER = "DELETE FROM catalog_discover_activity WHERE rowid IN (\
             SELECT rowid FROM (SELECT rowid, ROW_NUMBER() OVER (\
                 PARTITION BY user_id ORDER BY used_at DESC, rowid DESC) AS n \
             FROM catalog_discover_activity) WHERE n > ?1)";
    /// Activity used after a time and due by another, least recently
    /// serviced first, optionally for one user.
    pub(super) DUE_ACTIVITY = "SELECT user_id, feature, artist_mbid, section, provider, source \
         FROM catalog_discover_activity \
         WHERE source = ?1 AND used_at > ?2 AND retry_at <= ?3 \
           AND (?4 IS NULL OR user_id = ?4) \
         ORDER BY serviced_at, user_id, feature, artist_mbid LIMIT 100";
    pub(super) row ActivityColumns for [DUE_ACTIVITY] {
        user_id: String,
        feature: String,
        artist_mbid: String,
        section: String,
        provider: String,
        source: String,
    }
    /// Mark one activity row serviced and when it is due again.
    pub(super) FINISH_ACTIVITY = "UPDATE catalog_discover_activity \
         SET serviced_at = ?1, retry_at = ?2 \
         WHERE user_id = ?3 AND feature = ?4 AND artist_mbid = ?5 \
           AND section = ?6 AND provider = ?7 AND source = ?8";

    // -- the ignore ledger, the release-group map, learned genres ---------------

    /// Lowercased release-group ids a user ignored.
    pub(super) IGNORED_MBIDS = "SELECT release_group_mbid_lower FROM catalog_ignored_releases \
         WHERE user_id = ?1";
    /// A user's ignore ledger, newest first.
    pub(super) IGNORED_RELEASES = "SELECT release_group_mbid, artist_mbid, release_name, \
         artist_name, ignored_at \
         FROM catalog_ignored_releases WHERE user_id = ?1 ORDER BY ignored_at DESC";
    pub(super) row IgnoredColumns for [IGNORED_RELEASES] {
        release_group_mbid: String,
        artist_mbid: String,
        release_name: String,
        artist_name: String,
        ignored_at: Millis,
    }
    /// Record one ignore; again refreshes its names and time.
    pub(super) IGNORE_RELEASE = "INSERT INTO catalog_ignored_releases (user_id, \
             release_group_mbid_lower, release_group_mbid, artist_mbid, release_name, \
             artist_name, ignored_at) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
         ON CONFLICT(user_id, release_group_mbid_lower) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             artist_mbid = excluded.artist_mbid, \
             release_name = excluded.release_name, \
             artist_name = excluded.artist_name, \
             ignored_at = excluded.ignored_at";
    /// Remove one of a user's ignores.
    pub(super) UNIGNORE_RELEASE = "DELETE FROM catalog_ignored_releases \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2";
    /// Drop ignores older than a time.
    pub(super) PRUNE_IGNORED = "DELETE FROM catalog_ignored_releases WHERE ignored_at < ?1";
    /// Known release to release-group answers.
    family RESOLUTIONS = resolutions;
    pub(super) row ResolutionColumns for family [resolutions] {
        source_mbid_lower: String,
        release_group_mbid: Option<String>,
    }
    /// Remember one release to release-group answer.
    pub(super) SAVE_RESOLUTION = "INSERT INTO catalog_mbid_resolutions \
             (source_mbid_lower, source_mbid, release_group_mbid) \
         VALUES (?1, ?1, ?2) \
         ON CONFLICT(source_mbid_lower) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid";
    /// Remember one genre learned for an artist.
    pub(super) LEARN_ARTIST_GENRE = "INSERT INTO catalog_artist_genres \
             (artist_mbid, genre_folded, genre, learned_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(artist_mbid, genre_folded) DO UPDATE SET \
             genre = excluded.genre, learned_at = excluded.learned_at";
    /// Drop pairs no build has seen since a time.
    pub(super) FORGET_ARTIST_GENRES = "DELETE FROM catalog_artist_genres WHERE learned_at < ?1";
}

/// `len` placeholders for an `IN (...)` list.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(",")
}

/// `OWNED_ALBUMS`.
pub(super) fn owned_albums(ids: ListLen) -> String {
    format!("{}{OLDEST_OWNERS}", asked_groups(ids))
}

/// `OWNED_ARTISTS`.
pub(super) fn owned_artists(ids: ListLen) -> String {
    format!("{OWNED_ARTISTS_START}{}{OWNED_ARTISTS_END}", marks(ids))
}

/// The genre filter for the genre credits.
fn genre_filter(genres: ListLen) -> String {
    format!("AND genre.folded_name IN ({})", marks(genres))
}

/// `GENRE_ARTIST_COUNTS`.
pub(super) fn genre_artist_counts(genres: ListLen) -> String {
    format!(
        "{GENRE_CREDITS_START}{}{GENRE_CREDITS_END}{GENRE_COUNTS}",
        genre_filter(genres)
    )
}

/// `ARTISTS_FOR_GENRES`.
pub(super) fn artists_for_genres(genres: ListLen) -> String {
    format!(
        "{GENRE_CREDITS_START}{}{GENRE_CREDITS_END}\
         SELECT DISTINCT gc.genre_folded AS genre_folded, \
         lower(aie.provider_artist_id) AS artist_mbid \
         FROM genre_credits gc JOIN local_artist_external_identities aie \
         ON aie.local_artist_id = gc.local_artist_id AND aie.provider = 'musicbrainz' \
         ORDER BY gc.genre_folded, artist_mbid",
        genre_filter(genres)
    )
}

/// `LIBRARY_ARTIST_NAMES`.
pub(super) fn library_artist_names(artists: ListLen) -> String {
    format!("{LIBRARY_ARTIST_NAMES_START}{})", marks(artists))
}

/// `LIBRARY_ARTIST_GENRES`.
pub(super) fn library_artist_genres(artists: ListLen) -> String {
    format!(
        "{LIBRARY_ARTIST_GENRES_START}{}{LIBRARY_ARTIST_GENRES_END}",
        marks(artists)
    )
}

/// `LEARNED_ARTIST_GENRES`.
pub(super) fn learned_artist_genres(artists: ListLen) -> String {
    format!(
        "SELECT artist_mbid AS mbid, genre_folded AS folded, genre AS name \
         FROM catalog_artist_genres \
         WHERE artist_mbid IN ({}) \
         ORDER BY artist_mbid, learned_at DESC, genre_folded",
        marks(artists)
    )
}

/// `ANNIVERSARY_ALBUMS`.
pub(super) fn anniversary_albums(years: ListLen) -> String {
    format!(
        "{ANNIVERSARY_ALBUMS_START}{}) \
         GROUP BY a.id ORDER BY a.year ASC, a.title COLLATE NOCASE, a.id LIMIT ?",
        marks(years)
    )
}

/// `GENRE_REVISIONS`.
pub(super) fn genre_revisions(genres: ListLen) -> String {
    format!(
        "SELECT genre_folded, value FROM catalog_genre_artwork_revisions \
         WHERE genre_folded IN ({})",
        marks(genres)
    )
}

/// `ART_CANDIDATES`: binds a folded genre and its ordinal for each genre.
pub(super) fn art_candidates(genres: ListLen) -> String {
    let values = vec!["(?,?)"; genres.get()].join(",");
    format!(
        "WITH requested(genre_folded, ordinal) AS (VALUES {values}), \
         candidates AS (SELECT r.genre_folded, a.id AS album_id, a.title AS album_title, \
           a.album_artist_name, a.album_artist_id, COUNT(*) AS match_count, \
           w.version AS cover_version, w.content_hash, w.source, \
           CASE WHEN w.source = 'embedded' THEN emb.file_path ELSE w.source_locator END AS path, \
           {tracks} AS tracks \
         FROM requested r JOIN local_track_genres g ON g.folded_name = r.genre_folded \
         JOIN local_tracks t ON t.id = g.local_track_id AND +t.availability = 'indexed' \
         JOIN local_albums a ON a.id = t.local_album_id AND a.retired_into_album_id IS NULL \
         JOIN local_album_artwork w ON w.local_album_id = a.id \
           AND w.source IN ('folder','embedded') \
         LEFT JOIN local_tracks emb ON emb.id = w.source_locator AND w.source = 'embedded' \
           AND emb.availability = 'indexed' \
         GROUP BY r.genre_folded, a.id) \
         SELECT r.genre_folded AS genre_folded, COALESCE(rev.value, 0) AS revision, \
           c.album_id AS album_id, c.album_title AS album_title, \
           c.album_artist_name AS album_artist_name, c.album_artist_id AS album_artist_id, \
           c.match_count AS match_count, c.cover_version AS cover_version, \
           c.content_hash AS content_hash, c.source AS source, c.path AS path, c.tracks AS tracks \
         FROM requested r \
         LEFT JOIN catalog_genre_artwork_revisions rev ON rev.genre_folded = r.genre_folded \
         LEFT JOIN candidates c ON c.genre_folded = r.genre_folded \
         ORDER BY r.ordinal, c.match_count DESC, c.album_id",
        tracks = art_tracks_column("emb")
    )
}

/// `RADIO_TRACKS_BY_ARTISTS`: binds the artists twice, then the limit.
pub(super) fn radio_tracks_by_artists(artists: ListLen) -> String {
    let marks = marks(artists);
    format!(
        "{RADIO_TRACK_COLUMNS}FROM ({RADIO_TRACK_SELECT}) WHERE release_group_mbid IS NOT NULL \
         AND (artist_mbid IN ({marks}) OR album_artist_mbid IN ({marks})) \
         ORDER BY RANDOM() LIMIT ?"
    )
}

/// `RADIO_TRACKS_OF_ALBUMS`: binds the albums, then the limit.
pub(super) fn radio_tracks_of_albums(albums: ListLen) -> String {
    format!(
        "{RADIO_TRACK_COLUMNS}FROM ({RADIO_TRACK_SELECT}) \
         WHERE release_group_mbid IN ({}) \
         ORDER BY disc_number, track_number, id LIMIT ?",
        marks(albums)
    )
}

/// `RESOLUTIONS`.
pub(super) fn resolutions(ids: ListLen) -> String {
    format!(
        "SELECT source_mbid_lower, release_group_mbid FROM catalog_mbid_resolutions \
         WHERE source_mbid_lower IN ({})",
        marks(ids)
    )
}
