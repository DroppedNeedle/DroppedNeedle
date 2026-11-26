//! The engine's statements: a person's history as their taste is worked
//! out from it, their taste build and who is due one, and their candidate
//! pool with what it is built from.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_library::owns_group;
use droppedneedle_media::history::HistoryImportState;

use super::candidate::{CandidateKind, PathKind, PathSource};
use super::taste::Trend;
use super::weekly::{SetDay, ShortReason};
use crate::discover::facts::FactSource;
use crate::discover::models::PickState;
use crate::discover::tunings::TuningKind;
use crate::local::sql::{OPEN_REQUEST, musicbrainz, owned_album};

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
    /// Every live library artist with a MusicBrainz id, whether or not the
    /// library holds an album of theirs (a guest on one song counts), by
    /// that id. Placeholder rows are left out by kind.
    pub(in crate::discover) POOL_LIBRARY_ARTISTS = "SELECT DISTINCT \
         lower(ai.provider_artist_id) FROM local_artist_external_identities ai \
         JOIN local_artists r ON r.id = ai.local_artist_id WHERE "
        + (musicbrainz!("ai"))
        + " AND r.retired_into_artist_id IS NULL \
         AND r.kind NOT IN ('various_artists', 'unknown')";
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
    /// The anchor of a person's taste group `?2`, by id (lowercased) and
    /// name; no row when they have none by that number.
    pub(in crate::discover) TUNING_GROUP_ANCHOR = "SELECT lower(anchor_artist_mbid) AS mbid, \
         anchor_name AS name FROM catalog_taste_groups WHERE user_id = ?1 AND group_no = ?2";
    pub(in crate::discover) row TuningGroupAnchor for [TUNING_GROUP_ANCHOR] {
        mbid: String,
        name: String,
    }
    /// The artists in a person's taste group `?2`, by id.
    pub(in crate::discover) TUNING_GROUP_ARTISTS = "SELECT artist_mbid_lower, name \
         FROM catalog_taste_artists WHERE user_id = ?1 AND group_no = ?2 \
         ORDER BY artist_mbid_lower";
    pub(in crate::discover) row TuningGroupArtist for [TUNING_GROUP_ARTISTS] {
        artist_mbid_lower: String,
        name: String,
    }
    /// What a person said about the target `?2`, of any kind.
    pub(in crate::discover) TUNING_OF_TARGET = "SELECT kind, group_anchor_mbid \
         FROM catalog_discover_tunings WHERE user_id = ?1 AND target = ?2";
    pub(in crate::discover) row TuningOfTarget for [TUNING_OF_TARGET] {
        kind: TuningKind,
        group_anchor_mbid: Option<String>,
    }
    /// Keep one tuning of a person's, about a whole group when `?6` names
    /// its anchor (and `?7` its name); the same one again takes the new
    /// name, group and time.
    pub(in crate::discover) TUNING_SAVE = "INSERT INTO catalog_discover_tunings \
         (user_id, kind, target, target_name, created_at, group_anchor_mbid, group_label) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (user_id, kind, target) DO UPDATE SET \
           target_name = excluded.target_name, created_at = excluded.created_at, \
           group_anchor_mbid = excluded.group_anchor_mbid, group_label = excluded.group_label";
    /// Take back one tuning of a person's.
    pub(in crate::discover) TUNING_UNDO = "DELETE FROM catalog_discover_tunings \
         WHERE user_id = ?1 AND kind = ?2 AND target = ?3";
    /// The anchor of the group a person's tuning was said for; null for
    /// one artist.
    pub(in crate::discover) TUNING_ANCHOR = "SELECT group_anchor_mbid \
         FROM catalog_discover_tunings WHERE user_id = ?1 AND kind = ?2 AND target = ?3";
    /// Take back one statement a person made about a whole group: every row
    /// of kind `?2` said for the group anchored on `?3`.
    pub(in crate::discover) TUNING_UNDO_GROUP = "DELETE FROM catalog_discover_tunings \
         WHERE user_id = ?1 AND kind = ?2 AND group_anchor_mbid = ?3";
    /// A person's tunings, newest first.
    pub(in crate::discover) TUNING_LIST = "SELECT kind, target, target_name, \
         group_anchor_mbid, group_label, created_at FROM catalog_discover_tunings \
         WHERE user_id = ?1 ORDER BY created_at DESC, kind, target";
    pub(in crate::discover) row TuningRow for [TUNING_LIST] {
        kind: TuningKind,
        target: String,
        target_name: String,
        group_anchor_mbid: Option<String>,
        group_label: Option<String>,
        created_at: Millis,
    }
    /// The targets of a person's tunings of kind `?2`.
    pub(in crate::discover) TUNING_TARGETS = "SELECT target FROM catalog_discover_tunings \
         WHERE user_id = ?1 AND kind = ?2";

    // -- a person's taste, as their page shows it -------------------------------------

    /// A person's taste build: what it counted, from when, and whether a
    /// tuning they made is newer than their pool build, which every tuning
    /// is followed by (a phase through the taste build first). Whether a
    /// build is queued or running is read from the jobs beside it.
    pub(in crate::discover) TASTE_PAGE_BUILD = "SELECT b.plays_counted AS plays_counted, \
         b.imported_listens_counted AS imported_listens_counted, \
         b.history_from_at AS history_from_at, \
         EXISTS (SELECT 1 FROM catalog_discover_tunings t \
           WHERE t.user_id = b.user_id AND t.created_at > coalesce((SELECT p.built_at \
             FROM catalog_discover_pool_builds p WHERE p.user_id = b.user_id), 0)) AS updating \
         FROM catalog_taste_builds b WHERE b.user_id = ?1";
    pub(in crate::discover) row TastePageBuild for [TASTE_PAGE_BUILD] {
        plays_counted: i64,
        imported_listens_counted: i64,
        history_from_at: Option<Millis>,
        updating: bool,
    }
    /// A person's taste groups, largest share first, each with the latest
    /// play of any artist in it.
    pub(in crate::discover) TASTE_PAGE_GROUPS = "SELECT g.group_no AS group_no, \
         lower(g.anchor_artist_mbid) AS anchor_mbid, g.anchor_name AS anchor_name, \
         g.share AS share, g.trend AS trend, \
         (SELECT MAX(a.last_played_at) FROM catalog_taste_artists a \
           WHERE a.user_id = g.user_id AND a.group_no = g.group_no) AS last_played_at \
         FROM catalog_taste_groups g WHERE g.user_id = ?1 ORDER BY g.group_no";
    pub(in crate::discover) row TastePageGroup for [TASTE_PAGE_GROUPS] {
        group_no: i64,
        anchor_mbid: String,
        anchor_name: String,
        share: f64,
        trend: Trend,
        last_played_at: Option<Millis>,
    }
    /// The artists in a person's taste groups, most played first.
    pub(in crate::discover) TASTE_PAGE_ARTISTS = "SELECT group_no, artist_mbid_lower, name, \
         plays, imported_listens, is_seed FROM catalog_taste_artists \
         WHERE user_id = ?1 AND group_no IS NOT NULL \
         ORDER BY group_no, plays + imported_listens DESC, weight DESC, artist_mbid_lower";
    pub(in crate::discover) row TastePageArtist for [TASTE_PAGE_ARTISTS] {
        group_no: i64,
        artist_mbid_lower: String,
        name: String,
        plays: i64,
        imported_listens: i64,
        is_seed: bool,
    }
    /// A person's tunings about one artist each, with the group each was
    /// said for.
    pub(in crate::discover) TASTE_PAGE_TUNINGS = "SELECT target, kind, group_anchor_mbid, \
         group_label FROM catalog_discover_tunings WHERE user_id = ?1 \
         ORDER BY target, kind";
    pub(in crate::discover) row TastePageTuning for [TASTE_PAGE_TUNINGS] {
        target: String,
        kind: TuningKind,
        group_anchor_mbid: Option<String>,
        group_label: Option<String>,
    }

    // -- who is due a pool build -------------------------------------------------

    /// Of the people in the JSON list `?1` with a taste build, the ones due
    /// a pool build: none yet, one built before their latest taste build,
    /// or one built waiting for album facts of which at least one has
    /// arrived since: an artist's album list (kind `?2` from `?3`) or its
    /// albums' counts (kind `?4` from `?5`).
    pub(in crate::discover) POOL_DUE = "SELECT u.value FROM json_each(?1) u \
         JOIN catalog_taste_builds t ON t.user_id = u.value \
         LEFT JOIN catalog_discover_pool_builds b ON b.user_id = u.value \
         WHERE b.user_id IS NULL OR b.built_at < t.built_at \
          OR (b.incomplete = 1 AND EXISTS (SELECT 1 FROM json_each(b.waiting_for) w \
           JOIN catalog_fact_fetches f ON f.artist_mbid = w.value \
           WHERE ((f.kind = ?2 AND f.source = ?3) OR (f.kind = ?4 AND f.source = ?5)) \
             AND f.fetched_at > b.built_at))";

    // -- gaps and the person's own shelves ----------------------------------------

    /// Of the artists person `?1` played or signalled, by MusicBrainz id
    /// and heaviest first, the ones the library holds `?2` or more albums
    /// of, with how many. Placeholder rows are left out by kind.
    pub(in crate::discover) GAP_ARTISTS = "SELECT t.artist_mbid_lower AS mbid, \
         MIN(t.name) AS name, MIN(t.weight) AS weight, \
         MIN(t.plays + t.imported_listens) AS plays, MIN(t.group_no) AS group_no, \
         COUNT(DISTINCT ob.id) AS albums FROM catalog_taste_artists t \
         JOIN local_artist_external_identities ai \
           ON lower(ai.provider_artist_id) = t.artist_mbid_lower AND "
        + (musicbrainz!("ai"))
        + " JOIN local_artists r ON r.id = ai.local_artist_id \
         JOIN local_album_artists oc ON oc.local_artist_id = r.id \
         JOIN local_albums ob ON ob.id = oc.local_album_id \
         WHERE t.user_id = ?1 AND t.weight > 0 AND r.retired_into_artist_id IS NULL \
           AND r.kind NOT IN ('various_artists', 'unknown') AND "
        + (owned_album!("ob"))
        + " GROUP BY t.artist_mbid_lower HAVING COUNT(DISTINCT ob.id) >= ?2 \
         ORDER BY MIN(t.weight) DESC, t.artist_mbid_lower";
    pub(in crate::discover) row GapArtistRow for [GAP_ARTISTS] {
        mbid: String,
        name: String,
        weight: f64,
        plays: i64,
        group_no: Option<i64>,
        albums: i64,
    }
    /// The albums the library holds of each artist in the JSON list `?1`
    /// (lowercased), the ones `POOL_ALBUMS_OWNED` counts, each artist's
    /// oldest first, with the album's release group when it has one.
    pub(in crate::discover) GAP_HELD = "SELECT lower(ai.provider_artist_id) AS artist_mbid, \
         ob.id AS local_id, MIN(ob.title) AS title, MIN(ob.year) AS year, \
         (SELECT MIN(lower(ae.release_group_mbid)) FROM local_album_external_identities ae \
           WHERE ae.local_album_id = ob.id AND "
        + (musicbrainz!("ae"))
        + ") AS release_group_mbid FROM local_artist_external_identities ai \
         JOIN local_artists r ON r.id = ai.local_artist_id \
         JOIN local_album_artists oc ON oc.local_artist_id = r.id \
         JOIN local_albums ob ON ob.id = oc.local_album_id \
         WHERE lower(ai.provider_artist_id) IN (SELECT value FROM json_each(?1)) AND "
        + (musicbrainz!("ai"))
        + " AND r.retired_into_artist_id IS NULL AND "
        + (owned_album!("ob"))
        + " GROUP BY lower(ai.provider_artist_id), ob.id \
         ORDER BY artist_mbid, MIN(ob.year) IS NULL, MIN(ob.year), \
           MIN(ob.title) COLLATE NOCASE, ob.id";
    pub(in crate::discover) row GapHeldRow for [GAP_HELD] {
        artist_mbid: String,
        local_id: String,
        title: String,
        year: Option<i64>,
        release_group_mbid: Option<String>,
    }
    /// The albums person `?1` played `?2` times or more, last at `?3` or
    /// before, that are live and have files: most played first, at most
    /// `?4`, each with their first play of one of its tracks and of the
    /// album alone.
    pub(in crate::discover) SHELF_PLAYED = "SELECT ob.id AS local_id, ob.title AS title, \
         ob.album_artist_name AS artist_name, \
         (SELECT MIN(lower(aie.provider_artist_id)) FROM local_artist_external_identities aie \
           WHERE aie.local_artist_id = ob.album_artist_id AND "
        + (musicbrainz!("aie"))
        + ") AS artist_mbid, \
         (SELECT MIN(lower(ae.release_group_mbid)) FROM local_album_external_identities ae \
           WHERE ae.local_album_id = ob.id AND "
        + (musicbrainz!("ae"))
        + ") AS release_group_mbid, ob.year AS year, p.plays AS plays, \
         p.last_played_at AS last_played_at, \
         (SELECT MIN(h.played_at) FROM local_tracks t JOIN library_play_history h \
           ON h.user_id = p.user_id AND h.local_track_id = t.id \
           WHERE t.local_album_id = p.local_album_id) AS first_track_at, \
         (SELECT MIN(h.played_at) FROM library_play_history h WHERE h.user_id = p.user_id \
           AND h.local_track_id IS NULL AND h.local_album_id = p.local_album_id) \
           AS first_album_at \
         FROM library_user_album_plays p JOIN local_albums ob ON ob.id = p.local_album_id \
         WHERE p.user_id = ?1 AND p.plays >= ?2 AND p.last_played_at <= ?3 AND "
        + (owned_album!("ob"))
        + " ORDER BY p.plays DESC, p.last_played_at DESC, ob.id LIMIT ?4";
    pub(in crate::discover) row ShelfPlayedRow for [SHELF_PLAYED] {
        local_id: String,
        title: String,
        artist_name: Option<String>,
        artist_mbid: Option<String>,
        release_group_mbid: Option<String>,
        year: Option<i64>,
        plays: i64,
        last_played_at: Millis,
        first_track_at: Option<Millis>,
        first_album_at: Option<Millis>,
    }
    /// The release groups person `?1` asked for that arrived at `?2` or
    /// before and the library owns, none of whose owning albums they have
    /// played: the oldest arrival first, at most `?3`, each with the first
    /// album that owns it and when they asked.
    pub(in crate::discover) SHELF_NEVER_PLAYED = "SELECT ob.id AS local_id, \
         ob.title AS title, ob.album_artist_name AS artist_name, \
         (SELECT MIN(lower(aie.provider_artist_id)) FROM local_artist_external_identities aie \
           WHERE aie.local_artist_id = ob.album_artist_id AND "
        + (musicbrainz!("aie"))
        + ") AS artist_mbid, a.group_mbid AS release_group_mbid, ob.year AS year, \
         a.asked_at AS asked_at FROM (\
           SELECT q.musicbrainz_id_lower AS group_mbid, q.requested_at AS asked_at, \
           h.completed_at AS arrived_at, \
           (SELECT o.album_id FROM ("
        + (owns_group!("= +q.musicbrainz_id_lower"))
        + ") o ORDER BY o.album_id LIMIT 1) AS local_id \
           FROM request_history_requesters q JOIN request_history h \
             ON h.musicbrainz_id_lower = q.musicbrainz_id_lower \
           WHERE q.user_id = ?1 AND h.request_kind = 'album' AND h.status = 'imported' \
             AND h.completed_at <= ?2) a \
         JOIN local_albums ob ON ob.id = a.local_id \
         WHERE NOT EXISTS (SELECT 1 FROM library_user_album_plays p WHERE p.user_id = ?1 \
           AND p.local_album_id IN (SELECT o.album_id FROM ("
        + (owns_group!("= +a.group_mbid"))
        + ") o)) \
         ORDER BY a.arrived_at, a.group_mbid LIMIT ?3";
    pub(in crate::discover) row ShelfNeverPlayedRow for [SHELF_NEVER_PLAYED] {
        local_id: String,
        title: String,
        artist_name: Option<String>,
        artist_mbid: Option<String>,
        release_group_mbid: String,
        year: Option<i64>,
        asked_at: Millis,
    }
    /// When person `?1`'s history begins: the first play or imported listen
    /// their taste build counted, or their first play in the app, whichever
    /// is earlier; NULL with neither.
    pub(in crate::discover) SHELF_HISTORY_FROM = "SELECT MIN(at) FROM (\
         SELECT history_from_at AS at FROM catalog_taste_builds WHERE user_id = ?1 \
         UNION ALL SELECT MIN(played_at) FROM library_play_history WHERE user_id = ?1)";

    // -- the weekly set ----------------------------------------------------------

    /// What a person's weekly set is built from and when: their set day and
    /// when their first set began waiting for their history, their newest
    /// set's week, their taste build's counts, their pool build's, and
    /// their history import's state. No row for no account.
    pub(in crate::discover) WEEK_CONTEXT = "SELECT p.set_day AS set_day, \
         p.history_wait_from_at AS wait_from_at, \
         (SELECT MAX(s.week_start_at) FROM catalog_weekly_sets s WHERE s.user_id = a.id) \
           AS last_week_at, \
         t.built_at AS taste_built_at, \
         t.plays_counted + t.imported_listens_counted AS plays_counted, \
         t.history_from_at AS history_from_at, t.history_to_at AS history_to_at, \
         t.seeds AS seeds, b.built_at AS pool_built_at, b.incomplete AS pool_incomplete, \
         b.waiting_for AS waiting_for, b.neighbours_found AS neighbours_found, \
         b.known_left_out AS known_left_out, b.hubs_left_out AS hubs_left_out, \
         i.state AS import_state \
         FROM auth_users a LEFT JOIN catalog_discover_prefs p ON p.user_id = a.id \
         LEFT JOIN catalog_taste_builds t ON t.user_id = a.id \
         LEFT JOIN catalog_discover_pool_builds b ON b.user_id = a.id \
         LEFT JOIN media_history_imports i ON i.user_id = a.id \
         WHERE a.id = ?1";
    pub(in crate::discover) row WeekContext for [WEEK_CONTEXT] {
        set_day: Option<SetDay>,
        wait_from_at: Option<Millis>,
        last_week_at: Option<Millis>,
        taste_built_at: Option<Millis>,
        plays_counted: Option<i64>,
        history_from_at: Option<Millis>,
        history_to_at: Option<Millis>,
        seeds: Option<i64>,
        pool_built_at: Option<Millis>,
        pool_incomplete: Option<bool>,
        waiting_for: Option<String>,
        neighbours_found: Option<i64>,
        known_left_out: Option<i64>,
        hubs_left_out: Option<i64>,
        import_state: Option<HistoryImportState>,
    }
    /// Of the people in the JSON list `?1` with an account, each one's set
    /// day, newest set's week, taste and pool build times and first set's
    /// wait for their history: who may be due a set.
    pub(in crate::discover) WEEK_DUE = "SELECT u.value AS user_id, p.set_day AS set_day, \
         (SELECT MAX(s.week_start_at) FROM catalog_weekly_sets s WHERE s.user_id = u.value) \
           AS last_week_at, \
         t.built_at AS taste_built_at, b.built_at AS pool_built_at, \
         p.history_wait_from_at AS wait_from_at \
         FROM json_each(?1) u JOIN auth_users a ON a.id = u.value \
         LEFT JOIN catalog_taste_builds t ON t.user_id = u.value \
         LEFT JOIN catalog_discover_prefs p ON p.user_id = u.value \
         LEFT JOIN catalog_discover_pool_builds b ON b.user_id = u.value";
    pub(in crate::discover) row WeekDue for [WEEK_DUE] {
        user_id: String,
        set_day: Option<SetDay>,
        last_week_at: Option<Millis>,
        taste_built_at: Option<Millis>,
        pool_built_at: Option<Millis>,
        wait_from_at: Option<Millis>,
    }
    /// Person `?1`'s first set began waiting for their history at `?2`:
    /// kept only while they have no set and their import is in one of the
    /// states `?3`, `?4` and `?5`, and only the first time.
    pub(in crate::discover) WEEK_HISTORY_WAIT = "INSERT INTO catalog_discover_prefs \
         (user_id, updated_at, history_wait_from_at) SELECT ?1, ?2, ?2 \
         WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
           AND NOT EXISTS (SELECT 1 FROM catalog_weekly_sets WHERE user_id = ?1) \
           AND EXISTS (SELECT 1 FROM media_history_imports WHERE user_id = ?1 \
             AND state IN (?3, ?4, ?5)) \
         ON CONFLICT (user_id) DO UPDATE SET \
           history_wait_from_at = excluded.history_wait_from_at \
         WHERE catalog_discover_prefs.history_wait_from_at IS NULL";
    /// A person's set day; no row while they have not chosen one.
    pub(in crate::discover) PREFS_SET_DAY = "SELECT set_day FROM catalog_discover_prefs \
         WHERE user_id = ?1";
    /// Keep person `?1`'s set day `?2`, chosen at `?3`.
    pub(in crate::discover) PREFS_SAVE = "INSERT INTO catalog_discover_prefs \
         (user_id, set_day, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (user_id) DO UPDATE SET set_day = excluded.set_day, \
           updated_at = excluded.updated_at";
    /// A person's taste groups with their anchors and shares.
    pub(in crate::discover) SET_TASTE_GROUPS = "SELECT group_no, anchor_artist_mbid, \
         anchor_name, share FROM catalog_taste_groups WHERE user_id = ?1 ORDER BY group_no";
    pub(in crate::discover) row SetTasteGroup for [SET_TASTE_GROUPS] {
        group_no: i64,
        anchor_artist_mbid: String,
        anchor_name: String,
        share: f64,
    }
    /// The library's shelves: how many albums it holds, and how many
    /// artists are credited on them, placeholder rows left out by kind.
    pub(in crate::discover) SET_SHELVES = "SELECT \
         (SELECT COUNT(*) FROM local_albums ob WHERE "
        + (owned_album!("ob"))
        + ") AS albums, (SELECT COUNT(DISTINCT oc.local_artist_id) FROM local_album_artists oc \
           JOIN local_albums ob ON ob.id = oc.local_album_id \
           JOIN local_artists r ON r.id = oc.local_artist_id \
           WHERE r.kind NOT IN ('various_artists', 'unknown') \
             AND r.retired_into_artist_id IS NULL AND "
        + (owned_album!("ob"))
        + ") AS artists";
    pub(in crate::discover) row SetShelves for [SET_SHELVES] {
        albums: i64,
        artists: i64,
    }
    /// The picks of a person's sets built at `?2` or later: each album and
    /// artist, when its set was built, whether it was sampled and whether
    /// the person asked for it.
    pub(in crate::discover) SET_EARLIER_PICKS = "SELECT k.release_group_mbid_lower \
         AS release_group, lower(k.artist_mbid) AS artist_mbid, s.built_at AS built_at, \
         EXISTS (SELECT 1 FROM catalog_discover_picks d WHERE d.user_id = k.user_id \
           AND d.release_group_mbid_lower = k.release_group_mbid_lower \
           AND d.sampled_at IS NOT NULL) AS sampled, \
         EXISTS (SELECT 1 FROM request_history_requesters r WHERE r.user_id = k.user_id \
           AND r.musicbrainz_id_lower = k.release_group_mbid_lower) AS requested \
         FROM catalog_weekly_set_picks k JOIN catalog_weekly_sets s \
           ON s.user_id = k.user_id AND s.week_start_at = k.week_start_at \
         WHERE k.user_id = ?1 AND s.built_at >= ?2";
    pub(in crate::discover) row SetEarlierPick for [SET_EARLIER_PICKS] {
        release_group: String,
        artist_mbid: String,
        built_at: Millis,
        sampled: bool,
        requested: bool,
    }
    /// The first failure, and its service, among the fetch rows of the
    /// artists in the JSON list `?1` that answer `?2` (failed).
    pub(in crate::discover) SET_FAILING = "SELECT source, MIN(failing_since_at) AS since \
         FROM catalog_fact_fetches WHERE outcome = ?2 AND failing_since_at IS NOT NULL \
           AND artist_mbid IN (SELECT value FROM json_each(?1)) \
         GROUP BY source ORDER BY since, source LIMIT 1";
    pub(in crate::discover) row SetFailing for [SET_FAILING] {
        source: FactSource,
        since: Millis,
    }
    /// Keep a person's set for a week, unless that week has one.
    pub(in crate::discover) SET_INSERT = "INSERT INTO catalog_weekly_sets (user_id, \
         week_start_at, built_at, short_reason, plays_counted, history_from_at, history_to_at, \
         seeds, neighbours_found, known_left_out, hubs_left_out, shelf_artists, shelf_albums, \
         linked, tastes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
         ?15) ON CONFLICT (user_id, week_start_at) DO NOTHING";
    /// Keep one pick of a person's set.
    pub(in crate::discover) SET_PICK_INSERT = "INSERT INTO catalog_weekly_set_picks (user_id, \
         week_start_at, pick_no, release_group_mbid_lower, release_group_mbid, title, \
         artist_mbid, artist_name, primary_type, first_release_date, taste_no, reason_line, \
         evidence) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
    /// A person's newest set.
    pub(in crate::discover) SET_LATEST = "SELECT week_start_at, built_at, short_reason, \
         plays_counted, history_from_at, history_to_at, seeds, neighbours_found, known_left_out, \
         hubs_left_out, shelf_artists, shelf_albums, linked, tastes FROM catalog_weekly_sets \
         WHERE user_id = ?1 ORDER BY week_start_at DESC LIMIT 1";
    pub(in crate::discover) row SetRow for [SET_LATEST] {
        week_start_at: Millis,
        built_at: Millis,
        short_reason: Option<ShortReason>,
        plays_counted: i64,
        history_from_at: Option<Millis>,
        history_to_at: Option<Millis>,
        seeds: i64,
        neighbours_found: Option<i64>,
        known_left_out: Option<i64>,
        hubs_left_out: Option<i64>,
        shelf_artists: i64,
        shelf_albums: i64,
        linked: bool,
        tastes: String,
    }
    /// The picks of a person's set for the week starting `?2`, in order,
    /// each with what became of it: arrived when the library holds it,
    /// else what their request for it says (statuses read as the pick
    /// states), else on their Later pile, else what they told Discover in
    /// effect at `?3` about the album (target `?4`) or its artist (target
    /// `?5`), else open; and whether they heard its sample.
    pub(in crate::discover) SET_PICKS = "SELECT k.pick_no AS pick_no, \
         k.release_group_mbid AS release_group_mbid, k.title AS title, \
         k.artist_mbid AS artist_mbid, k.artist_name AS artist_name, \
         k.primary_type AS primary_type, k.first_release_date AS first_release_date, \
         k.taste_no AS taste_no, k.reason_line AS reason_line, k.evidence AS evidence, "
        + PICK_STATE
        + " AS state, \
         EXISTS (SELECT 1 FROM catalog_discover_picks d WHERE d.user_id = k.user_id \
           AND d.release_group_mbid_lower = k.release_group_mbid_lower \
           AND d.sampled_at IS NOT NULL) AS heard \
         FROM catalog_weekly_set_picks k WHERE k.user_id = ?1 AND k.week_start_at = ?2 \
         ORDER BY k.pick_no";
    pub(in crate::discover) row SetPickRow for [SET_PICKS] {
        pick_no: i64,
        release_group_mbid: String,
        title: String,
        artist_mbid: String,
        artist_name: String,
        primary_type: Option<String>,
        first_release_date: Option<String>,
        taste_no: Option<i64>,
        reason_line: String,
        evidence: String,
        state: PickState,
        heard: bool,
    }
    /// The albums of a person's newest set and their artists, lowercased.
    pub(in crate::discover) SET_LATEST_PICKS = "SELECT release_group_mbid_lower, \
         lower(artist_mbid) AS artist_mbid FROM catalog_weekly_set_picks \
         WHERE user_id = ?1 AND week_start_at = \
           (SELECT MAX(week_start_at) FROM catalog_weekly_sets WHERE user_id = ?1)";
    pub(in crate::discover) row SetLatestPick for [SET_LATEST_PICKS] {
        release_group_mbid_lower: String,
        artist_mbid: String,
    }
    /// The evidence of the newest kept pick of person `?1` for the release
    /// group `?2` (lowercased), as it was built.
    pub(in crate::discover) SET_EVIDENCE = "SELECT evidence FROM catalog_weekly_set_picks \
         WHERE user_id = ?1 AND release_group_mbid_lower = ?2 \
         ORDER BY week_start_at DESC LIMIT 1";
    /// A person's set picks.
    pub(in crate::discover) SET_PICKS_DELETE = "DELETE FROM catalog_weekly_set_picks \
         WHERE user_id = ?1";
    /// A person's sets.
    pub(in crate::discover) SETS_DELETE = "DELETE FROM catalog_weekly_sets WHERE user_id = ?1";
    /// Clear when person `?1`'s first set began waiting for their history.
    pub(in crate::discover) PREFS_WAIT_CLEARED = "UPDATE catalog_discover_prefs \
         SET history_wait_from_at = NULL WHERE user_id = ?1 \
           AND history_wait_from_at IS NOT NULL";

    // -- past weeks ------------------------------------------------------------

    /// What became of the set pick `k` (a `catalog_weekly_set_picks` row):
    /// arrived when the library holds it, else what the person's request
    /// for it says (statuses read as the pick states), else on their Later
    /// pile, else what they told Discover in effect at `?3` about the album
    /// (target `?4`) or its artist (target `?5`), else open. The week and
    /// every past week read it alike.
    fragment PICK_STATE = "CASE WHEN EXISTS (SELECT 1 FROM ("
        + (owns_group!("= +k.release_group_mbid_lower"))
        + ")) THEN 'arrived' \
         ELSE COALESCE((SELECT CASE \
             WHEN r.status IN ('pending', 'queued', 'downloading') THEN 'asked' \
             WHEN r.status = 'awaiting_approval' THEN 'awaiting_approval' \
             WHEN r.status = 'rejected' THEN 'refused' \
             WHEN r.status IN ('failed', 'cancelling', 'cancelled', 'incomplete') \
               THEN 'failed' END \
           FROM request_history_requesters q JOIN request_history r \
             ON r.musicbrainz_id_lower = q.musicbrainz_id_lower \
           WHERE q.user_id = k.user_id AND q.musicbrainz_id_lower = k.release_group_mbid_lower \
             AND r.request_kind = 'album'), \
         CASE WHEN EXISTS (SELECT 1 FROM catalog_later_albums l WHERE l.user_id = k.user_id \
             AND l.release_group_mbid_lower = k.release_group_mbid_lower) THEN 'later' END, \
         (SELECT CASE f.kind WHEN 'no' THEN 'no' WHEN 'know_it' THEN 'known' \
             WHEN 'not_now' THEN 'not_now' END \
           FROM catalog_discover_feedback f WHERE f.user_id = k.user_id AND f.target = ?4 \
             AND f.target_mbid_lower = k.release_group_mbid_lower \
             AND (f.expires_at IS NULL OR f.expires_at > ?3)), \
         CASE WHEN EXISTS (SELECT 1 FROM catalog_discover_feedback f \
             WHERE f.user_id = k.user_id AND f.target = ?5 AND k.artist_mbid <> '' \
               AND f.target_mbid_lower = lower(k.artist_mbid) \
               AND (f.expires_at IS NULL OR f.expires_at > ?3)) THEN 'not_artist' END, \
         'open') END";
    /// Every pick of person `?1`'s sets whose week starts before `?2`, as `g`,
    /// each with its state (see `PICK_STATE`), worked out once.
    fragment PAST_PICKS = "WITH g AS MATERIALIZED (SELECT k.week_start_at AS week_start_at, \
         k.pick_no AS pick_no, k.release_group_mbid_lower AS release_group_mbid_lower, \
         k.release_group_mbid AS release_group_mbid, k.title AS title, \
         k.artist_mbid AS artist_mbid, k.artist_name AS artist_name, \
         k.primary_type AS primary_type, k.first_release_date AS first_release_date, \
         k.reason_line AS reason_line, k.evidence AS evidence, "
        + PICK_STATE
        + " AS state FROM catalog_weekly_set_picks k \
         WHERE k.user_id = ?1 AND k.week_start_at < ?2)";
    /// The newest `?6` of person `?1`'s sets before the week `?2` that hold a
    /// pick matching the filter, with every pick of each in order: its
    /// state, whether it matches (a state in the JSON list `?7` when there
    /// is one, and `?8`, lowercased, in its title or artist when there is
    /// one), when it arrived from the pick record, and how often the person
    /// played the albums that own its release group.
    pub(in crate::discover) PAST_WEEKS_PAGE = PAST_PICKS
        + ", m AS (SELECT g.*, (?7 IS NULL OR g.state IN (SELECT value FROM json_each(?7))) \
           AND (?8 IS NULL OR instr(lower(g.title), ?8) > 0 \
             OR instr(lower(g.artist_name), ?8) > 0) AS matches FROM g), \
         w AS (SELECT DISTINCT week_start_at FROM m WHERE matches \
           ORDER BY week_start_at DESC LIMIT ?6) \
         SELECT m.week_start_at AS week_start_at, m.pick_no AS pick_no, \
         m.release_group_mbid AS release_group_mbid, m.title AS title, \
         m.artist_mbid AS artist_mbid, m.artist_name AS artist_name, \
         m.primary_type AS primary_type, m.first_release_date AS first_release_date, \
         m.reason_line AS reason_line, m.evidence AS evidence, m.state AS state, \
         m.matches AS matches, \
         (SELECT d.arrived_at FROM catalog_discover_picks d WHERE d.user_id = ?1 \
           AND d.release_group_mbid_lower = m.release_group_mbid_lower) AS arrived_at, \
         (SELECT SUM(p.plays) FROM library_user_album_plays p WHERE p.user_id = ?1 \
           AND p.local_album_id IN (SELECT album_id FROM ("
        + (owns_group!("= +m.release_group_mbid_lower"))
        + "))) AS plays \
         FROM m JOIN w ON w.week_start_at = m.week_start_at \
         ORDER BY m.week_start_at DESC, m.pick_no";
    pub(in crate::discover) row PastWeekPickRow for [PAST_WEEKS_PAGE] {
        week_start_at: Millis,
        pick_no: i64,
        release_group_mbid: String,
        title: String,
        artist_mbid: String,
        artist_name: String,
        primary_type: Option<String>,
        first_release_date: Option<String>,
        reason_line: String,
        evidence: String,
        state: PickState,
        matches: bool,
        arrived_at: Option<Millis>,
        plays: Option<i64>,
    }
    /// How many of person `?1`'s set picks before the week `?2` are in each
    /// state, from the pick record the first arrival among them, how many
    /// were sampled, how many arrived at `?6` or before, and how many of
    /// those were played after their first 30 days.
    pub(in crate::discover) PAST_WEEKS_RECORD = PAST_PICKS
        + " SELECT g.state AS state, COUNT(*) AS picks, MIN(d.arrived_at) AS arrived_at, \
         COUNT(d.sampled_at) AS sampled, \
         COALESCE(SUM(d.arrived_at <= ?6), 0) AS month_on, \
         COALESCE(SUM(d.arrived_at <= ?6 AND d.plays_90d > d.plays_30d), 0) AS still_played \
         FROM g LEFT JOIN catalog_discover_picks d ON d.user_id = ?1 \
           AND d.release_group_mbid_lower = g.release_group_mbid_lower \
         GROUP BY g.state";
    pub(in crate::discover) row PastWeeksTally for [PAST_WEEKS_RECORD] {
        state: PickState,
        picks: i64,
        arrived_at: Option<Millis>,
        sampled: i64,
        month_on: i64,
        still_played: i64,
    }
    /// Person `?1`'s set picks before the week `?2` in a state of the JSON
    /// list `?6`, one row an album (lowercased) with the first week it was
    /// picked, newest first.
    pub(in crate::discover) PAST_WEEKS_FOUND = PAST_PICKS
        + " SELECT g.release_group_mbid_lower AS release_group_mbid, \
         MIN(g.week_start_at) AS week_start_at FROM g \
         WHERE g.state IN (SELECT value FROM json_each(?6)) \
         GROUP BY g.release_group_mbid_lower ORDER BY week_start_at DESC";
    pub(in crate::discover) row FoundPickRow for [PAST_WEEKS_FOUND] {
        release_group_mbid: String,
        week_start_at: Millis,
    }
}
