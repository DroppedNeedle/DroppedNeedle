//! The collections stores' statements. In each playlist read that names a
//! caller, `?1` is that user's id.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;

use droppedneedle_auth::listed_user;
use droppedneedle_library::{copy_of_group, owns_group};

droppedneedle_kernel::sql! {
    /// The caller may read the playlist: it is theirs, it is public, or it
    /// is shared with them.
    fragment READABLE = "(COALESCE(p.user_id = ?1, 0) OR p.is_public != 0 \
         OR EXISTS (SELECT 1 FROM catalog_playlist_members m \
         WHERE m.playlist_id = p.id AND m.user_id = ?1))";
    /// The playlist is the caller's.
    fragment OWNED = "p.user_id = ?1";
    /// The caller may change the playlist's songs: it is theirs, or it is
    /// shared with them to edit.
    fragment EDITABLE = "(COALESCE(p.user_id = ?1, 0) OR EXISTS (SELECT 1 \
         FROM catalog_playlist_members m WHERE m.playlist_id = p.id AND m.user_id = ?1 \
         AND m.can_edit = 1))";
    /// One playlist with its aggregates, in `PlaylistRow`'s mapper order.
    fragment PLAYLIST_COLUMNS = "SELECT p.id, p.name, p.user_id, \
         COALESCE(u.display_name, u.username), p.is_public, p.source_ref, \
         p.created_at / 1000, p.updated_at / 1000, \
         (c.playlist_id IS NOT NULL), \
         (SELECT COUNT(*) FROM catalog_playlist_tracks t WHERE t.playlist_id = p.id), \
         (SELECT SUM(CAST(t.duration_ms AS REAL)) / 1000.0 FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id), \
         (SELECT COUNT(*) FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL), \
         (SELECT COALESCE(SUM(CAST(t.duration_ms AS REAL)), 0.0) / 1000.0 FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL), \
         (SELECT m.can_edit FROM catalog_playlist_members m \
          WHERE m.playlist_id = p.id AND m.user_id = ?1), \
         EXISTS (SELECT 1 FROM catalog_playlist_members m WHERE m.playlist_id = p.id)";
    /// The tables a playlist row is read from.
    fragment PLAYLIST_FROM = " FROM catalog_playlists p \
         LEFT JOIN auth_users u ON u.id = p.user_id \
         LEFT JOIN catalog_playlist_covers c ON c.playlist_id = p.id";
    /// The list order.
    fragment BY_NAME = " ORDER BY p.name COLLATE NOCASE, p.id";
    /// One entry, in `PlaylistTrack`'s mapper order.
    fragment TRACK_COLUMNS = "SELECT id, position, track_name, artist_name, album_name, \
         album_id, artist_id, track_source_id, cover_url, source_type, available_sources, \
         format, track_number, disc_number, duration_ms / 1000.0, created_at / 1000, \
         plex_rating_key, library_file_id, recording_mbid FROM catalog_playlist_tracks";

    /// Every playlist, then whether the caller may read it. A private
    /// playlist with no owner is left out unless `?2`, an admin asking.
    pub(super) LIST_ALL = PLAYLIST_COLUMNS + ", " + READABLE + PLAYLIST_FROM
        + " WHERE (p.user_id IS NOT NULL OR p.is_public != 0 OR ?2)" + BY_NAME;
    /// The playlists the caller may read.
    pub(super) LIST_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE " + READABLE + BY_NAME;
    /// The first `?2` entry covers of each playlist the caller may read.
    pub(super) LIST_COVERS = "SELECT r.playlist_id, r.cover_url FROM (SELECT playlist_id, \
         cover_url, ROW_NUMBER() OVER (PARTITION BY playlist_id ORDER BY position) AS rank \
         FROM catalog_playlist_tracks WHERE cover_url IS NOT NULL AND cover_url != '') r \
         JOIN catalog_playlists p ON p.id = r.playlist_id \
         WHERE r.rank <= ?2 AND " + READABLE + " ORDER BY r.playlist_id, r.rank";
    /// One playlist `?2` the caller may read.
    pub(super) GET_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM
        + " WHERE p.id = ?2 AND " + READABLE;
    /// One playlist `?2` of the caller's.
    pub(super) GET_OWNED = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE p.id = ?2 AND " + OWNED;
    /// Whether the caller owns playlist `?2`, which they may read.
    pub(super) READABLE_OWNER = "SELECT COALESCE(p.user_id = ?1, 0) FROM catalog_playlists p \
         WHERE p.id = ?2 AND " + READABLE;
    /// Whether the caller may change the songs of playlist `?2`, which
    /// they may read.
    pub(super) READABLE_EDITOR = "SELECT " + EDITABLE + " FROM catalog_playlists p \
         WHERE p.id = ?2 AND " + READABLE;
    /// The first `?2` entry covers of one playlist `?1`.
    pub(super) PLAYLIST_COVERS = "SELECT cover_url FROM catalog_playlist_tracks \
         WHERE playlist_id = ?1 AND cover_url IS NOT NULL AND cover_url != '' \
         ORDER BY position LIMIT ?2";
    /// The entries of playlist `?2`, which the caller may read.
    pub(super) TRACKS_READABLE = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + READABLE + ") ORDER BY position";
    /// The entries of playlist `?2`, the caller's.
    pub(super) TRACKS_OWNED = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + OWNED + ") ORDER BY position";
    /// The uploaded cover of playlist `?2`, which the caller may read.
    pub(super) COVER_READABLE = "SELECT c.content_type, c.image FROM catalog_playlist_covers c \
         JOIN catalog_playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + READABLE;
    /// The uploaded cover of playlist `?2`, the caller's.
    pub(super) COVER_OWNED = "SELECT c.content_type, c.image FROM catalog_playlist_covers c \
         JOIN catalog_playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + OWNED;
    /// The caller's playlist imported from source `?2`.
    pub(super) FIND_BY_SOURCE = "SELECT id FROM catalog_playlists WHERE user_id = ?1 AND source_ref = ?2";
    /// The pull state of playlist `?1`: enabled, last check and error.
    pub(super) PULL_STATE = "SELECT enabled, last_checked_at, last_error \
         FROM catalog_playlist_pull_state WHERE playlist_id = ?1";
    /// Turn the pull for playlist `?1` on or off, keeping its snapshot.
    pub(super) PULL_SET = "INSERT INTO catalog_playlist_pull_state (playlist_id, enabled) \
         VALUES (?1, ?2) ON CONFLICT(playlist_id) DO UPDATE SET enabled = excluded.enabled";
    /// Every playlist that pulls: id, owner, source ref and stored snapshot.
    pub(super) PULL_DUE = "SELECT p.id, p.user_id, p.source_ref, s.snapshot_id \
         FROM catalog_playlists p JOIN catalog_playlist_pull_state s ON s.playlist_id = p.id \
         WHERE s.enabled = 1 AND p.source_ref LIKE 'spotify:%' ORDER BY p.id";
    /// One pull check: the time, the snapshot when it changed, the error.
    pub(super) PULL_CHECK = "UPDATE catalog_playlist_pull_state \
         SET last_checked_at = ?2, snapshot_id = COALESCE(?3, snapshot_id), \
         last_error = ?4 WHERE playlist_id = ?1";
    /// Record one check: the snapshot read, when, and the error, if any.
    pub(super) PULL_RECORD = "UPDATE catalog_playlist_pull_state \
         SET snapshot_id = ?2, last_checked_at = ?3, last_error = ?4 \
         WHERE playlist_id = ?1";
    /// The newest import job for playlist `?1`, while it runs or after it
    /// failed.
    pub(super) PLAYLIST_IMPORT_STATE = "SELECT state FROM durable_jobs \
         WHERE kind = 'spotify.import' \
         AND json_extract(payload_json, '$.playlist_id') = ?1 \
         ORDER BY created_at DESC LIMIT 1";
    /// For each `[track, artist, album]` in the JSON array `?2`, by its
    /// index, the playlists the caller may read holding an entry with
    /// exactly those names.
    pub(super) MEMBERSHIP = "WITH q(idx, track_name, artist_name, album_name) AS \
         (SELECT key, json_extract(value, '$[0]'), json_extract(value, '$[1]'), \
         json_extract(value, '$[2]') FROM json_each(?2)) \
         SELECT DISTINCT q.idx, t.playlist_id FROM q \
         JOIN catalog_playlist_tracks t ON t.track_name = q.track_name \
         AND t.artist_name = q.artist_name AND t.album_name = q.album_name \
         JOIN catalog_playlists p ON p.id = t.playlist_id WHERE " + READABLE
        + " ORDER BY q.idx, t.playlist_id";

    /// Whether playlist `?1` is user `?2`'s.
    pub(super) OWNED_GUARD = "SELECT 1 FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Whether user `?1` may change the songs of playlist `?2`.
    pub(super) EDIT_GUARD = "SELECT 1 FROM catalog_playlists p WHERE p.id = ?2 AND " + EDITABLE;
    /// Stamp playlist `?2` as changed at `?1`, milliseconds.
    pub(super) STAMP = "UPDATE catalog_playlists SET updated_at = ?1 WHERE id = ?2";
    /// Stamp user `?3`'s playlist `?2` as changed at `?1`, milliseconds.
    pub(super) TOUCH = "UPDATE catalog_playlists SET updated_at = ?1 \
         WHERE id = ?2 AND user_id = ?3";
    /// One empty private playlist made at `?3`, milliseconds.
    pub(super) INSERT_PLAYLIST = "INSERT INTO catalog_playlists (id, name, created_at, \
         updated_at, source_ref, user_id, is_public) VALUES (?1, ?2, ?3, ?3, ?4, ?5, 0)";
    /// Rename user `?3`'s playlist `?2`.
    pub(super) RENAME = "UPDATE catalog_playlists SET name = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Set the public flag of user `?3`'s playlist `?2`.
    pub(super) SET_PUBLIC = "UPDATE catalog_playlists SET is_public = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Delete user `?2`'s playlist `?1`; its entries and cover go with it.
    pub(super) DELETE_PLAYLIST = "DELETE FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Delete playlist `?1` if it has no owner; its entries and cover go
    /// with it.
    pub(super) DELETE_STRAY = "DELETE FROM catalog_playlists WHERE id = ?1 AND user_id IS NULL";
    /// One playlist as a write left it: name, flag and made time in epoch
    /// seconds.
    pub(super) SAVED = "SELECT name, is_public, created_at / 1000 \
         FROM catalog_playlists WHERE id = ?1";
    /// Entry ids in position order.
    pub(super) ENTRY_ORDER = "SELECT id FROM catalog_playlist_tracks WHERE playlist_id = ?1 \
         ORDER BY position";
    /// Place one entry.
    pub(super) SET_POSITION = "UPDATE catalog_playlist_tracks SET position = ?1 WHERE id = ?2";
    /// One entry with its length `?16` and made time `?17`, milliseconds.
    pub(super) INSERT_ENTRY = "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, \
         track_name, artist_name, album_name, album_id, artist_id, track_source_id, \
         cover_url, source_type, available_sources, format, track_number, disc_number, \
         duration_ms, created_at, plex_rating_key, library_file_id, recording_mbid) VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)";
    /// Every entry of one playlist.
    pub(super) CLEAR_ENTRIES = "DELETE FROM catalog_playlist_tracks WHERE playlist_id = ?1";
    /// One entry of one playlist.
    pub(super) REMOVE_ENTRY = "DELETE FROM catalog_playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// Whether playlist `?1` holds entry `?2`.
    pub(super) ENTRY_EXISTS = "SELECT 1 FROM catalog_playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// An entry's preferred source. A recording belongs to the source that
    /// gave it, so another source starts with none.
    pub(super) SET_SOURCE_TYPE = "UPDATE catalog_playlist_tracks SET source_type = ?1, \
         recording_mbid = CASE WHEN source_type = ?1 THEN recording_mbid END \
         WHERE id = ?2 AND playlist_id = ?3";
    /// An entry's known sources, as a JSON list.
    pub(super) SET_AVAILABLE_SOURCES = "UPDATE catalog_playlist_tracks SET available_sources = ?1 \
         WHERE id = ?2 AND playlist_id = ?3";
    /// The albums that own entry `e`'s release group.
    fragment ENTRY_OWNERS = (owns_group!("= lower(e.album_id)"));
    /// The caller's playlist `?2`'s entries with no source yet, each with
    /// the indexed library tracks on the albums that own its release group,
    /// entry by entry in playlist order. An entry's tracks come first from
    /// the copy a lookup by its group opens, then album by album.
    pub(super) LIBRARY_COPY_CANDIDATES = "SELECT e.id AS entry_id, e.track_name AS entry_title, \
         COALESCE(e.disc_number, 1) AS entry_disc, e.track_number AS entry_track, \
         t.id AS track_id, t.title AS track_title, t.disc_number AS track_disc, \
         t.track_number AS track_number \
         FROM catalog_playlist_tracks e JOIN local_tracks t \
         ON t.local_album_id IN (SELECT album_id FROM (" + ENTRY_OWNERS + ")) \
         AND +t.availability = 'indexed' \
         WHERE e.playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + OWNED + ") AND e.source_type = '' AND e.library_file_id IS NULL \
         AND e.album_id IS NOT NULL \
         ORDER BY e.position, t.local_album_id IS NOT ("
        + (copy_of_group!("= lower(e.album_id)"))
        + "), t.local_album_id, t.disc_number, t.track_number, t.id";
    /// An entry with no source yet and a library track that may be its copy.
    pub(super) row LibraryCopyCandidate for [LIBRARY_COPY_CANDIDATES] {
        entry_id: String,
        entry_title: String,
        entry_disc: i64,
        entry_track: Option<i64>,
        track_id: String,
        track_title: String,
        track_disc: i64,
        track_number: i64,
    }
    /// Point entry `?3` of the caller's playlist `?2`, still without a
    /// source, at library track `?4` while that track is indexed.
    pub(super) LINK_LIBRARY_COPY = "UPDATE catalog_playlist_tracks SET track_source_id = ?4, \
         library_file_id = ?4, source_type = 'local', available_sources = '[\"local\"]' \
         WHERE id = ?3 AND playlist_id = (SELECT p.id FROM catalog_playlists p \
         WHERE p.id = ?2 AND " + OWNED + ") AND source_type = '' AND library_file_id IS NULL \
         AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.id = ?4 AND +t.availability = 'indexed')";
    /// Every playlist, with its owner, holding an entry with no source yet
    /// whose release group an album with an indexed track owns: the ones a
    /// link pass opens.
    pub(super) PLAYLISTS_TO_LINK = "SELECT DISTINCT p.user_id AS user_id, p.id AS playlist_id \
         FROM catalog_playlist_tracks e JOIN catalog_playlists p ON p.id = e.playlist_id \
         WHERE e.source_type = '' AND e.library_file_id IS NULL AND e.album_id IS NOT NULL \
         AND p.user_id IS NOT NULL AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id IN (SELECT album_id FROM (" + ENTRY_OWNERS + ")) \
         AND +t.availability = 'indexed') ORDER BY p.user_id, p.id";
    /// A playlist a link pass opens, and its owner.
    pub(super) row PlaylistToLink for [PLAYLISTS_TO_LINK] {
        user_id: String,
        playlist_id: String,
    }
    /// What a link pass follows: the library's catalog revision, which
    /// every change to its albums and tracks moves, and the latest change
    /// to any playlist, which adding entries moves.
    pub(super) LINK_MARK = "SELECT COALESCE((SELECT value FROM library_catalog_revision \
         WHERE singleton = 1), 0) AS catalog_revision, \
         COALESCE((SELECT MAX(updated_at) FROM catalog_playlists), 0) AS playlists_changed";
    /// Where the library and the playlists stood at a link pass.
    pub(super) row LinkMark for [LINK_MARK] {
        catalog_revision: i64,
        playlists_changed: i64,
    }
    /// The people the caller's playlist `?2` is shared with, first shared
    /// first: user id, display name and whether they may edit.
    /// A member whose person did not choose their display name shows as
    /// "No display name".
    pub(super) MEMBERS = "SELECT u.id AS user_id, CASE WHEN "
        + (listed_user!("u"))
        + " THEN u.display_name ELSE 'No display name' END AS display_name, \
         m.can_edit AS can_edit FROM catalog_playlist_members m \
         JOIN auth_users u ON u.id = m.user_id WHERE m.playlist_id = ?2 \
         AND EXISTS (SELECT 1 FROM catalog_playlists p WHERE p.id = ?2 AND " + OWNED + ") \
         ORDER BY m.added_at, u.id";
    /// One person a playlist is shared with.
    pub(super) row MemberColumns for [MEMBERS] {
        user_id: String,
        display_name: String,
        can_edit: bool,
    }
    /// How many of the user ids in the JSON array `?1` name a user the
    /// user directory lists, or one already sharing playlist `?2`, who
    /// stays whether or not they chose a display name.
    pub(super) KNOWN_USERS = "SELECT COUNT(*) FROM auth_users u \
         WHERE u.id IN (SELECT value FROM json_each(?1)) AND (" + (listed_user!("u"))
        + " OR EXISTS (SELECT 1 FROM catalog_playlist_members m \
         WHERE m.playlist_id = ?2 AND m.user_id = u.id))";
    /// Unshare playlist `?1` from everyone not in the JSON array `?2`.
    pub(super) DROP_MEMBERS = "DELETE FROM catalog_playlist_members \
         WHERE playlist_id = ?1 AND user_id NOT IN (SELECT value FROM json_each(?2))";
    /// Share playlist `?1` with user `?2` at `?3`, milliseconds, who may
    /// edit when `?4`; one already sharing it keeps when they were added.
    pub(super) ADD_MEMBER = "INSERT INTO catalog_playlist_members (playlist_id, user_id, added_at, \
         can_edit) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT DO UPDATE SET can_edit = excluded.can_edit";
    /// Take user `?2` off playlist `?1`.
    pub(super) LEAVE = "DELETE FROM catalog_playlist_members WHERE playlist_id = ?1 AND user_id = ?2";
    /// Store or replace one playlist's uploaded cover.
    pub(super) UPSERT_COVER = "INSERT INTO catalog_playlist_covers (playlist_id, content_type, image, \
         updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (playlist_id) DO UPDATE SET \
         content_type = excluded.content_type, image = excluded.image, \
         updated_at = excluded.updated_at";
    /// Delete one playlist's uploaded cover.
    pub(super) DELETE_COVER = "DELETE FROM catalog_playlist_covers WHERE playlist_id = ?1";

    /// The entries of playlist `?2`, which the caller may read, in order,
    /// each with its live local file's album artist and path relative to
    /// its library root. A removed file reads as no file.
    pub(super) EXPORT_ENTRIES = "SELECT pt.track_name, pt.artist_name, pt.album_name, \
         t.album_artist_name, pt.duration_ms, t.relative_path \
         FROM catalog_playlist_tracks pt \
         LEFT JOIN local_tracks t ON t.id = pt.library_file_id AND "
        + (crate::local::sql::LIVE_TRACK)
        + " WHERE pt.playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + READABLE + ") ORDER BY pt.position";
    /// One playlist entry as an export lists it.
    pub(crate) row PlaylistExportRow for [EXPORT_ENTRIES] {
        track_name: String,
        artist_name: String,
        album_name: String,
        album_artist_name: Option<String>,
        duration_ms: Option<i64>,
        relative_path: Option<String>,
    }

    /// A user's favorites, of one kind `?2` or every kind, newest first.
    pub(super) FAVORITES = "SELECT f.item_id AS item_id, \
         n.display_name AS display_name, f.created_at / 1000.0 AS created_at \
         FROM catalog_user_favorites f LEFT JOIN catalog_user_favorite_names n \
         ON n.user_id = f.user_id AND n.item_kind = f.item_kind AND n.item_id = f.item_id \
         WHERE f.user_id = ?1 AND (?2 IS NULL OR f.item_kind = ?2) \
         ORDER BY f.created_at DESC, f.item_kind, f.item_id";
    pub(super) row FavoriteColumns for [FAVORITES] {
        item_id: String,
        display_name: Option<String>,
        created_at: f64,
    }
    /// When each of some items was favorited: item id, then the time.
    family STARRED_AT = starred_at;
    pub(super) row Starred for family [starred_at] {
        item_id: String,
        created_at: Millis,
    }
    /// Favorite one item; an existing row keeps its date.
    pub(super) ADD_FAVORITE = "INSERT INTO catalog_user_favorites \
         (user_id, item_kind, item_id, created_at) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (user_id, item_kind, item_id) DO NOTHING";
    /// Save or replace the name shown for one favorite.
    pub(super) NAME_FAVORITE = "INSERT INTO catalog_user_favorite_names \
         (user_id, item_kind, item_id, display_name) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (user_id, item_kind, item_id) DO UPDATE SET \
         display_name = excluded.display_name";
    /// Delete the name saved for one favorite.
    pub(super) UNNAME_FAVORITE = "DELETE FROM catalog_user_favorite_names \
         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3";
    /// Unfavorite one item.
    pub(super) REMOVE_FAVORITE = "DELETE FROM catalog_user_favorites \
         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3";

    /// Clear a user's rating of one item.
    pub(super) CLEAR_RATING = "DELETE FROM catalog_user_ratings \
         WHERE user_id = ?1 AND item_kind = ?2 AND item_id = ?3";
    /// Set a user's rating of one item.
    pub(super) SET_RATING = "INSERT INTO catalog_user_ratings \
         (user_id, item_kind, item_id, rating, rated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id, item_kind, item_id) DO UPDATE SET \
         rating = excluded.rating, rated_at = excluded.rated_at";
    /// Some items' ratings: item id, the user's own, the mean of everyone's.
    family RATINGS = ratings;
    /// The ids of one kind a user rated, highest first, then the most
    /// recently rated.
    pub(super) RATED_IDS = "SELECT item_id FROM catalog_user_ratings \
         WHERE user_id = ?1 AND item_kind = ?2 ORDER BY rating DESC, rated_at DESC, item_id";

    /// One follow with its follower and approval verdict.
    fragment FOLLOW_SELECT = "SELECT f.user_id AS user_id, \
         COALESCE(u.username_display, u.username, u.display_name, f.user_id) AS user_name, \
         COALESCE(u.role, 'user') AS user_role, f.artist_mbid AS artist_mbid, \
         f.artist_name AS artist_name, f.auto_download AS auto_download, \
         a.state AS approval_state, a.requested_at / 1000.0 AS requested_at, \
         f.followed_at / 1000.0 AS followed_at, f.updated_at / 1000.0 AS updated_at \
         FROM acquire_followed_artists f \
         LEFT JOIN auth_users u ON u.id = f.user_id \
         LEFT JOIN auto_download_approvals a \
         ON a.user_id = f.user_id AND a.artist_mbid_lower = f.artist_mbid_lower";
    /// User `?1`'s follow of lowercased artist `?2`.
    pub(super) FOLLOW = FOLLOW_SELECT + " WHERE f.user_id = ?1 AND f.artist_mbid_lower = ?2";
    /// A user's follows, newest first.
    pub(super) FOLLOWS = FOLLOW_SELECT
        + " WHERE f.user_id = ?1 ORDER BY f.followed_at DESC, f.artist_mbid_lower";
    pub(super) row FollowColumns for [FOLLOW, FOLLOWS] {
        user_id: String,
        user_name: String,
        user_role: String,
        artist_mbid: String,
        artist_name: String,
        auto_download: bool,
        approval_state: Option<String>,
        requested_at: Option<f64>,
        followed_at: f64,
        updated_at: f64,
    }
    /// The albums that own the feed row `n`'s release group.
    fragment OWNERS = (owns_group!("= +n.release_group_mbid_lower"));
    /// How many release sightings a filter matches.
    family RELEASE_COUNT = release_count;
    /// One page of release sightings, in `ReleaseRow`'s mapper order.
    family RELEASE_PAGE = release_page;
    /// Releases not yet owned, discovered after user `?1`'s seen marker.
    pub(super) UNSEEN_RELEASES = "SELECT COUNT(*) FROM acquire_new_release_feed n \
         JOIN acquire_followed_artists f \
         ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ?1 \
         WHERE NOT EXISTS (" + OWNERS + ") \
         AND n.discovered_at > COALESCE( \
         (SELECT seen_at FROM catalog_new_release_seen_marks WHERE user_id = ?1), 0)";
    /// Move a user's seen marker.
    pub(super) MARK_SEEN = "INSERT INTO catalog_new_release_seen_marks (user_id, seen_at) \
         VALUES (?1, ?2) ON CONFLICT (user_id) DO UPDATE SET seen_at = excluded.seen_at";

    /// Mark the tracks of one scope `?3` played or unplayed for user `?1`
    /// at `?2`, milliseconds.
    family MARK_PLAY = mark_play;
}

