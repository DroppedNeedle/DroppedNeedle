//! Production playback ports over the SQLite schema.
//!
//! The reporting traits are synchronous, so these stores use the blocking
//! forms of [`Db`]: reads go through the request read pool and a play is
//! one background write on the writer lane. Catalog reads hit
//! `local_tracks`, plays land in `library_play_history`, prefs read
//! `user_listening_prefs`, and display names resolve from `auth_users`.
//!
//! Every call runs through [`off_worker`], which moves a multi-threaded
//! runtime's other tasks off the current thread before it waits on the
//! database, so a busy database never stalls unrelated requests. Reads fail
//! closed (misses and defaults, never an error to the player) and log what
//! failed. A play the lane refuses is logged at error level with its
//! details, so no play disappears silently.

use std::sync::Arc;

use droppedneedle_auth::times::to_iso;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_library::corrections::continuation::CURRENT_TRACK;
use droppedneedle_library::repairs::plays::{Matched, PlayKeys, match_play};
use rusqlite::OptionalExtension;

use super::ports::{
    DisplayNames, ListeningPrefs, PlayHistory, PlayRecord, ProviderFailure, ScrobbleLinks,
    ScrobblePrefs, ScrobbleTargets, TrackCatalog, TrackInfo, VISIBILITY_FULL,
};

/// Production playback stores over the database.
pub struct PlaybackDb {
    db: Db,
    ids: Arc<dyn IdGenerator>,
}

impl PlaybackDb {
    /// Playback reads and writes on `db`.
    pub fn new(db: Db, ids: Arc<dyn IdGenerator>) -> Self {
        Self { db, ids }
    }
}

