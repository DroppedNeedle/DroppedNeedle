//! Discover's statements: what its pages read from the library, and its own
//! tables (the page cache, activity, what people told Discover, the
//! release-group map and the genres builds learned).

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;

use super::facts::{FactKind, FactSource};
use super::feedback::{FeedbackKind, FeedbackTarget};

use crate::artwork::local::art_tracks_column;
use crate::local::sql::{
    OPEN_REQUEST, live_album, musicbrainz, owned_album, owned_artist, track_codec,
};
use droppedneedle_library::identity::asked_groups;
use droppedneedle_library::{all_owners, not_yet_identified, owns_group, recording_mbid};

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

    /// Every album that owns a release group, by [`all_owners!`], the rule
    /// the serve's hide reads, with its album artist's MBID.
    pub(super) LIBRARY_ALBUMS = (concat!(
        "SELECT o.group_mbid AS release_group_mbid, \
         lower(aie.provider_artist_id) AS artist_mbid \
         FROM (",
        all_owners!(),
        ") o JOIN local_albums a ON a.id = o.album_id \
         LEFT JOIN local_artist_external_identities aie \
           ON aie.local_artist_id = a.album_artist_id AND ",
        musicbrainz!("aie")
    ));
    pub(super) row LibraryAlbumColumns for [LIBRARY_ALBUMS] {
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

    /// An album `a` with its identities and own art, its artist `an`.
    fragment HOME_ALBUM_COLUMNS = "SELECT a.id AS id, a.title AS title, \
         COALESCE(an.display_name, a.album_artist_name) AS artist_name, \
         (SELECT ae.release_group_mbid FROM local_album_external_identities ae \
          WHERE ae.local_album_id = a.id AND ae.provider = 'musicbrainz' \
          AND ae.release_group_mbid IS NOT NULL ORDER BY ae.release_group_mbid LIMIT 1) AS rg, \
         (SELECT aie.provider_artist_id FROM local_artist_external_identities aie \
          WHERE aie.local_artist_id = a.album_artist_id AND aie.provider = 'musicbrainz' \
          ORDER BY aie.provider_artist_id LIMIT 1) AS artist_mbid, \
         a.year AS year, \
         (SELECT w.version FROM local_album_artwork w WHERE w.local_album_id = a.id \
          AND w.source IN ('folder','embedded')) AS art_version ";
    /// A live album with its identities and own art.
    fragment HOME_ALBUM_SELECT = HOME_ALBUM_COLUMNS
        + "FROM local_albums a LEFT JOIN local_artists an ON an.id = a.album_artist_id \
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
    /// The albums a user played most since a time, with their plays: only
    /// albums the lists show, most played first, then the latest played.
    pub(super) PLAYED_ALBUMS = "WITH played AS (SELECT local_album_id AS album_id, \
         COUNT(*) AS plays, MAX(played_at) AS last_at FROM library_play_history \
         WHERE user_id = ?1 AND played_at >= ?2 AND local_album_id IS NOT NULL \
         GROUP BY local_album_id) "
        + HOME_ALBUM_COLUMNS
        + ", p.plays AS plays FROM played p JOIN local_albums a ON a.id = p.album_id \
         LEFT JOIN local_artists an ON an.id = a.album_artist_id WHERE "
        + (owned_album!("a"))
        + " ORDER BY p.plays DESC, p.last_at DESC, a.id LIMIT ?3";
    pub(super) row PlayedAlbumColumns for [PLAYED_ALBUMS] {
        id: String,
        title: String,
        artist_name: Option<String>,
        rg: Option<String>,
        artist_mbid: Option<String>,
        year: Option<i64>,
        art_version: Option<i64>,
        plays: i64,
    }
    /// The artists with the most albums. No placeholder ranks: not the
    /// sentinel rows (by kind), and not a row tags keyed for one of the
    /// names they go by, folded as the scan folds a display name.
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
           AND ar.kind NOT IN ('various_artists', 'unknown') \
           AND ar.folded_name NOT IN ('various artists', 'various', 'va', \
             'unknown artist', 'unknown artists') \
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
    /// A playlist the caller may read: their own, a public one, or one
    /// shared with them.
    pub(super) VISIBLE_PLAYLIST = "SELECT id FROM catalog_playlists p \
         WHERE id = ?1 AND (user_id = ?2 OR is_public = 1 OR EXISTS (SELECT 1 \
         FROM catalog_playlist_members m WHERE m.playlist_id = p.id AND m.user_id = ?2))";
    /// A playlist entry `p`: the stored artist id (a media server's own id
    /// for an imported playlist), the artist name, and the MusicBrainz id
    /// of the first credited artist of a library track with the entry's
    /// recording id (its identity, else the credit's or the file's id). The
    /// `t.id IN` list reaches the tracks through the lowered-id indexes.
    fragment PLAYLIST_ENTRY_SELECT = "SELECT p.artist_id AS artist_id, \
         p.artist_name AS artist_name, \
         (SELECT lower(COALESCE(ai.provider_artist_id, ta.artist_mbid, t.embedded_artist_mbid)) \
          FROM local_tracks t \
          JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
          LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id AND "
         + (musicbrainz!("ti"))
         + " JOIN local_track_artists ta ON ta.local_track_id = t.id AND ta.position = 0 \
          JOIN local_artists r ON r.id = ta.local_artist_id AND r.retired_into_artist_id IS NULL \
          LEFT JOIN local_artist_external_identities ai ON ai.local_artist_id = r.id AND "
         + (musicbrainz!("ai"))
         + " WHERE t.availability = 'indexed' AND p.recording_mbid <> '' \
            AND t.id IN (SELECT local_track_id FROM local_track_external_identities \
                WHERE lower(recording_mbid) = lower(p.recording_mbid) \
              UNION ALL SELECT id FROM local_tracks \
                WHERE lower(COALESCE(embedded_recording_mbid, '')) = lower(p.recording_mbid)) \
            AND lower(COALESCE("
         + (recording_mbid!("ti", "t"))
         + ", '')) = lower(p.recording_mbid) \
          ORDER BY t.id LIMIT 1) AS recording_artist_mbid ";
    /// A playlist's entries in playlist order.
    pub(super) PLAYLIST_ENTRIES = PLAYLIST_ENTRY_SELECT
        + "FROM catalog_playlist_tracks p WHERE p.playlist_id = ?1 ORDER BY p.position";
    pub(super) row PlaylistEntry for [PLAYLIST_ENTRIES] {
        artist_id: Option<String>,
        artist_name: String,
        recording_artist_mbid: Option<String>,
    }
    /// Every entry of a user's own playlists, leaving out the ones the app
    /// makes for them (the Weekly Mix), with whether its stored artist id
    /// names a library artist and that artist's MusicBrainz id.
    pub(super) SEED_PLAYLIST_ENTRIES = PLAYLIST_ENTRY_SELECT
        + ", EXISTS (SELECT 1 FROM local_artists la WHERE la.id = p.artist_id) AS local_artist, \
         (SELECT lower(le.provider_artist_id) FROM local_artist_external_identities le \
          WHERE le.local_artist_id = p.artist_id AND le.provider = 'musicbrainz') \
           AS local_artist_mbid \
         FROM catalog_playlists l JOIN catalog_playlist_tracks p ON p.playlist_id = l.id \
         WHERE l.user_id = ?1 \
           AND (l.source_ref IS NULL OR l.source_ref NOT LIKE 'personal-mix:%')";
    pub(super) row SeedPlaylistEntry for [SEED_PLAYLIST_ENTRIES] {
        artist_id: Option<String>,
        artist_name: String,
        recording_artist_mbid: Option<String>,
        local_artist: bool,
        local_artist_mbid: Option<String>,
    }

    // -- seeds: one person's own rows, each read filtered by their id -------------

    /// The library artist `s.artist_id` with its MusicBrainz id, not a
    /// placeholder row (by kind).
    fragment SEED_ARTIST = (concat!(
        " JOIN local_artists r ON r.id = s.artist_id \
         AND r.kind NOT IN ('various_artists', 'unknown') \
         JOIN local_artist_external_identities ai ON ai.local_artist_id = r.id AND ",
        musicbrainz!("ai"),
        " GROUP BY lower(ai.provider_artist_id)"
    ));
    /// A user's hearts per artist: on the artist, on an album (its album
    /// artist) and on a track (its first credited artist).
    pub(super) SEED_HEARTS = "WITH s(kind, artist_id) AS (\
         SELECT 'artist', item_id FROM catalog_user_favorites \
           WHERE user_id = ?1 AND item_kind = 'artist' \
         UNION ALL SELECT 'album', a.album_artist_id FROM catalog_user_favorites f \
           JOIN local_albums a ON a.id = f.item_id \
           WHERE f.user_id = ?1 AND f.item_kind = 'album' \
         UNION ALL SELECT 'track', ta.local_artist_id FROM catalog_user_favorites f \
           JOIN local_track_artists ta ON ta.local_track_id = f.item_id AND ta.position = 0 \
           WHERE f.user_id = ?1 AND f.item_kind = 'track') \
         SELECT lower(ai.provider_artist_id) AS mbid, MIN(r.display_name) AS name, \
         SUM(s.kind = 'artist') AS artists, SUM(s.kind = 'album') AS albums, \
         SUM(s.kind = 'track') AS tracks FROM s"
        + SEED_ARTIST;
    pub(super) row SeedHearts for [SEED_HEARTS] {
        mbid: String,
        name: String,
        artists: i64,
        albums: i64,
        tracks: i64,
    }
    /// A user's ratings per artist, reached as hearts are: how many fives,
    /// fours, and ones or twos.
    pub(super) SEED_RATINGS = "WITH s(rating, artist_id) AS (\
         SELECT rating, item_id FROM catalog_user_ratings \
           WHERE user_id = ?1 AND item_kind = 'artist' \
         UNION ALL SELECT f.rating, a.album_artist_id FROM catalog_user_ratings f \
           JOIN local_albums a ON a.id = f.item_id \
           WHERE f.user_id = ?1 AND f.item_kind = 'album' \
         UNION ALL SELECT f.rating, ta.local_artist_id FROM catalog_user_ratings f \
           JOIN local_track_artists ta ON ta.local_track_id = f.item_id AND ta.position = 0 \
           WHERE f.user_id = ?1 AND f.item_kind = 'track') \
         SELECT lower(ai.provider_artist_id) AS mbid, MIN(r.display_name) AS name, \
         SUM(s.rating = 5) AS fives, SUM(s.rating = 4) AS fours, \
         SUM(s.rating <= 2) AS lows FROM s"
        + SEED_ARTIST;
    pub(super) row SeedRatings for [SEED_RATINGS] {
        mbid: String,
        name: String,
        fives: i64,
        fours: i64,
        lows: i64,
    }
    /// The artists a user follows.
    pub(super) SEED_FOLLOWS = "SELECT artist_mbid_lower AS mbid, artist_name AS name \
         FROM acquire_followed_artists WHERE user_id = ?1";
    pub(super) row SeedFollow for [SEED_FOLLOWS] {
        mbid: String,
        name: String,
    }
    /// The albums a user asked for per artist: asked by a person (not
    /// for a follow, a watch, an upgrade or an edition change), leaving out
    /// the refused and the withdrawn.
    pub(super) SEED_REQUESTS = "SELECT lower(h.artist_mbid) AS mbid, \
         MIN(h.artist_name) AS name, COUNT(*) AS albums \
         FROM request_history_requesters q JOIN request_history h \
           ON h.musicbrainz_id_lower = q.musicbrainz_id_lower \
         WHERE q.user_id = ?1 AND h.request_kind = 'album' \
           AND h.origin IN ('user', 'approval', 'retry') \
           AND h.status NOT IN ('rejected', 'cancelled') AND h.artist_mbid IS NOT NULL \
         GROUP BY lower(h.artist_mbid)";
    pub(super) row SeedRequests for [SEED_REQUESTS] {
        mbid: String,
        name: String,
        albums: i64,
    }
    /// Live library artists with a MusicBrainz id, up to the folded-name
    /// list.
    fragment NAMED_ARTISTS_START = (concat!(
        "SELECT a.display_name AS name, \
         lower(ai.provider_artist_id) AS mbid FROM local_artists a \
         JOIN local_artist_external_identities ai ON ai.local_artist_id = a.id AND ",
        musicbrainz!("ai"),
        " WHERE a.retired_into_artist_id IS NULL AND a.kind <> 'various_artists' \
             AND a.folded_name IN ("
    ));
    /// Live library artists with a MusicBrainz id, by folded name.
    family NAMED_ARTISTS = named_artists;
    pub(super) row NamedArtist for family [named_artists] {
        name: String,
        mbid: String,
    }

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
    /// Drop every page of one user.
    pub(super) FORGET_USER_PAGES = "DELETE FROM catalog_cached_pages WHERE user_id = ?1";
    /// Drop every page.
    pub(super) CLEAR_CACHED_PAGES = "DELETE FROM catalog_cached_pages";
    /// Move to the next clear generation.
    pub(super) NEXT_PAGE_GENERATION = "UPDATE catalog_page_generations \
         SET generation = generation + 1 WHERE singleton = 1";
    /// Every user's saved page of two kinds.
    pub(super) SAVED_PAGES_OF_KINDS = "SELECT user_id, page, payload, built_at \
         FROM catalog_cached_pages WHERE page IN (?1, ?2) ORDER BY user_id, page";
    pub(super) row SavedPageColumns for [SAVED_PAGES_OF_KINDS] {
        user_id: String,
        page: String,
        payload: Vec<u8>,
        built_at: Millis,
    }

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
    /// Forget every activity row of one user.
    pub(super) FORGET_ACTIVITY = "DELETE FROM catalog_discover_activity WHERE user_id = ?1";
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

    // -- what people told Discover, the release-group map, learned genres -------

    /// The targets a person's answers hide at a time.
    pub(super) FEEDBACK_IN_EFFECT = "SELECT target, target_mbid_lower \
         FROM catalog_discover_feedback \
         WHERE user_id = ?1 AND (expires_at IS NULL OR expires_at > ?2)";
    pub(super) row FeedbackTargetColumns for [FEEDBACK_IN_EFFECT] {
        target: FeedbackTarget,
        target_mbid_lower: String,
    }
    /// A person's answers in effect at a time, newest first.
    pub(super) FEEDBACK_LIST = "SELECT target, target_mbid_lower, kind, \
             release_group_mbid, artist_mbid, release_name, artist_name, created_at, \
             expires_at \
         FROM catalog_discover_feedback \
         WHERE user_id = ?1 AND (expires_at IS NULL OR expires_at > ?2) \
         ORDER BY created_at DESC, target, target_mbid_lower";
    pub(super) row FeedbackColumns for [FEEDBACK_LIST] {
        target: FeedbackTarget,
        target_mbid_lower: String,
        kind: FeedbackKind,
        release_group_mbid: String,
        artist_mbid: String,
        release_name: String,
        artist_name: String,
        created_at: Millis,
        expires_at: Option<Millis>,
    }
    /// Record one answer for a person who still exists; a later answer
    /// about the same target replaces it.
    pub(super) SAVE_FEEDBACK = "INSERT INTO catalog_discover_feedback (user_id, target, \
             target_mbid_lower, kind, release_group_mbid, artist_mbid, release_name, \
             artist_name, created_at, expires_at, made_from) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
         ON CONFLICT(user_id, target, target_mbid_lower) DO UPDATE SET \
             kind = excluded.kind, \
             release_group_mbid = excluded.release_group_mbid, \
             artist_mbid = excluded.artist_mbid, \
             release_name = excluded.release_name, \
             artist_name = excluded.artist_name, \
             created_at = excluded.created_at, \
             expires_at = excluded.expires_at, \
             made_from = excluded.made_from";
    /// Take back one of a person's answers.
    pub(super) UNDO_FEEDBACK = "DELETE FROM catalog_discover_feedback \
         WHERE user_id = ?1 AND target = ?2 AND target_mbid_lower = ?3";
    /// Drop answers past their expiry (?1), and carried hides (?3, with an
    /// expiry) made before the retention's start (?2).
    pub(super) PRUNE_FEEDBACK = "DELETE FROM catalog_discover_feedback \
         WHERE expires_at <= ?1 \
            OR (kind = ?3 AND expires_at IS NOT NULL AND created_at < ?2)";

    // -- the Later pile ----------------------------------------------------------

    /// Put album ?2 (as sent; keyed lowercased) on person ?1's pile at ?6,
    /// while they exist. An album already there keeps its row.
    pub(super) LATER_MARK = "INSERT INTO catalog_later_albums (user_id, \
             release_group_mbid_lower, release_group_mbid, title, artist_mbid, artist_name, \
             added_at) \
         SELECT ?1, lower(?2), ?2, ?3, ?4, ?5, ?6 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
         ON CONFLICT(user_id, release_group_mbid_lower) DO NOTHING";
    /// Take album ?2 (lowercased) off person ?1's pile.
    pub(super) LATER_REMOVE = "DELETE FROM catalog_later_albums \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2";
    /// Take every album the library now owns off person ?1's pile.
    pub(super) LATER_DROP_OWNED = "DELETE FROM catalog_later_albums \
         WHERE user_id = ?1 AND EXISTS ("
        + (owns_group!("= +catalog_later_albums.release_group_mbid_lower"))
        + ")";
    /// Person ?1's pile as listed: the albums the library does not own.
    fragment LATER_LISTED = " FROM catalog_later_albums l WHERE l.user_id = ?1 \
         AND NOT EXISTS ("
        + (owns_group!("= +l.release_group_mbid_lower"))
        + ")";
    /// One page of person ?1's pile, newest first, starting after the row
    /// added at ?2 with key ?3 (none: the first page), ?4 rows, only album
    /// ?5 (lowercased) when one is named; each with whether an album request
    /// for it is open.
    pub(super) LATER_PAGE = "SELECT l.release_group_mbid_lower AS release_group_mbid_lower, \
         l.release_group_mbid AS release_group_mbid, l.title AS title, \
         l.artist_mbid AS artist_mbid, l.artist_name AS artist_name, l.added_at AS added_at, \
         EXISTS (SELECT 1 FROM request_history \
             WHERE musicbrainz_id_lower = l.release_group_mbid_lower AND "
        + OPEN_REQUEST
        + ") AS requested"
        + LATER_LISTED
        + " AND (?5 IS NULL OR l.release_group_mbid_lower = ?5) \
         AND (?2 IS NULL OR l.added_at < ?2 \
             OR (l.added_at = ?2 AND l.release_group_mbid_lower < ?3)) \
         ORDER BY l.added_at DESC, l.release_group_mbid_lower DESC LIMIT ?4";
    pub(super) row LaterRow for [LATER_PAGE] {
        release_group_mbid_lower: String,
        release_group_mbid: String,
        title: String,
        artist_mbid: Option<String>,
        artist_name: String,
        added_at: Millis,
        requested: bool,
    }
    /// How many albums person ?1's pile lists, only album ?2 (lowercased)
    /// when one is named.
    pub(super) LATER_COUNT = "SELECT COUNT(*)"
        + LATER_LISTED
        + " AND (?2 IS NULL OR l.release_group_mbid_lower = ?2)";
    /// Every album on person ?1's pile, lowercased.
    pub(super) LATER_GROUPS = "SELECT release_group_mbid_lower FROM catalog_later_albums \
         WHERE user_id = ?1";
    /// The artists of the albums person ?1's pile lists, one row an artist.
    pub(super) SEED_LATER = "SELECT lower(l.artist_mbid) AS mbid, MIN(l.artist_name) AS name"
        + LATER_LISTED
        + " AND l.artist_mbid IS NOT NULL GROUP BY lower(l.artist_mbid)";
    pub(super) row SeedLater for [SEED_LATER] {
        mbid: String,
        name: String,
    }

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

    // -- the pick record ---------------------------------------------------------

    /// Record one offer (?1 user, ?2 lowercased group, ?3 group as given,
    /// ?4 artist, ?5 album, ?6 artist name, ?7 surface, ?8 now, ?9
    /// evidence) for a person who still exists. A row offered before keeps
    /// its first surface and date.
    pub(super) OFFER_PICK = "INSERT INTO catalog_discover_picks (user_id, \
             release_group_mbid_lower, release_group_mbid, artist_mbid, album_name, \
             artist_name, first_surface, first_offered_at, last_offered_at, evidence) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
         ON CONFLICT(user_id, release_group_mbid_lower) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             artist_mbid = excluded.artist_mbid, \
             album_name = excluded.album_name, \
             artist_name = excluded.artist_name, \
             last_offered_at = MAX(catalog_discover_picks.last_offered_at, \
                 excluded.last_offered_at), \
             evidence = excluded.evidence";
    /// Person ?1's pick ?2 was shown at ?3: counted once per UTC day.
    pub(super) PICK_SHOWN = "UPDATE catalog_discover_picks SET \
             first_shown_at = COALESCE(first_shown_at, ?3), \
             last_shown_at = MAX(COALESCE(last_shown_at, ?3), ?3), \
             shown_days = shown_days + (last_shown_at IS NULL \
                 OR last_shown_at / 86400000 < ?3 / 86400000) \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2";
    /// Person ?1's pick ?2 was first sampled at ?3.
    pub(super) PICK_SAMPLED = "UPDATE catalog_discover_picks \
         SET sampled_at = COALESCE(sampled_at, ?3) \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2";
    /// Person ?1's pick ?2 was first opened at ?3.
    pub(super) PICK_OPENED = "UPDATE catalog_discover_picks \
         SET opened_at = COALESCE(opened_at, ?3) \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2";
    /// Every row of person ?1's.
    pub(super) FORGET_PICKS = "DELETE FROM catalog_discover_picks WHERE user_id = ?1";
    /// The people with rows in the pick record.
    pub(super) PICK_USERS = "SELECT DISTINCT user_id FROM catalog_discover_picks ORDER BY user_id";
    /// Whether person ?1 has rows in the pick record.
    pub(super) USER_HAS_PICKS = "SELECT EXISTS (SELECT 1 FROM catalog_discover_picks \
         WHERE user_id = ?1)";
    /// Person ?1's first ask for each pick at or after it was first offered,
    /// and where it was made. A request seen once is kept.
    pub(super) PICKS_REQUESTED = "UPDATE catalog_discover_picks \
         SET requested_at = r.requested_at, requested_from = r.made_from \
         FROM request_history_requesters r \
         WHERE catalog_discover_picks.user_id = ?1 AND r.user_id = ?1 \
           AND r.musicbrainz_id_lower = catalog_discover_picks.release_group_mbid_lower \
           AND catalog_discover_picks.requested_at IS NULL \
           AND r.requested_at >= catalog_discover_picks.first_offered_at";
    /// When each of person ?1's picks arrived: the oldest album that owns
    /// its release group and came in once the pick was offered. An arrival
    /// seen once is kept.
    pub(super) PICKS_ARRIVED = (concat!(
        "WITH first_owned AS (SELECT p.release_group_mbid_lower AS g, \
             MIN(a.created_at) AS at \
         FROM catalog_discover_picks p JOIN local_albums a \
           ON a.created_at >= p.first_offered_at AND a.id IN (SELECT album_id FROM (",
        owns_group!("= +p.release_group_mbid_lower"),
        ")) WHERE p.user_id = ?1 AND p.arrived_at IS NULL \
         GROUP BY p.release_group_mbid_lower) \
         UPDATE catalog_discover_picks SET arrived_at = first_owned.at FROM first_owned \
         WHERE catalog_discover_picks.user_id = ?1 \
           AND catalog_discover_picks.release_group_mbid_lower = first_owned.g \
           AND catalog_discover_picks.arrived_at IS NULL"
    ));
    /// Person ?1's plays of each arrived pick in its first ?2 ms (90 days)
    /// after arrival: the first, and how many in the first ?3 ms and in all
    /// of it. A play is of the pick when it is on an album that owns the
    /// release group (read through the album index) or names the group
    /// (read through the person's plays in the window). It is one pass over
    /// the person's plays per branch, each pick's stopping at its window,
    /// read on the long pool outside any write.
    pub(super) PICK_PLAYS = (concat!(
        "WITH picks AS MATERIALIZED (SELECT release_group_mbid_lower AS g, \
             arrived_at AS at FROM catalog_discover_picks \
             WHERE user_id = ?1 AND arrived_at IS NOT NULL), \
         owners AS MATERIALIZED (SELECT p.g AS g, p.at AS at, a.id AS album_id \
         FROM picks p JOIN local_albums a ON a.id IN (SELECT album_id FROM (",
        owns_group!("= +p.g"),
        "))), \
         plays AS (SELECT o.g AS g, h.id AS play, h.played_at - o.at AS since \
             FROM owners o JOIN library_play_history h ON h.local_album_id = o.album_id \
             WHERE h.user_id = ?1 AND h.played_at >= o.at AND h.played_at < o.at + ?2 \
           UNION SELECT p.g, h.id, h.played_at - p.at \
             FROM picks p JOIN library_play_history h ON h.user_id = ?1 \
               AND h.played_at >= p.at AND h.played_at < p.at + ?2 \
             WHERE lower(h.release_group_mbid) = p.g) \
         SELECT g AS release_group_mbid_lower, MIN(since) AS first_since, \
             SUM(since < ?3) AS plays_30d, COUNT(*) AS plays_90d \
         FROM plays GROUP BY g ORDER BY g"
    ));
    pub(super) row PickPlaysColumns for [PICK_PLAYS] {
        release_group_mbid_lower: String,
        first_since: i64,
        plays_30d: i64,
        plays_90d: i64,
    }
    /// Set person ?1's pick ?2: first played ?3 ms after arrival, ?4 and ?5
    /// plays. A row already holding these is left alone.
    pub(super) PICK_PLAYS_SET = "UPDATE catalog_discover_picks \
         SET first_played_at = arrived_at + ?3, plays_30d = ?4, plays_90d = ?5 \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2 AND arrived_at IS NOT NULL \
           AND (first_played_at IS NOT arrived_at + ?3 OR plays_30d IS NOT ?4 \
             OR plays_90d IS NOT ?5)";
    /// Clear the play counts of person ?1's picks not in the JSON list ?2.
    pub(super) PICK_PLAYS_CLEARED = "UPDATE catalog_discover_picks \
         SET first_played_at = NULL, plays_30d = 0, plays_90d = 0 \
         WHERE user_id = ?1 \
           AND (first_played_at IS NOT NULL OR plays_30d != 0 OR plays_90d != 0) \
           AND release_group_mbid_lower NOT IN (SELECT value FROM json_each(?2))";
    /// What person ?1 told Discover about each pick: about its album (?2)
    /// first, else about its artist (?3). A row already holding it is left
    /// alone.
    pub(super) PICKS_FEEDBACK = "WITH told AS (SELECT p.release_group_mbid_lower AS g, \
             (SELECT f.kind FROM catalog_discover_feedback f WHERE f.user_id = p.user_id \
                   AND ((f.target = ?2 AND f.target_mbid_lower = p.release_group_mbid_lower) \
                     OR (f.target = ?3 AND p.artist_mbid != '' \
                       AND f.target_mbid_lower = lower(p.artist_mbid))) \
                 ORDER BY f.target = ?2 DESC LIMIT 1) AS kind, \
             (SELECT f.created_at FROM catalog_discover_feedback f WHERE f.user_id = p.user_id \
                   AND ((f.target = ?2 AND f.target_mbid_lower = p.release_group_mbid_lower) \
                     OR (f.target = ?3 AND p.artist_mbid != '' \
                       AND f.target_mbid_lower = lower(p.artist_mbid))) \
                 ORDER BY f.target = ?2 DESC LIMIT 1) AS at \
         FROM catalog_discover_picks p WHERE p.user_id = ?1) \
         UPDATE catalog_discover_picks SET feedback_kind = told.kind, feedback_at = told.at \
         FROM told WHERE catalog_discover_picks.user_id = ?1 \
           AND catalog_discover_picks.release_group_mbid_lower = told.g \
           AND (catalog_discover_picks.feedback_kind IS NOT told.kind \
             OR catalog_discover_picks.feedback_at IS NOT told.at)";
    /// Drop person ?1's rows with no outcome: never shown and last offered
    /// before ?2, or shown and last offered or shown before ?3.
    pub(super) PICKS_RETAINED = "DELETE FROM catalog_discover_picks WHERE user_id = ?1 \
         AND sampled_at IS NULL AND opened_at IS NULL AND requested_at IS NULL \
         AND arrived_at IS NULL AND first_played_at IS NULL AND feedback_kind IS NULL \
         AND let_go_at IS NULL \
         AND ((first_shown_at IS NULL AND last_offered_at < ?2) \
           OR (first_shown_at IS NOT NULL \
             AND MAX(last_offered_at, COALESCE(last_shown_at, 0)) < ?3))";

    // -- kept facts ------------------------------------------------------------

    /// The fetch rows of kind ?1 from source ?2 for the artists in the JSON
    /// list ?3.
    pub(super) FACT_FETCHES = "SELECT artist_mbid, fetched_at, read_at, wanted_at \
         FROM catalog_fact_fetches WHERE kind = ?1 AND source = ?2 \
           AND artist_mbid IN (SELECT value FROM json_each(?3))";
    pub(super) row FactFetchColumns for [FACT_FETCHES] {
        artist_mbid: String,
        fetched_at: Option<Millis>,
        read_at: Millis,
        wanted_at: Option<Millis>,
    }
    /// The kept similar artists from source ?1 of the artists in the JSON
    /// list ?2, best first by the provider's own rank.
    pub(super) KEPT_NEIGHBOURS = "SELECT artist_mbid, neighbour_mbid, neighbour_name, score \
         FROM catalog_artist_neighbours WHERE source = ?1 \
           AND artist_mbid IN (SELECT value FROM json_each(?2)) ORDER BY artist_mbid, rank";
    pub(super) row KeptNeighbourColumns for [KEPT_NEIGHBOURS] {
        artist_mbid: String,
        neighbour_mbid: String,
        neighbour_name: Option<String>,
        score: f64,
    }
    /// The kept counts of the artists in the JSON list ?1.
    pub(super) KEPT_LISTENERS = "SELECT artist_mbid, listeners, listens \
         FROM catalog_artist_listeners WHERE artist_mbid IN (SELECT value FROM json_each(?1))";
    pub(super) row KeptListenerColumns for [KEPT_LISTENERS] {
        artist_mbid: String,
        listeners: Option<i64>,
        listens: Option<i64>,
    }
    /// Artist ?1's kept albums, in its discography's order.
    pub(super) KEPT_ALBUMS = "SELECT release_group_mbid, title, artist_name, primary_type, \
         secondary_types, first_release_date, credited_artist_mbids, listeners, listens \
         FROM catalog_artist_albums WHERE artist_mbid = ?1 ORDER BY position";
    pub(super) row KeptAlbumColumns for [KEPT_ALBUMS] {
        release_group_mbid: String,
        title: String,
        artist_name: String,
        primary_type: Option<String>,
        secondary_types: String,
        first_release_date: Option<String>,
        credited_artist_mbids: Option<String>,
        listeners: Option<i64>,
        listens: Option<i64>,
    }
    /// The groups of the kept albums of the artists in the JSON list ?1.
    pub(super) KEPT_ALBUM_GROUPS = "SELECT DISTINCT release_group_mbid \
         FROM catalog_artist_albums WHERE artist_mbid IN (SELECT value FROM json_each(?1)) \
         ORDER BY release_group_mbid";
    /// Fact ?1, kind ?2, source ?3 answered (?4, `found` or `none`) at ?5,
    /// due again at ?6. A new row counts as read now.
    pub(super) FACT_ANSWERED = "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, \
         outcome, fetched_at, failing_since_at, failures, due_at, wanted_at, want_order, \
         read_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0, ?6, NULL, NULL, ?5) \
         ON CONFLICT (artist_mbid, kind, source) DO UPDATE SET outcome = excluded.outcome, \
           fetched_at = excluded.fetched_at, failing_since_at = NULL, failures = 0, \
           due_at = excluded.due_at, wanted_at = NULL, want_order = NULL";
    /// Fact ?1, kind ?2, source ?3 failed at ?4: its rows stay, the first
    /// failure's time is kept, and it is due again ?5 ms after ?4 doubled
    /// for each failure in a row, at most ?6 ms after. A new row is wanted
    /// with order ?7.
    pub(super) FACT_FAILED = "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, \
         outcome, fetched_at, failing_since_at, failures, due_at, wanted_at, want_order, \
         read_at) VALUES (?1, ?2, ?3, 'failed', NULL, ?4, 1, ?4 + ?5, ?4, ?7, ?4) \
         ON CONFLICT (artist_mbid, kind, source) DO UPDATE SET outcome = 'failed', \
           failing_since_at = COALESCE(failing_since_at, excluded.failing_since_at), \
           failures = failures + 1, due_at = ?4 + MIN(?5 << MIN(failures, 10), ?6)";
    /// Fact ?1, kind ?2, source ?3 could not answer yet (a discography still
    /// being walked): asked again at ?5. A new row is wanted at ?4 with
    /// order ?6.
    pub(super) FACT_NOT_YET = "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, \
         due_at, wanted_at, want_order, read_at) VALUES (?1, ?2, ?3, ?5, ?4, ?6, ?4) \
         ON CONFLICT (artist_mbid, kind, source) DO UPDATE SET due_at = excluded.due_at";
    /// The facts of kind ?1 from source ?2 for the artists in the JSON list
    /// ?3 were read at ?4.
    pub(super) FACTS_READ = "UPDATE catalog_fact_fetches SET read_at = ?4 \
         WHERE kind = ?1 AND source = ?2 AND artist_mbid IN (SELECT value FROM json_each(?3)) \
           AND read_at < ?4";
    /// The facts of kind ?1 from source ?2 for the artists in the JSON list
    /// ?3 were wanted at ?4 with order ?5. One that has answered is left
    /// alone; one already wanted keeps its earliest order.
    pub(super) FACTS_WANTED = "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, \
         due_at, wanted_at, want_order, read_at) \
         SELECT value, ?1, ?2, ?4, ?4, ?5, ?4 FROM json_each(?3) WHERE true \
         ON CONFLICT (artist_mbid, kind, source) DO UPDATE SET wanted_at = excluded.wanted_at, \
           want_order = MIN(COALESCE(want_order, excluded.want_order), excluded.want_order), \
           read_at = excluded.read_at \
         WHERE catalog_fact_fetches.fetched_at IS NULL";
    /// The facts of kind ?1 from source ?2 for the artists in the JSON list
    /// ?3 are wanted again now (?4) with order ?5, answered before or not.
    pub(super) FACTS_WANTED_AGAIN = "INSERT INTO catalog_fact_fetches (artist_mbid, kind, \
         source, due_at, wanted_at, want_order, read_at) \
         SELECT value, ?1, ?2, ?4, ?4, ?5, ?4 FROM json_each(?3) WHERE true \
         ON CONFLICT (artist_mbid, kind, source) DO UPDATE SET due_at = excluded.due_at, \
           wanted_at = excluded.wanted_at, \
           want_order = MIN(COALESCE(want_order, excluded.want_order), excluded.want_order)";
    /// Drop artist ?1's kept similar artists from source ?2.
    pub(super) NEIGHBOURS_DROPPED = "DELETE FROM catalog_artist_neighbours \
         WHERE artist_mbid = ?1 AND source = ?2";
    /// Keep artist ?1's similar artist ?4 (named ?5, scored ?6) from source
    /// ?2 at rank ?3.
    pub(super) NEIGHBOUR_KEPT = "INSERT INTO catalog_artist_neighbours (artist_mbid, source, \
         rank, neighbour_mbid, neighbour_name, score) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    /// Keep artist ?1's counts: ?2 listeners, ?3 listens.
    pub(super) LISTENERS_KEPT = "INSERT INTO catalog_artist_listeners (artist_mbid, listeners, \
         listens) VALUES (?1, ?2, ?3) ON CONFLICT (artist_mbid) DO UPDATE SET \
         listeners = excluded.listeners, listens = excluded.listens";
    /// Drop artist ?1's kept albums not in the JSON list ?2.
    pub(super) ALBUMS_DROPPED = "DELETE FROM catalog_artist_albums WHERE artist_mbid = ?1 \
         AND release_group_mbid NOT IN (SELECT value FROM json_each(?2))";
    /// Keep artist ?1's album ?2 at ?3 with its facts; a group kept before
    /// keeps its counts.
    pub(super) ALBUM_KEPT = "INSERT INTO catalog_artist_albums (artist_mbid, \
         release_group_mbid, position, title, artist_name, primary_type, secondary_types, \
         first_release_date, credited_artist_mbids, listeners, listens) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL) \
         ON CONFLICT (artist_mbid, release_group_mbid) DO UPDATE SET \
           position = excluded.position, title = excluded.title, \
           artist_name = excluded.artist_name, primary_type = excluded.primary_type, \
           secondary_types = excluded.secondary_types, \
           first_release_date = excluded.first_release_date, \
           credited_artist_mbids = excluded.credited_artist_mbids";
    /// Keep group ?1's counts on every kept album list: ?2 listeners, ?3
    /// listens.
    pub(super) ALBUM_COUNTS_KEPT = "UPDATE catalog_artist_albums SET listeners = ?2, \
         listens = ?3 WHERE release_group_mbid = ?1";
    /// The facts the refresh job may read now (?1): of the kinds in the
    /// JSON list ?2, due, and wanted or read since ?3 (a wanted fact
    /// someone still reads stays wanted), or answered and read since ?3.
    /// Wanted first, by order and age, then the oldest due; ?4 at most.
    pub(super) FACTS_TO_READ = "SELECT artist_mbid, kind, source FROM catalog_fact_fetches \
         WHERE due_at <= ?1 AND kind IN (SELECT value FROM json_each(?2)) \
           AND ((wanted_at IS NOT NULL AND (wanted_at >= ?3 OR read_at >= ?3)) \
             OR (wanted_at IS NULL AND fetched_at IS NOT NULL AND read_at >= ?3)) \
         ORDER BY wanted_at IS NULL, want_order, wanted_at, due_at, artist_mbid LIMIT ?4";
    pub(super) row FactToReadColumns for [FACTS_TO_READ] {
        artist_mbid: String,
        kind: FactKind,
        source: FactSource,
    }
    /// Whether any fact the refresh job may read now (?1) waits, picked as
    /// [`FACTS_TO_READ`] picks them.
    pub(super) FACTS_WAITING = "SELECT EXISTS (SELECT 1 FROM catalog_fact_fetches \
         WHERE due_at <= ?1 AND kind IN (SELECT value FROM json_each(?2)) \
           AND ((wanted_at IS NOT NULL AND (wanted_at >= ?3 OR read_at >= ?3)) \
             OR (wanted_at IS NULL AND fetched_at IS NOT NULL AND read_at >= ?3)))";
    /// Drop the kept similar artists of lists last read before ?1, found
    /// from their fetch rows.
    pub(super) UNREAD_NEIGHBOURS_DROPPED = "DELETE FROM catalog_artist_neighbours \
         WHERE (artist_mbid, source) IN (SELECT artist_mbid, source FROM catalog_fact_fetches \
           WHERE read_at < ?1 AND kind = 'neighbours')";
    /// Drop the kept counts last read before ?1, found from their fetch
    /// rows.
    pub(super) UNREAD_LISTENERS_DROPPED = "DELETE FROM catalog_artist_listeners \
         WHERE artist_mbid IN (SELECT artist_mbid FROM catalog_fact_fetches \
           WHERE read_at < ?1 AND kind = 'listeners')";
    /// Drop the kept album lists last read before ?1, found from their
    /// fetch rows.
    pub(super) UNREAD_ALBUMS_DROPPED = "DELETE FROM catalog_artist_albums \
         WHERE artist_mbid IN (SELECT artist_mbid FROM catalog_fact_fetches \
           WHERE read_at < ?1 AND kind = 'albums')";
    /// Drop the fetch rows last read before ?1.
    pub(super) UNREAD_FETCHES_DROPPED = "DELETE FROM catalog_fact_fetches WHERE read_at < ?1";
    /// Every kept fact row.
    pub(super) KEPT_FACT_ROWS = "SELECT (SELECT COUNT(*) FROM catalog_artist_neighbours) \
         + (SELECT COUNT(*) FROM catalog_artist_listeners) \
         + (SELECT COUNT(*) FROM catalog_artist_albums)";
    /// Drop every kept similar artist.
    pub(super) ALL_NEIGHBOURS_DROPPED = "DELETE FROM catalog_artist_neighbours";
    /// Drop every kept count.
    pub(super) ALL_LISTENERS_DROPPED = "DELETE FROM catalog_artist_listeners";
    /// Drop every kept album list.
    pub(super) ALL_ALBUMS_DROPPED = "DELETE FROM catalog_artist_albums";
    /// Drop every fetch row.
    pub(super) ALL_FETCHES_DROPPED = "DELETE FROM catalog_fact_fetches";
    /// Count ?3 background reads to ?2 on UTC day ?1.
    pub(super) READS_COUNTED = "INSERT INTO catalog_background_read_days (day, source, reads) \
         VALUES (?1, ?2, ?3) ON CONFLICT (day, source) DO UPDATE SET \
         reads = reads + excluded.reads";
    /// The background reads counted on UTC day ?1, every source together.
    pub(super) READS_ON_DAY = "SELECT COALESCE(SUM(reads), 0) \
         FROM catalog_background_read_days WHERE day = ?1";
    /// Drop the read counts of days before ?1.
    pub(super) READ_DAYS_DROPPED = "DELETE FROM catalog_background_read_days WHERE day < ?1";
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

/// `NAMED_ARTISTS`.
pub(super) fn named_artists(names: ListLen) -> String {
    format!("{NAMED_ARTISTS_START}{})", marks(names))
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