/// `?, ?, ...` placeholders for an IN-list.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// `STARRED_AT`: the user, the kind, then the ids.
pub(super) fn starred_at(ids: ListLen) -> String {
    format!(
        "SELECT item_id, created_at FROM catalog_user_favorites \
         WHERE user_id = ? AND item_kind = ? AND item_id IN ({})",
        marks(ids)
    )
}

/// `RATINGS`: the user, the kind, then the ids.
pub(super) fn ratings(ids: ListLen) -> String {
    format!(
        "SELECT item_id, MAX(CASE WHEN user_id = ? THEN rating END), AVG(rating) \
         FROM catalog_user_ratings WHERE item_kind = ? AND item_id IN ({}) GROUP BY item_id",
        marks(ids)
    )
}

/// The rows a release list reads. The user binds first, then with `since`
/// a date and a discovery time: a release dated on or after the date or,
/// undated, discovered at or after the time.
fn release_from(owned_too: bool, since: bool) -> String {
    let mut from = "FROM acquire_new_release_feed n JOIN acquire_followed_artists f \
         ON f.artist_mbid_lower = n.artist_mbid_lower AND f.user_id = ? WHERE 1 = 1"
        .to_owned();
    if !owned_too {
        from.push_str(&format!(" AND NOT EXISTS ({OWNERS})"));
    }
    if since {
        from.push_str(
            " AND (n.first_release_date >= ? \
             OR (n.first_release_date IS NULL AND n.discovered_at >= ?))",
        );
    }
    from
}

