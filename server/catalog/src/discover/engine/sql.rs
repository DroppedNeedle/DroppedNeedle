//! The engine's statements: a person's history as their taste is worked
//! out from it, their taste build and who is due one, and their candidate
//! pool with what it is built from.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_library::owns_group;

use super::candidate::{CandidateKind, PathKind, PathSource};
use crate::discover::tunings::TuningKind;
use crate::local::sql::{OPEN_REQUEST, musicbrainz, owned_album, owned_artist};

droppedneedle_kernel::sql! {
    // -- history -------------------------------------------------------------

    /// A person's plays in the app and imported listens per artist, by
    /// MusicBrainz id: how many of each, in how many calendar months, the
    /// first and the latest, and how many since `?2`. A play filed by name
    /// alone, a placeholder artist row (by kind) and a listen with no artist
    /// id are left out. A listen within `?3` ms of one of the person's
    /// counted plays of the same artist and the same recording or title is
    /// that play, scrobbled: the play is counted and the listen is not. The
    /// title is enough because ListenBrainz may map a scrobble to another
    /// recording of the same song.
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
           WHERE l.user_id = ?1 AND l.artist_mbid IS NOT NULL AND l.artist_mbid <> '' \
             AND NOT EXISTS (SELECT 1 FROM library_play_history p \
               JOIN local_artists pr ON pr.id = p.local_artist_id \
                 AND pr.kind NOT IN ('various_artists', 'unknown') \
               JOIN local_artist_external_identities pi ON pi.local_artist_id = pr.id AND "
        + (musicbrainz!("pi"))
        + " WHERE p.user_id = ?1 \
                 AND p.played_at BETWEEN l.listened_at - ?3 AND l.listened_at + ?3 \
                 AND lower(pi.provider_artist_id) = lower(l.artist_mbid) \
                 AND ((p.recording_mbid <> '' \
                     AND lower(p.recording_mbid) = lower(l.recording_mbid)) \
                   OR lower(p.track_name) = lower(l.track_name)))) \
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

    // -- the candidate pool ------------------------------------------------------

    /// Whether a person has a taste build.
    pub(in crate::discover) POOL_HAS_TASTE = "SELECT EXISTS (SELECT 1 \
         FROM catalog_taste_builds WHERE user_id = ?1)";
    /// A person's seeds, heaviest first, their plays in the app and imported
    /// listens together.
    pub(in crate::discover) POOL_SEEDS = "SELECT artist_mbid_lower AS mbid, name, \
         plays + imported_listens AS plays, weight, group_no FROM catalog_taste_artists \
         WHERE user_id = ?1 AND is_seed = 1 ORDER BY weight DESC, artist_mbid_lower";
    pub(in crate::discover) row PoolSeed for [POOL_SEEDS] {
        mbid: String,
        name: String,
        plays: i64,
        weight: f64,
        group_no: Option<i64>,
    }
    /// A person's taste groups and their shares.
    pub(in crate::discover) POOL_GROUPS = "SELECT group_no, share FROM catalog_taste_groups \
         WHERE user_id = ?1 ORDER BY group_no";
    pub(in crate::discover) row PoolGroup for [POOL_GROUPS] {
        group_no: i64,
        share: f64,
    }
    /// Every artist a person's rows name: the artists they know.
    pub(in crate::discover) POOL_KNOWN_ARTISTS = "SELECT artist_mbid_lower \
         FROM catalog_taste_artists WHERE user_id = ?1";
    /// Every artist the library holds an album of, by MusicBrainz id.
    pub(in crate::discover) POOL_OWNED_ARTISTS = "SELECT DISTINCT lower(ai.provider_artist_id) \
         FROM local_artist_external_identities ai \
         JOIN local_artists r ON r.id = ai.local_artist_id WHERE "
        + (musicbrainz!("ai"))
        + " AND "
        + (owned_artist!("r"));
    /// The release groups among the JSON list `?1` the library holds.
    pub(in crate::discover) POOL_OWNED_AMONG = "WITH asked(g) AS \
         (SELECT value FROM json_each(?1)) SELECT DISTINCT o.group_mbid FROM ("
        + (owns_group!("IN (SELECT g FROM asked)"))
        + ") o";
    /// Every album the library is fetching: an open album request.
    pub(in crate::discover) POOL_FETCHING = "SELECT musicbrainz_id_lower FROM request_history \
         WHERE " + OPEN_REQUEST;
    /// How many albums the library holds of each artist in the JSON list
    /// `?1`, one row an artist that has any.
    pub(in crate::discover) POOL_ALBUMS_OWNED = "SELECT lower(ai.provider_artist_id) AS mbid, \
         COUNT(DISTINCT ob.id) AS albums FROM local_artist_external_identities ai \
         JOIN local_artists r ON r.id = ai.local_artist_id \
         JOIN local_album_artists oc ON oc.local_artist_id = r.id \
         JOIN local_albums ob ON ob.id = oc.local_album_id \
         WHERE lower(ai.provider_artist_id) IN (SELECT value FROM json_each(?1)) AND "
        + (musicbrainz!("ai"))
        + " AND r.retired_into_artist_id IS NULL AND "
        + (owned_album!("ob"))
        + " GROUP BY lower(ai.provider_artist_id)";
    pub(in crate::discover) row PoolAlbumsOwned for [POOL_ALBUMS_OWNED] {
        mbid: String,
        albums: i64,
    }
    /// A person's picks shown at `?2` or later: whether each was sampled,
    /// and whether they asked for it, read from their requests (the pick
    /// record learns of a request only once a day).
    pub(in crate::discover) POOL_SHOWN = "SELECT p.release_group_mbid_lower AS release_group, \
         lower(p.artist_mbid) AS artist_mbid, p.last_shown_at AS last_shown_at, \
         p.sampled_at IS NOT NULL AS sampled, \
         EXISTS (SELECT 1 FROM request_history_requesters r WHERE r.user_id = p.user_id \
           AND r.musicbrainz_id_lower = p.release_group_mbid_lower) AS requested \
         FROM catalog_discover_picks p WHERE p.user_id = ?1 AND p.last_shown_at >= ?2";
    pub(in crate::discover) row PoolShown for [POOL_SHOWN] {
        release_group: String,
        artist_mbid: String,
        last_shown_at: Millis,
        sampled: bool,
        requested: bool,
    }
    /// The kept album rows of the release groups in the JSON list `?1`.
    pub(in crate::discover) POOL_GROUP_FACTS = "SELECT release_group_mbid, artist_mbid, \
         primary_type, secondary_types, first_release_date, credited_artist_mbids, listeners \
         FROM catalog_artist_albums \
         WHERE release_group_mbid IN (SELECT value FROM json_each(?1)) \
         ORDER BY release_group_mbid, artist_mbid";
    pub(in crate::discover) row PoolGroupFacts for [POOL_GROUP_FACTS] {
        release_group_mbid: String,
        artist_mbid: String,
        primary_type: Option<String>,
        secondary_types: String,
        first_release_date: Option<String>,
        credited_artist_mbids: Option<String>,
        listeners: Option<i64>,
    }
    /// A person's candidates, in their place in the pool.
    pub(in crate::discover) POOL_CANDIDATES = "SELECT release_group_mbid_lower, title, \
         artist_mbid, artist_name, credited_artist_mbids, primary_type, first_release_date, \
         album_listeners, artist_listeners, kind, support, group_no, group_support \
         FROM catalog_discover_candidates WHERE user_id = ?1 ORDER BY rank";
    /// One of a person's candidates, by release group (`?2`, lowercased).
    pub(in crate::discover) POOL_CANDIDATE = "SELECT release_group_mbid_lower, title, \
         artist_mbid, artist_name, credited_artist_mbids, primary_type, first_release_date, \
         album_listeners, artist_listeners, kind, support, group_no, group_support \
         FROM catalog_discover_candidates WHERE user_id = ?1 AND release_group_mbid_lower = ?2";
    pub(in crate::discover) row PoolCandidate for [POOL_CANDIDATES, POOL_CANDIDATE] {
        release_group_mbid_lower: String,
        title: String,
        artist_mbid: String,
        artist_name: String,
        credited_artist_mbids: String,
        primary_type: Option<String>,
        first_release_date: Option<String>,
        album_listeners: Option<i64>,
        artist_listeners: Option<i64>,
        kind: CandidateKind,
        support: f64,
        group_no: Option<i64>,
        group_support: f64,
    }
    /// A person's candidates' paths, each candidate's in order.
    pub(in crate::discover) POOL_PATHS = "SELECT release_group_mbid_lower, kind, source, \
         fetched_at, artist_mbid, artist_name, artist_plays, artist_weight, \
         artist_albums_owned, link_strength, link_rank, link_of, named_by_labs, \
         named_by_lastfm, link_closest_new, known_credit_mbid \
         FROM catalog_discover_candidate_paths \
         WHERE user_id = ?1 ORDER BY release_group_mbid_lower, path_no";
    /// One of a person's candidates' paths (`?2`, lowercased), in order.
    pub(in crate::discover) POOL_CANDIDATE_PATHS = "SELECT release_group_mbid_lower, kind, \
         source, fetched_at, artist_mbid, artist_name, artist_plays, artist_weight, \
         artist_albums_owned, link_strength, link_rank, link_of, named_by_labs, \
         named_by_lastfm, link_closest_new, known_credit_mbid \
         FROM catalog_discover_candidate_paths \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2 ORDER BY path_no";
    pub(in crate::discover) row PoolPath for [POOL_PATHS, POOL_CANDIDATE_PATHS] {
        release_group_mbid_lower: String,
        kind: PathKind,
        source: PathSource,
        fetched_at: Option<Millis>,
        artist_mbid: Option<String>,
        artist_name: Option<String>,
        artist_plays: Option<i64>,
        artist_weight: Option<f64>,
        artist_albums_owned: Option<i64>,
        link_strength: Option<f64>,
        link_rank: Option<i64>,
        link_of: Option<i64>,
        named_by_labs: bool,
        named_by_lastfm: bool,
        link_closest_new: bool,
        known_credit_mbid: Option<String>,
    }
    /// A person's pool build row.
    pub(in crate::discover) POOL_BUILD_DELETE = "DELETE FROM catalog_discover_pool_builds \
         WHERE user_id = ?1";
    /// A person's candidates' paths.
    pub(in crate::discover) POOL_PATHS_DELETE = "DELETE FROM catalog_discover_candidate_paths \
         WHERE user_id = ?1";
    /// A person's candidates.
    pub(in crate::discover) POOL_CANDIDATES_DELETE = "DELETE FROM catalog_discover_candidates \
         WHERE user_id = ?1";
    /// Keep a person's pool build row.
    pub(in crate::discover) POOL_BUILD_INSERT = "INSERT INTO catalog_discover_pool_builds \
         (user_id, built_at, incomplete, waiting_for, seeds_used, neighbours_found, \
          known_left_out, hubs_left_out, artists_ranked, artists_without_album, \
          new_artist_candidates, source_pick_candidates, gap_candidates, shelf_candidates, \
          fresh_candidates, thread_candidates) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)";
    /// Keep one of a person's candidates.
    pub(in crate::discover) POOL_CANDIDATE_INSERT = "INSERT INTO catalog_discover_candidates \
         (user_id, release_group_mbid_lower, rank, kind, title, artist_mbid, artist_name, \
          credited_artist_mbids, primary_type, first_release_date, album_listeners, \
          artist_listeners, support, group_no, group_support) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";
    /// Keep one path of a person's candidate.
    pub(in crate::discover) POOL_PATH_INSERT = "INSERT INTO catalog_discover_candidate_paths \
         (user_id, release_group_mbid_lower, path_no, kind, source, fetched_at, artist_mbid, \
          artist_name, artist_plays, artist_weight, artist_albums_owned, link_strength, \
          link_rank, link_of, named_by_labs, named_by_lastfm, known_credit_mbid, \
          link_closest_new) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
          ?18)";

    /// A person's seeds, by MusicBrainz id, lowercased.
    pub(in crate::discover) POOL_SEED_IDS = "SELECT artist_mbid_lower \
         FROM catalog_taste_artists WHERE user_id = ?1 AND is_seed = 1";

    // -- a pick's evidence ---------------------------------------------------------

    /// Whether a person has no plays or imported listens of the artist `?2`
    /// (lowercased) and has not played the release group `?3` (lowercased),
    /// as their taste rows hold them.
    pub(in crate::discover) EVIDENCE_NO_PLAYS = "SELECT NOT EXISTS (SELECT 1 \
         FROM catalog_taste_artists WHERE user_id = ?1 AND artist_mbid_lower = ?2 \
           AND plays + imported_listens > 0) \
         AND NOT EXISTS (SELECT 1 FROM catalog_known_release_groups \
           WHERE user_id = ?1 AND release_group_mbid_lower = ?3)";

    // -- how a person tuned Discover -------------------------------------------------

    /// The name a person's taste rows give the artist `?2` (lowercased);
    /// no row when it is not one of their artists.
    pub(in crate::discover) TUNING_TARGET_NAME = "SELECT name FROM catalog_taste_artists \
         WHERE user_id = ?1 AND artist_mbid_lower = ?2";
    /// Keep one tuning of a person's; the same one again takes the new name
    /// and time.
    pub(in crate::discover) TUNING_SAVE = "INSERT INTO catalog_discover_tunings \
         (user_id, kind, target, target_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id, kind, target) DO UPDATE SET \
           target_name = excluded.target_name, created_at = excluded.created_at";
    /// Take back one tuning of a person's.
    pub(in crate::discover) TUNING_UNDO = "DELETE FROM catalog_discover_tunings \
         WHERE user_id = ?1 AND kind = ?2 AND target = ?3";
    /// A person's tunings, newest first.
    pub(in crate::discover) TUNING_LIST = "SELECT kind, target, target_name, created_at \
         FROM catalog_discover_tunings WHERE user_id = ?1 \
         ORDER BY created_at DESC, kind, target";
    pub(in crate::discover) row TuningRow for [TUNING_LIST] {
        kind: TuningKind,
        target: String,
        target_name: String,
        created_at: Millis,
    }
    /// The targets of a person's tunings of kind `?2`.
    pub(in crate::discover) TUNING_TARGETS = "SELECT target FROM catalog_discover_tunings \
         WHERE user_id = ?1 AND kind = ?2";

    // -- who is due a pool build -------------------------------------------------

    /// Of the people in the JSON list `?1`, the ones whose pool was built
    /// waiting for album facts of which at least one has arrived since: an
    /// artist's album list (kind `?2` from `?3`) or its albums' counts (kind
    /// `?4` from `?5`).
    pub(in crate::discover) POOL_DUE = "SELECT u.value FROM json_each(?1) u \
         JOIN catalog_discover_pool_builds b ON b.user_id = u.value \
         WHERE b.incomplete = 1 AND EXISTS (SELECT 1 FROM json_each(b.waiting_for) w \
           JOIN catalog_fact_fetches f ON f.artist_mbid = w.value \
           WHERE ((f.kind = ?2 AND f.source = ?3) OR (f.kind = ?4 AND f.source = ?5)) \
             AND f.fetched_at > b.built_at)";
}
