//! The statements of the lists on rotation: one read per idea, each
//! answering the ids of its songs in the list's order with how many songs
//! match in all, then the rows the refresh and the routes keep.
//!
//! A shuffled idea's read binds the seed's three numbers first, as the
//! player's shuffle key reads them; the person follows.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::time::Millis;

use super::RotationIdea;
use crate::local::sql::{LIVE_TRACK, PLAYER_SHUFFLE_KEY, PLAYS};

droppedneedle_kernel::sql! {
    /// The person `?4` played track `t`, or marked it played.
    fragment PLAYED = "(EXISTS (SELECT 1 FROM " + PLAYS + " WHERE h.user_id = ?4 \
         AND h.local_track_id = t.id) OR EXISTS (SELECT 1 FROM catalog_user_play_marks m \
         WHERE m.user_id = ?4 AND m.local_track_id = t.id AND m.marked_played_at IS NOT NULL))";
    /// The person `?4` played track `t` at `?5` or later.
    fragment PLAYED_SINCE = "EXISTS (SELECT 1 FROM " + PLAYS + " WHERE h.user_id = ?4 \
         AND h.local_track_id = t.id AND h.played_at >= ?5)";
    /// A song's id and how many songs match in all.
    fragment PICKED = "SELECT t.id AS id, COUNT(*) OVER () AS matches FROM local_tracks t ";
    /// The week's shuffle, then the track's own key.
    fragment SHUFFLED = " ORDER BY " + PLAYER_SHUFFLE_KEY + ", t.search_key";

    /// The newest songs in the library. Binds: the limit.
    pub(crate) ADDED_RECENTLY = PICKED + "WHERE " + LIVE_TRACK
        + " ORDER BY t.imported_at DESC, t.id DESC LIMIT ?1";
    /// The songs person `?1` played twice or more since `?2`, most played
    /// first, then the latest played. Binds: the person, the time, the
    /// limit.
    pub(crate) ON_REPEAT = "SELECT t.id AS id, COUNT(*) OVER () AS matches FROM \
         (SELECT h.local_track_id AS track_id, COUNT(*) AS plays, MAX(h.played_at) AS last_at \
         FROM " + PLAYS + " WHERE h.user_id = ?1 AND h.local_track_id IS NOT NULL \
         AND h.played_at >= ?2 GROUP BY h.local_track_id HAVING COUNT(*) >= 2) p \
         CROSS JOIN local_tracks t ON t.id = p.track_id WHERE " + LIVE_TRACK
        + " ORDER BY p.plays DESC, p.last_at DESC, t.id LIMIT ?3";
    /// The songs person `?1` played from `?2` up to `?3`, most played then
    /// first. Binds: the person, the two times, the limit.
    pub(crate) A_YEAR_AGO = "SELECT t.id AS id, COUNT(*) OVER () AS matches FROM \
         (SELECT h.local_track_id AS track_id, COUNT(*) AS plays, MAX(h.played_at) AS last_at \
         FROM " + PLAYS + " WHERE h.user_id = ?1 AND h.local_track_id IS NOT NULL \
         AND h.played_at >= ?2 AND h.played_at < ?3 GROUP BY h.local_track_id) p \
         CROSS JOIN local_tracks t ON t.id = p.track_id WHERE " + LIVE_TRACK
        + " ORDER BY p.plays DESC, p.last_at DESC, t.id LIMIT ?4";
    /// The songs person `?4` rated four or more or hearted, alone or by
    /// their album, that they have not played since `?5`. Binds: the seed,
    /// the person, the time, the limit.
    pub(crate) FORGOTTEN_FAVOURITES = PICKED + "WHERE t.id IN (SELECT r.item_id \
         FROM catalog_user_ratings r WHERE r.user_id = ?4 AND r.item_kind = 'track' \
         AND r.rating >= 4 UNION SELECT f.item_id FROM catalog_user_favorites f \
         WHERE f.user_id = ?4 AND f.item_kind = 'track' UNION SELECT ft.id \
         FROM catalog_user_favorites f CROSS JOIN local_tracks ft ON ft.local_album_id = f.item_id \
         WHERE f.user_id = ?4 AND f.item_kind = 'album') AND " + LIVE_TRACK
        + " AND NOT " + PLAYED_SINCE + SHUFFLED + " LIMIT ?6";
    /// The songs person `?4` never played on the 20 albums they played
    /// most, reached from those albums by key: the unary plus keeps the
    /// planner off the indexes led by `availability`, which walk every
    /// track. Binds: the seed, the person, the limit.
    pub(crate) DEEP_CUTS = "SELECT t.id AS id, COUNT(*) OVER () AS matches FROM \
         (SELECT a.local_album_id AS album_id FROM library_user_album_plays a \
         WHERE a.user_id = ?4 ORDER BY a.plays DESC, a.local_album_id LIMIT 20) top \
         CROSS JOIN local_tracks t ON t.local_album_id = top.album_id \
         WHERE +t.availability = 'indexed' AND NOT EXISTS (SELECT 1 FROM local_albums ra \
         WHERE ra.id = t.local_album_id AND +ra.retired_into_album_id IS NOT NULL) \
         AND NOT " + PLAYED + SHUFFLED + " LIMIT ?5";
    /// The songs person `?4` rated five. Binds: the seed, the person, the
    /// limit.
    pub(crate) FIVE_STARS = PICKED + "WHERE t.id IN (SELECT r.item_id \
         FROM catalog_user_ratings r WHERE r.user_id = ?4 AND r.item_kind = 'track' \
         AND r.rating = 5) AND " + LIVE_TRACK + SHUFFLED + " LIMIT ?5";
    /// The songs person `?4` never played and never marked played. Binds:
    /// the seed, the person, the limit.
    pub(crate) NEVER_PLAYED = PICKED + "WHERE " + LIVE_TRACK + " AND NOT " + PLAYED
        + SHUFFLED + " LIMIT ?5";
    /// The songs by artists person `?4` follows that they have not played
    /// since `?5`. Binds: the seed, the person, the time, the limit.
    pub(crate) FOLLOWED_ARTISTS = PICKED + "WHERE t.id IN (SELECT ta.local_track_id \
         FROM acquire_followed_artists f CROSS JOIN local_artist_external_identities le \
         ON lower(le.provider_artist_id) = f.artist_mbid_lower AND +le.provider = 'musicbrainz' \
         CROSS JOIN local_track_artists ta ON ta.local_artist_id = le.local_artist_id \
         WHERE f.user_id = ?4) AND " + LIVE_TRACK + " AND NOT " + PLAYED_SINCE
        + SHUFFLED + " LIMIT ?6";
    /// FLAC, WAV and ALAC songs: an MP4 file is ALAC when the scan read a
    /// bit depth. Binds: the seed, the limit.
    pub(crate) LOSSLESS = PICKED + "WHERE " + LIVE_TRACK
        + " AND (t.file_format IN ('flac', 'wav') OR (t.file_format = 'm4a' \
         AND t.bit_depth IS NOT NULL))" + SHUFFLED + " LIMIT ?4";
    /// The songs of seven minutes or more. Binds: the seed, the limit.
    pub(crate) LONG_ONES = PICKED + "WHERE " + LIVE_TRACK + " AND t.duration_seconds >= 420"
        + SHUFFLED + " LIMIT ?4";
    /// The songs of the decade starting `?4`. Binds: the seed, the decade,
    /// the limit.
    pub(crate) DECADE = PICKED + "WHERE " + LIVE_TRACK + " AND t.year >= ?4 AND t.year < ?4 + 10"
        + SHUFFLED + " LIMIT ?5";
    /// A song an idea picked, and how many match in all.
    pub(crate) row Picked for [ADDED_RECENTLY, ON_REPEAT, A_YEAR_AGO, FORGOTTEN_FAVOURITES,
        DEEP_CUTS, FIVE_STARS, NEVER_PLAYED, FOLLOWED_ARTISTS, LOSSLESS, LONG_ONES, DECADE] {
        id: String,
        matches: i64,
    }
    /// How many songs each decade of the library holds, oldest first.
    pub(crate) DECADE_SONGS = "SELECT (t.year / 10) * 10 AS decade, COUNT(*) AS songs \
         FROM local_tracks t WHERE " + LIVE_TRACK + " AND t.year BETWEEN 1000 AND 2999 \
         GROUP BY 1 ORDER BY 1";
    /// A decade and its songs.
    pub(crate) row DecadeSongs for [DECADE_SONGS] {
        decade: i64,
        songs: i64,
    }

    /// Person `?1`'s rows, one an idea looked at.
    pub(crate) LISTS = "SELECT idea, decade, playlist_id, pinned, match_count, last_live_at, \
         checked_at FROM catalog_rotation_lists WHERE user_id = ?1";
    /// One idea's row.
    pub(crate) row ListRow for [LISTS] {
        idea: RotationIdea,
        decade: Option<i64>,
        playlist_id: Option<String>,
        pinned: bool,
        match_count: i64,
        last_live_at: Option<Millis>,
        checked_at: Option<Millis>,
    }
    /// Whether person `?1` still has an account.
    pub(crate) ACCOUNT = "SELECT EXISTS (SELECT 1 FROM auth_users WHERE id = ?1)";
    /// Whether person `?1` turned lists on rotation on; no row is on.
    pub(crate) ENABLED = "SELECT enabled FROM catalog_rotation_prefs WHERE user_id = ?1";
    /// The library files of playlist `?1`'s entries, in order.
    pub(crate) ENTRY_FILES = "SELECT library_file_id FROM catalog_playlist_tracks \
         WHERE playlist_id = ?1 ORDER BY position";
    /// The idea of person `?1`'s playlist `?2`, when it is a list on
    /// rotation.
    pub(crate) IDEA_OF = "SELECT idea FROM catalog_rotation_lists \
         WHERE user_id = ?1 AND playlist_id = ?2";
    /// The people among the JSON list `?1` with lists on rotation on whose
    /// rows were not all looked at since `?2`: the ones a refresh is due.
    pub(crate) DUE = "SELECT s.value FROM json_each(?1) s \
         WHERE COALESCE((SELECT p.enabled FROM catalog_rotation_prefs p \
         WHERE p.user_id = s.value), 1) = 1 \
         AND COALESCE((SELECT MIN(COALESCE(l.checked_at, 0)) FROM catalog_rotation_lists l \
         WHERE l.user_id = s.value), 0) < ?2";

    /// Person `?1`'s idea `?2` looked at `?5`: `?4` songs, showing decade
    /// `?3` when given (else the one it showed last).
    pub(crate) LOOKED = "INSERT INTO catalog_rotation_lists \
         (user_id, idea, decade, match_count, checked_at) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id, idea) DO UPDATE SET \
         decade = COALESCE(excluded.decade, decade), match_count = excluded.match_count, \
         checked_at = excluded.checked_at";
    /// The playlist person `?1`'s idea `?2` fills now.
    pub(crate) LIVE_PLAYLIST = "SELECT playlist_id FROM catalog_rotation_lists \
         WHERE user_id = ?1 AND idea = ?2";
    /// Person `?1`'s idea `?2` goes live at `?4` in playlist `?3`, unless it
    /// is live already.
    pub(crate) GO_LIVE = "UPDATE catalog_rotation_lists SET playlist_id = ?3, last_live_at = ?4, \
         pinned = 0 WHERE user_id = ?1 AND idea = ?2 AND playlist_id IS NULL";
    /// Person `?1`'s idea `?2` is still live at `?3`.
    pub(crate) STAY_LIVE = "UPDATE catalog_rotation_lists SET last_live_at = ?3 \
         WHERE user_id = ?1 AND idea = ?2 AND playlist_id IS NOT NULL";
    /// Delete person `?1`'s list on rotation `?2`.
    pub(crate) DROP_LIST = "DELETE FROM catalog_playlists WHERE id = ?2 AND user_id = ?1 \
         AND EXISTS (SELECT 1 FROM catalog_rotation_lists l \
         WHERE l.playlist_id = ?2 AND l.user_id = ?1)";
    /// Person `?1`'s idea `?2` is no longer live.
    pub(crate) GO_OUT = "UPDATE catalog_rotation_lists SET playlist_id = NULL, pinned = 0 \
         WHERE user_id = ?1 AND idea = ?2";
    /// Turn person `?1`'s lists on rotation on (`?2`) or off at `?3`.
    pub(crate) SET_ENABLED = "INSERT INTO catalog_rotation_prefs (user_id, enabled, updated_at) \
         VALUES (?1, ?2, ?3) ON CONFLICT (user_id) DO UPDATE SET \
         enabled = excluded.enabled, updated_at = excluded.updated_at";
    /// Delete every list on rotation of person `?1`.
    pub(crate) DROP_ALL = "DELETE FROM catalog_playlists WHERE user_id = ?1 \
         AND id IN (SELECT playlist_id FROM catalog_rotation_lists WHERE user_id = ?1)";
    /// Person `?1` has no list live, none kept, and nothing looked at, so a
    /// refresh picks again at once.
    pub(crate) RESET_ALL = "UPDATE catalog_rotation_lists SET playlist_id = NULL, pinned = 0, \
         checked_at = NULL WHERE user_id = ?1";
    /// Keep person `?1`'s list on rotation `?2` on past Monday (`?3`), or
    /// let it go.
    pub(crate) PIN = "UPDATE catalog_rotation_lists SET pinned = ?3 \
         WHERE user_id = ?1 AND playlist_id = ?2";
}