/// Run blocking database work. On a multi-threaded runtime the worker hands
/// its other tasks to the rest of the pool first (`block_in_place`); on a
/// current-thread runtime (tests) or outside a runtime it runs inline.
fn off_worker<R>(work: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// The answer of a read, or `fallback` after logging why the read failed.
fn or_logged<T>(read: &'static str, result: Result<T, DbError>, fallback: impl FnOnce() -> T) -> T {
    result.unwrap_or_else(|error| {
        tracing::warn!(read, %error, "playback read failed; answering the default");
        fallback()
    })
}

impl TrackCatalog for PlaybackDb {
    fn get_track(&self, track_id: &str) -> Option<TrackInfo> {
        off_worker(|| or_logged("playback-track", self.read_track(track_id), || None))
    }
}

impl PlaybackDb {
    /// The track a local id names now: an id that continued into another
    /// track reads as that track (retired ids stay aliases).
    fn read_track(&self, track_id: &str) -> Result<Option<TrackInfo>, DbError> {
        self.db.read_blocking("playback-track", |conn| {
            let track = conn
                .query_row(
                    &format!(
                        "SELECT id, title, artist_name, album_title, duration_seconds, \
                         embedded_recording_mbid, \
                         (SELECT e.release_group_mbid FROM local_album_external_identities e \
                          WHERE e.local_album_id = local_tracks.local_album_id) AS rg_mbid \
                         FROM local_tracks WHERE id = ({CURRENT_TRACK})"
                    ),
                    [track_id],
                    |row| {
                        let duration_ms = row
                            .get::<_, Option<f64>>("duration_seconds")?
                            .map(|secs| (secs * 1000.0) as i64)
                            .unwrap_or(0);
                        Ok(TrackInfo {
                            track_id: row.get("id")?,
                            title: row.get("title")?,
                            artist_name: row
                                .get::<_, Option<String>>("artist_name")?
                                .unwrap_or_default(),
                            album_title: row.get("album_title")?,
                            duration_ms,
                            recording_mbid: row
                                .get::<_, Option<String>>("embedded_recording_mbid")?
                                .filter(|mbid| !mbid.is_empty()),
                            // The release group the album was matched to,
                            // never an id from the file's tags: an unmatched
                            // album has none, and no cover address.
                            rg_mbid: row
                                .get::<_, Option<String>>("rg_mbid")?
                                .filter(|mbid| !mbid.is_empty()),
                        })
                    },
                )
                .optional()?;
            Ok(track)
        })
    }
}

impl PlayHistory for PlaybackDb {
    fn record(&self, user_id: &str, record: &PlayRecord) {
        off_worker(|| self.write_play(user_id, record));
    }
}

impl PlaybackDb {
    /// Insert one play as a background write. A play the lane refuses is
    /// logged with what it was, never dropped silently.
    ///
    /// A play of a library track is filed under the track, its album and
    /// its first credited artist (the album artist when the track credits
    /// none), all read from the catalog in the insert itself. A track that
    /// is gone leaves the three ids empty and the play is kept by name.
    ///
    /// A play made on a connected server or as a preview names no library
    /// track. It is filed the same way when it clearly matches one: see
    /// [`remote_match`], which reads on the write's own transaction.
    fn write_play(&self, user_id: &str, record: &PlayRecord) {
        let id = self.ids.new_id();
        let outcome = self
            .db
            .write_blocking(Lane::Background, "playback-play", |tx| {
                let track_id = record.track_id.clone().or_else(|| remote_match(tx, record));
                tx.execute(
                    "INSERT INTO library_play_history \
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
                     LEFT JOIN local_albums a ON a.id = t.local_album_id",
                    rusqlite::params![
                        id,
                        user_id,
                        track_id,
                        record.track_name,
                        record.artist_name,
                        record.album_name,
                        record.recording_mbid,
                        record.release_group_mbid,
                        record.duration_ms,
                        record.source,
                        to_iso(record.played_at),
                    ],
                )?;
                Ok::<_, DbError>(())
            });
        if let Err(error) = outcome {
            tracing::error!(
                %error,
                user_id,
                track = %record.track_name,
                artist = %record.artist_name,
                played_at = record.played_at,
                "play history write failed; the play is not recorded"
            );
        }
    }
}

/// The library track a play made on a connected server or as a preview
/// is filed under: the one playable track that holds the play's recording
/// id. Names alone never file such a play, and neither does a recording
/// several tracks share. The rule is the one the repair of older plays
/// applies to such a play, so it is filed the same way then and now. A
/// play of the library that names no track is not matched here: the
/// repair files it by its names after the next scan or start.
fn remote_match(conn: &rusqlite::Connection, record: &PlayRecord) -> Option<String> {
    let keys = PlayKeys {
        track_name: record.track_name.clone(),
        artist_name: record.artist_name.clone(),
        album_name: record.album_name.clone(),
        recording_mbid: record.recording_mbid.clone(),
        release_group_mbid: record.release_group_mbid.clone(),
        duration_ms: record.duration_ms,
        source: record.source.clone(),
    };
    if keys.is_local() {
        return None;
    }
    match match_play(conn, &keys) {
        Ok(Matched::Track { id, .. }) => Some(id),
        Ok(Matched::Left(_)) => None,
        Err(error) => {
            tracing::warn!(%error, "play could not be matched to a track; kept by name");
            None
        }
    }
}

/// What a user's prefs read as before they save any: the column defaults.
fn default_scrobble_prefs() -> ScrobblePrefs {
    ScrobblePrefs {
        scrobble_to_lastfm: false,
        scrobble_to_listenbrainz: false,
        navidrome_handles_external: true,
    }
}

impl ListeningPrefs for PlaybackDb {
    fn scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs {
        off_worker(|| {
            or_logged(
                "playback-scrobble-prefs",
                self.read_scrobble_prefs(user_id),
                default_scrobble_prefs,
            )
        })
    }

    fn visibility(&self, user_id: &str) -> Result<String, ProviderFailure> {
        off_worker(|| {
            self.read_visibility(user_id).map_err(|error| {
                tracing::warn!(read = "playback-visibility", %error, "playback read failed");
                ProviderFailure(error.to_string())
            })
        })
    }
}

impl PlaybackDb {
    fn read_scrobble_prefs(&self, user_id: &str) -> Result<ScrobblePrefs, DbError> {
        self.db.read_blocking("playback-scrobble-prefs", |conn| {
            let prefs = conn
                .query_row(
                    "SELECT scrobble_to_lastfm, scrobble_to_listenbrainz, \
                     navidrome_handles_external_scrobbles FROM user_listening_prefs \
                     WHERE user_id = ?1",
                    [user_id],
                    |row| {
                        Ok(ScrobblePrefs {
                            scrobble_to_lastfm: row.get::<_, i64>(0)? != 0,
                            scrobble_to_listenbrainz: row.get::<_, i64>(1)? != 0,
                            navidrome_handles_external: row.get::<_, i64>(2)? != 0,
                        })
                    },
                )
                .optional()?;
            // No row yet: the column defaults are the right answer.
            Ok(prefs.unwrap_or_else(default_scrobble_prefs))
        })
    }

    fn read_visibility(&self, user_id: &str) -> Result<String, DbError> {
        self.db.read_blocking("playback-visibility", |conn| {
            let visibility = conn
                .query_row(
                    "SELECT now_playing_visibility FROM user_listening_prefs WHERE user_id = ?1",
                    [user_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok(visibility.unwrap_or_else(|| VISIBILITY_FULL.to_owned()))
        })
    }
}

impl ScrobbleLinks for PlaybackDb {
    fn linked(&self, user_id: &str) -> ScrobbleTargets {
        off_worker(|| {
            or_logged(
                "playback-scrobble-links",
                self.read_links(user_id),
                ScrobbleTargets::default,
            )
        })
    }
}

impl PlaybackDb {
    /// Linked scrobble services from `user_connections`: a ListenBrainz
    /// row (its document is sealed whole, so presence is the signal) and
    /// a Last.fm row whose plain document carries a session key. A failed
    /// read reads as unlinked.
    fn read_links(&self, user_id: &str) -> Result<ScrobbleTargets, DbError> {
        let rows = self.db.read_blocking("playback-scrobble-links", |conn| {
            let mut query = conn.prepare(
                "SELECT service, connection_data FROM user_connections \
                 WHERE user_id = ?1 AND enabled = 1 AND service IN ('lastfm', 'listenbrainz')",
            )?;
            let rows = query
                .query_map([user_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, DbError>(rows)
        })?;
        let mut targets = ScrobbleTargets::default();
        for (service, data) in rows {
            match service.as_str() {
                "listenbrainz" => targets.listenbrainz = true,
                "lastfm" => {
                    targets.lastfm = serde_json::from_str::<serde_json::Value>(&data)
                        .ok()
                        .and_then(|doc| {
                            doc.get("session_key")
                                .and_then(serde_json::Value::as_str)
                                .map(|key| !key.is_empty())
                        })
                        .unwrap_or(false);
                }
                _ => {}
            }
        }
        Ok(targets)
    }
}

impl DisplayNames for PlaybackDb {
    fn display_name(&self, user_id: &str) -> String {
        off_worker(|| {
            or_logged(
                "playback-display-name",
                self.read_display_name(user_id),
                || None,
            )
            .unwrap_or_else(|| user_id.to_owned())
        })
    }
}

impl PlaybackDb {
    /// The user's display name, else their username; `None` when neither
    /// is set or the user is gone.
    fn read_display_name(&self, user_id: &str) -> Result<Option<String>, DbError> {
        let names = self.db.read_blocking("playback-display-name", |conn| {
            let names = conn
                .query_row(
                    "SELECT display_name, username FROM auth_users WHERE id = ?1",
                    [user_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                        ))
                    },
                )
                .optional()?;
            Ok::<_, DbError>(names)
        })?;
        Ok(names.and_then(|(display, username)| {
            display
                .into_iter()
                .chain(username)
                .find(|name| !name.trim().is_empty())
        }))
    }
}