/// `RELEASE_COUNT`.
pub(super) fn release_count(owned_too: bool, since: bool) -> String {
    format!("SELECT COUNT(*) {}", release_from(owned_too, since))
}

/// `RELEASE_PAGE`: the binds of [`release_from`], then limit and offset.
pub(super) fn release_page(owned_too: bool, since: bool) -> String {
    format!(
        "SELECT n.release_group_mbid, n.title, n.artist_name, f.artist_mbid, n.primary_type, \
         n.first_release_date, EXISTS ({OWNERS}) \
         {} ORDER BY n.first_release_date DESC, n.discovered_at DESC LIMIT ? OFFSET ?",
        release_from(owned_too, since)
    )
}

droppedneedle_kernel::sql_choice! {
    /// What a played or unplayed mark covers.
    pub(super) enum MarkTracks {
        /// One track row.
        Track,
        /// Every track row of an album, apart from the missing rows that
        /// continued into another track.
        Album,
        /// The streamable tracks on the albums an artist leads.
        AlbumArtist,
    }
}

/// `MARK_PLAY`: played keeps an earlier unplayed mark; unplayed clears
/// the played one.
pub(super) fn mark_play(tracks: MarkTracks, played: bool) -> String {
    let tracks = match tracks {
        MarkTracks::Track => "SELECT id FROM local_tracks WHERE id = ?3",
        MarkTracks::Album => {
            "SELECT id FROM local_tracks WHERE local_album_id = ?3 \
             AND id NOT IN (SELECT alias FROM local_track_aliases)"
        }
        MarkTracks::AlbumArtist => {
            "SELECT t.id FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
             WHERE +t.availability = 'indexed' AND a.album_artist_id = ?3"
        }
    };
    let (values, update) = if played {
        ("?2, NULL", "marked_played_at = excluded.marked_played_at")
    } else {
        (
            "NULL, ?2",
            "marked_unplayed_at = excluded.marked_unplayed_at, marked_played_at = NULL",
        )
    };
    format!(
        "INSERT INTO catalog_user_play_marks \
         (user_id, local_track_id, marked_played_at, marked_unplayed_at) \
         SELECT ?1, id, {values} FROM ({tracks}) WHERE true \
         ON CONFLICT (user_id, local_track_id) DO UPDATE SET {update}"
    )
}
