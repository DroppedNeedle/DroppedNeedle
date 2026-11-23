//! The engine's statements: a person's history as their taste is worked
//! out from it, their taste build and who is due one.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::time::Millis;

use crate::local::sql::musicbrainz;

droppedneedle_kernel::sql! {
    // -- history -------------------------------------------------------------

    /// A person's plays in the app and imported listens per artist, by
    /// MusicBrainz id: how many of each, in how many calendar months, the
    /// first and the latest, and how many since `?2`. A play filed by name
    /// alone, a placeholder artist row (by kind) and a listen with no artist
    /// id are left out.
    pub(in crate::discover) HISTORY_ARTISTS = "WITH s(mbid, name, at, app) AS (\
         SELECT lower(ai.provider_artist_id), r.display_name, h.played_at, 1 \
           FROM library_play_history h \
           JOIN local_artists r ON r.id = h.local_artist_id \
             AND r.kind NOT IN ('various_artists', 'unknown') \
           JOIN local_artist_external_identities ai ON ai.local_artist_id = r.id AND "
        + (musicbrainz!("ai"))
        + " WHERE h.user_id = ?1 \
         UNION ALL SELECT lower(l.artist_mbid), l.artist_name, l.listened_at, 0 \
           FROM media_imported_listens l \
           WHERE l.user_id = ?1 AND l.artist_mbid IS NOT NULL AND l.artist_mbid <> '') \
         SELECT mbid, MIN(name) AS name, SUM(app) AS plays, SUM(1 - app) AS imported_listens, \
         COUNT(DISTINCT strftime('%Y-%m', at / 1000, 'unixepoch')) AS months, \
         MIN(at) AS first_played_at, MAX(at) AS last_played_at, SUM(at >= ?2) AS recent \
         FROM s GROUP BY mbid";
    pub(in crate::discover) row HistoryArtist for [HISTORY_ARTISTS] {
        mbid: String,
        name: String,
        plays: i64,
        imported_listens: i64,
        months: i64,
        first_played_at: Millis,
        last_played_at: Millis,
        recent: i64,
    }
    /// The release groups a person played in the app, with how often and
    /// the latest.
    pub(in crate::discover) HISTORY_GROUPS = "SELECT lower(release_group_mbid) AS mbid, \
         COUNT(*) AS plays, MAX(played_at) AS last_played_at FROM library_play_history \
         WHERE user_id = ?1 AND release_group_mbid IS NOT NULL AND release_group_mbid <> '' \
         GROUP BY lower(release_group_mbid)";
    pub(in crate::discover) row HistoryGroup for [HISTORY_GROUPS] {
        mbid: String,
        plays: i64,
        last_played_at: Millis,
    }
    /// Whether a person's imported history is in state `?2`.
    pub(in crate::discover) IMPORT_IN_STATE = "SELECT EXISTS (SELECT 1 \
         FROM media_history_imports WHERE user_id = ?1 AND state = ?2)";
    /// The release groups a person played in the app since `?2`, most
    /// played first, then the latest played, `?3` from `?4`.
    pub(in crate::discover) TOP_PLAYED_GROUPS = "SELECT g.mbid AS release_group_mbid, \
         g.title AS title, g.artist_name AS artist_name, g.artist_mbid AS artist_mbid, \
         g.plays AS plays FROM (SELECT lower(h.release_group_mbid) AS mbid, \
           COALESCE(MIN(h.album_name), '') AS title, MIN(h.artist_name) AS artist_name, \
           MIN(lower(ai.provider_artist_id)) AS artist_mbid, COUNT(*) AS plays, \
           MAX(h.played_at) AS last_at \
           FROM library_play_history h \
           LEFT JOIN local_artist_external_identities ai \
             ON ai.local_artist_id = h.local_artist_id AND "
        + (musicbrainz!("ai"))
        + " WHERE h.user_id = ?1 AND h.played_at >= ?2 \
             AND h.release_group_mbid IS NOT NULL AND h.release_group_mbid <> '' \
           GROUP BY lower(h.release_group_mbid)) g \
         ORDER BY g.plays DESC, g.last_at DESC, g.mbid LIMIT ?3 OFFSET ?4";
    pub(in crate::discover) row TopPlayedGroup for [TOP_PLAYED_GROUPS] {
        release_group_mbid: String,
        title: String,
        artist_name: String,
        artist_mbid: Option<String>,
        plays: i64,
    }
    /// How many release groups a person played in the app since `?2`.
    pub(in crate::discover) PLAYED_GROUP_COUNT = "SELECT COUNT(DISTINCT \
         lower(release_group_mbid)) FROM library_play_history \
         WHERE user_id = ?1 AND played_at >= ?2 \
           AND release_group_mbid IS NOT NULL AND release_group_mbid <> ''";
    /// The albums a person said they know already, per artist.
    pub(in crate::discover) SEED_KNOWN_ALBUMS = "SELECT lower(artist_mbid) AS mbid, \
         MIN(artist_name) AS name, COUNT(*) AS albums FROM catalog_discover_feedback \
         WHERE user_id = ?1 AND target = ?2 AND kind = ?3 AND artist_mbid <> '' \
         GROUP BY lower(artist_mbid)";
    pub(in crate::discover) row SeedKnownAlbums for [SEED_KNOWN_ALBUMS] {
        mbid: String,
        name: String,
        albums: i64,
    }

    // -- the taste build -------------------------------------------------------

    /// Whether an account exists.
    pub(in crate::discover) USER_EXISTS = "SELECT EXISTS (SELECT 1 FROM auth_users \
         WHERE id = ?1)";
    /// A person's build row, if any, with each of their seeds: one row with
    /// no seed for a build that has none, no row at all without a build.
    pub(in crate::discover) TASTE_SEEDS = "SELECT a.artist_mbid_lower AS mbid, a.name AS name, \
         a.plays AS plays, a.weight AS weight, a.group_no AS group_no \
         FROM catalog_taste_builds b LEFT JOIN catalog_taste_artists a \
           ON a.user_id = b.user_id AND a.is_seed = 1 \
         WHERE b.user_id = ?1 ORDER BY a.weight DESC, a.artist_mbid_lower";
    pub(in crate::discover) row TasteSeed for [TASTE_SEEDS] {
        mbid: Option<String>,
        name: Option<String>,
        plays: Option<i64>,
        weight: Option<f64>,
        group_no: Option<i64>,
    }
    /// The group each of a person's grouped artists is in.
    pub(in crate::discover) TASTE_GROUP_NOS = "SELECT artist_mbid_lower, group_no \
         FROM catalog_taste_artists WHERE user_id = ?1 AND group_no IS NOT NULL";
    pub(in crate::discover) row TasteGroupNo for [TASTE_GROUP_NOS] {
        artist_mbid_lower: String,
        group_no: i64,
    }
    /// Whether a person has groups.
    pub(in crate::discover) TASTE_HAS_GROUPS = "SELECT EXISTS (SELECT 1 \
         FROM catalog_taste_groups WHERE user_id = ?1)";
    /// A person's groups that none of their artists is in.
    pub(in crate::discover) TASTE_GROUPS_UNUSED_DELETE = "DELETE FROM catalog_taste_groups \
         WHERE user_id = ?1 AND group_no NOT IN (SELECT group_no FROM catalog_taste_artists \
           WHERE user_id = ?1 AND group_no IS NOT NULL)";
    /// The release groups a person is known to have played.
    pub(in crate::discover) KNOWN_GROUPS = "SELECT release_group_mbid_lower \
         FROM catalog_known_release_groups WHERE user_id = ?1";
    /// A person's build row.
    pub(in crate::discover) TASTE_BUILD_DELETE = "DELETE FROM catalog_taste_builds \
         WHERE user_id = ?1";
    /// A person's groups.
    pub(in crate::discover) TASTE_GROUPS_DELETE = "DELETE FROM catalog_taste_groups \
         WHERE user_id = ?1";
    /// A person's known artists.
    pub(in crate::discover) TASTE_ARTISTS_DELETE = "DELETE FROM catalog_taste_artists \
         WHERE user_id = ?1";
    /// A person's known release groups.
    pub(in crate::discover) KNOWN_GROUPS_DELETE = "DELETE FROM catalog_known_release_groups \
         WHERE user_id = ?1";
    /// Keep a person's build row.
    pub(in crate::discover) TASTE_BUILD_INSERT = "INSERT INTO catalog_taste_builds \
         (user_id, built_at, plays_counted, imported_listens_counted, artists_played, seeds, \
          history_from_at, history_to_at, incomplete) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    /// Keep one of a person's groups.
    pub(in crate::discover) TASTE_GROUP_INSERT = "INSERT INTO catalog_taste_groups \
         (user_id, group_no, anchor_artist_mbid, anchor_name, share, recent_share, trend) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    /// Keep one of a person's known artists.
    pub(in crate::discover) TASTE_ARTIST_INSERT = "INSERT INTO catalog_taste_artists \
         (user_id, artist_mbid_lower, artist_mbid, name, plays, imported_listens, \
          months_active, last_played_at, weight, is_seed, group_no) \
         VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
    /// Keep one of a person's known release groups.
    pub(in crate::discover) KNOWN_GROUP_INSERT = "INSERT INTO catalog_known_release_groups \
         (user_id, release_group_mbid_lower, plays, last_played_at) VALUES (?1, ?2, ?3, ?4)";

    // -- who is due ------------------------------------------------------------

    /// Of the people in the JSON list `?1`, the ones due a taste build:
    /// none yet, one built at `?2` or before, one built at `?8` or before
    /// with a play since (up to now, `?9`), one older than an imported
    /// history, or one that missed facts which have all arrived since
    /// (lists of kind `?3` from `?4`, kept as `?7`, and their neighbours'
    /// counts of kind `?5` from `?6`). The seed reads go through the seeds'
    /// own index.
    pub(in crate::discover) TASTE_DUE = "SELECT u.value FROM json_each(?1) u \
         JOIN auth_users a ON a.id = u.value \
         LEFT JOIN catalog_taste_builds b ON b.user_id = u.value \
         WHERE b.user_id IS NULL OR b.built_at <= ?2 \
          OR (b.built_at <= ?8 AND EXISTS (SELECT 1 FROM library_play_history h \
            WHERE h.user_id = u.value AND h.played_at > b.built_at AND h.played_at <= ?9)) \
          OR EXISTS (SELECT 1 FROM media_history_imports i \
            WHERE i.user_id = u.value AND i.last_synced_at > b.built_at) \
          OR (b.incomplete = 1 \
           AND NOT EXISTS (SELECT 1 FROM catalog_taste_artists s \
             WHERE s.user_id = b.user_id AND s.is_seed = 1 \
               AND NOT EXISTS (SELECT 1 FROM catalog_fact_fetches f \
                 WHERE f.artist_mbid = s.artist_mbid_lower AND f.kind = ?3 \
                   AND f.source = ?4 AND f.fetched_at IS NOT NULL)) \
           AND NOT EXISTS (SELECT 1 FROM catalog_taste_artists s \
             JOIN catalog_artist_neighbours n ON n.artist_mbid = s.artist_mbid_lower \
               AND n.source = ?7 \
             WHERE s.user_id = b.user_id AND s.is_seed = 1 \
               AND NOT EXISTS (SELECT 1 FROM catalog_fact_fetches f \
                 WHERE f.artist_mbid = n.neighbour_mbid AND f.kind = ?5 \
                   AND f.source = ?6 AND f.fetched_at IS NOT NULL)) \
           AND EXISTS (SELECT 1 FROM catalog_taste_artists s \
             JOIN catalog_fact_fetches f ON f.artist_mbid = s.artist_mbid_lower \
             WHERE s.user_id = b.user_id AND s.is_seed = 1 AND f.kind = ?3 \
               AND f.source = ?4 AND f.fetched_at > b.built_at \
             UNION ALL SELECT 1 FROM catalog_taste_artists s \
             JOIN catalog_artist_neighbours n ON n.artist_mbid = s.artist_mbid_lower \
               AND n.source = ?7 \
             JOIN catalog_fact_fetches f ON f.artist_mbid = n.neighbour_mbid \
             WHERE s.user_id = b.user_id AND s.is_seed = 1 AND f.kind = ?5 \
               AND f.source = ?6 AND f.fetched_at > b.built_at))";
}
