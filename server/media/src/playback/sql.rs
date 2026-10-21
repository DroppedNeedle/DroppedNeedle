//! The playback store's statements.

use droppedneedle_library::corrections::continuation::CURRENT_TRACK;

droppedneedle_kernel::sql! {
    /// The track a local id names now, with its recording and the release
    /// group its album was matched to. The recording is the accepted
    /// identity's, else the file's own: the rule catalog's `recording_mbid!`
    /// states, written out here because media sits below catalog.
    pub(super) TRACK = "SELECT id, title, artist_name, album_title, duration_seconds, \
         COALESCE((SELECT i.recording_mbid FROM local_track_external_identities i \
                   WHERE i.local_track_id = local_tracks.id AND i.provider = 'musicbrainz'), \
                  embedded_recording_mbid) AS recording_mbid, \
         (SELECT e.release_group_mbid FROM local_album_external_identities e \
          WHERE e.local_album_id = local_tracks.local_album_id) AS rg_mbid \
         FROM local_tracks WHERE id = (" + CURRENT_TRACK + ")";
    /// The track a local id names now, so a play is filed under the track
    /// an id continued or merged into by the time it is written.
    pub(super) CURRENT_TRACK_ID = "SELECT (" + CURRENT_TRACK + ")";
    /// Record one play, filed under the track, its album and its first
    /// credited artist when the track is known.
    pub(super) RECORD_PLAY = "INSERT INTO library_play_history \
         (id, user_id, local_track_id, local_album_id, local_artist_id, track_name, \
          artist_name, album_name, recording_mbid, release_group_mbid, duration_ms, \
          source, played_at) \
         SELECT ?1, ?2, t.id, t.local_album_id, \
          COALESCE((SELECT x.local_artist_id FROM local_track_artists x \
                    WHERE x.local_track_id = t.id ORDER BY x.position LIMIT 1), \
                   a.album_artist_id), \
          ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11 \
         FROM (SELECT 1) \
         LEFT JOIN local_tracks t ON t.id = ?3 \
         LEFT JOIN local_albums a ON a.id = t.local_album_id";
    /// One play as a scrobble of it is sent.
    pub(super) PLAY = "SELECT user_id, track_name, artist_name, album_name, recording_mbid, \
         duration_ms, played_at FROM library_play_history WHERE id = ?1";
    /// One user's listening preferences.
    pub(super) PREFS = "SELECT scrobble_to_lastfm, scrobble_to_listenbrainz, \
         navidrome_handles_external_scrobbles, primary_music_source, \
         now_playing_visibility, auto_request_personal_mix \
         FROM user_listening_prefs WHERE user_id = ?1";
    /// Store one user's whole preferences row.
    pub(super) SAVE_PREFS = "INSERT INTO user_listening_prefs \
         (user_id, scrobble_to_lastfm, scrobble_to_listenbrainz, \
          navidrome_handles_external_scrobbles, primary_music_source, \
          now_playing_visibility, updated_at, auto_request_personal_mix) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (user_id) DO UPDATE SET \
          scrobble_to_lastfm = excluded.scrobble_to_lastfm, \
          scrobble_to_listenbrainz = excluded.scrobble_to_listenbrainz, \
          navidrome_handles_external_scrobbles = excluded.navidrome_handles_external_scrobbles, \
          primary_music_source = excluded.primary_music_source, \
          now_playing_visibility = excluded.now_playing_visibility, \
          updated_at = excluded.updated_at, \
          auto_request_personal_mix = excluded.auto_request_personal_mix";
    /// The stored document of one user's link to one service.
    pub(super) CONNECTION = "SELECT connection_data FROM user_connections \
         WHERE user_id = ?1 AND service = ?2";
    /// Store or replace one user's link to one service.
    pub(super) SAVE_CONNECTION = "INSERT INTO user_connections \
         (user_id, service, connection_data, enabled, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 1, ?4, ?4) \
         ON CONFLICT (user_id, service) DO UPDATE SET \
          connection_data = excluded.connection_data, \
          updated_at = excluded.updated_at";
    /// Remove one user's link to one service.
    pub(super) DELETE_CONNECTION = "DELETE FROM user_connections \
         WHERE user_id = ?1 AND service = ?2";
    /// The enabled scrobble links of one user.
    pub(super) SCROBBLE_LINKS = "SELECT service, connection_data FROM user_connections \
         WHERE user_id = ?1 AND enabled = 1 AND service IN ('lastfm', 'listenbrainz')";
    /// One user's display name and username.
    pub(super) DISPLAY_NAMES = "SELECT display_name, username FROM auth_users WHERE id = ?1";
}
