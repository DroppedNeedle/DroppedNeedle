//! Production playback ports over the SQLite schema.
//!
//! The reporting traits are synchronous, so these stores use one rusqlite
//! handle on the runtime database instead of the async pool. The handle is
//! opened through [`crate::db::open_connection`], so it carries the same
//! pragmas as every other connection. Catalog reads hit `local_tracks`,
//! plays land in `library_play_history`, prefs read
//! `user_listening_prefs`, and display names resolve from `auth_users`.
//!
//! Every call runs through [`off_worker`], which moves a multi-threaded
//! runtime's other tasks off the current thread before touching SQLite, so a
//! busy database never stalls unrelated requests. Reads fail closed (misses
//! and defaults, never an error to the player). A play is retried while the
//! database is busy and logged at error level with its details if it still
//! cannot be written, so no play disappears silently.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auth::times::to_iso;
use crate::ids::IdGenerator;

use super::ports::{
    DisplayNames, ListeningPrefs, PlayHistory, PlayRecord, ProviderFailure, ScrobbleLinks,
    ScrobblePrefs, ScrobbleTargets, TrackCatalog, TrackInfo, VISIBILITY_FULL,
};

/// Production playback stores over one rusqlite handle.
pub struct PlaybackDb {
    conn: Mutex<rusqlite::Connection>,
    ids: Arc<dyn IdGenerator>,
}

impl PlaybackDb {
    /// Open the runtime database for playback reads and writes.
    pub fn open(path: &Path, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        let conn =
            crate::db::open_connection(path).map_err(|error| format!("playback db: {error}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            ids,
        })
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, rusqlite::Connection>> {
        self.conn.lock().ok()
    }
}

/// Attempts at one play write while the database stays busy.
const PLAY_WRITE_ATTEMPTS: u32 = 3;
/// Pause between busy retries of a play write.
const PLAY_WRITE_BACKOFF: Duration = Duration::from_millis(200);

/// Run blocking SQLite work. On a multi-threaded runtime the worker hands
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

impl TrackCatalog for PlaybackDb {
    fn get_track(&self, track_id: &str) -> Option<TrackInfo> {
        off_worker(|| self.read_track(track_id))
    }
}

impl PlaybackDb {
    fn read_track(&self, track_id: &str) -> Option<TrackInfo> {
        let guard = self.lock()?;
        let mut query = guard
            .prepare(
                "SELECT id, title, artist_name, album_title, duration_seconds, \
                 embedded_recording_mbid FROM local_tracks WHERE id = ?1",
            )
            .ok()?;
        let mut rows = query.query([track_id]).ok()?;
        let row = rows.next().ok()??;
        let duration_ms = row
            .get::<_, Option<f64>>("duration_seconds")
            .ok()?
            .map(|secs| (secs * 1000.0) as i64)
            .unwrap_or(0);
        Some(TrackInfo {
            track_id: row.get("id").ok()?,
            title: row.get("title").ok()?,
            artist_name: row
                .get::<_, Option<String>>("artist_name")
                .ok()?
                .unwrap_or_default(),
            album_title: row.get("album_title").ok()?,
            duration_ms,
            recording_mbid: row
                .get::<_, Option<String>>("embedded_recording_mbid")
                .ok()?
                .filter(|mbid| !mbid.is_empty()),
            // Release-group MBIDs live in the library identity tables, which
            // this store does not read yet, so presence covers stay empty.
            rg_mbid: None,
        })
    }
}

impl PlayHistory for PlaybackDb {
    fn record(&self, user_id: &str, record: &PlayRecord) {
        off_worker(|| self.write_play(user_id, record));
    }
}

impl PlaybackDb {
    /// Insert one play, retrying while SQLite reports busy. A play that
    /// still fails is logged with what it was, never dropped silently.
    fn write_play(&self, user_id: &str, record: &PlayRecord) {
        let id = self.ids.new_id();
        let mut attempt = 1;
        let outcome = loop {
            let Some(guard) = self.lock() else {
                break Err("playback db lock poisoned".to_owned());
            };
            let result = guard.execute(
                "INSERT INTO library_play_history \
                 (id, user_id, track_name, artist_name, album_name, recording_mbid, \
                  release_group_mbid, duration_ms, source, played_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    user_id,
                    record.track_name,
                    record.artist_name,
                    record.album_name,
                    record.recording_mbid,
                    record.release_group_mbid,
                    record.duration_ms,
                    record.source,
                    to_iso(record.played_at),
                ],
            );
            drop(guard);
            match result {
                Ok(_) => break Ok(()),
                Err(error)
                    if crate::db::rusqlite_is_busy(&error) && attempt < PLAY_WRITE_ATTEMPTS =>
                {
                    tracing::warn!(attempt, %error, "play history busy; retrying");
                    attempt += 1;
                    std::thread::sleep(PLAY_WRITE_BACKOFF);
                }
                Err(error) => break Err(error.to_string()),
            }
        };
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

impl ListeningPrefs for PlaybackDb {
    fn scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs {
        off_worker(|| self.read_scrobble_prefs(user_id))
    }

    fn visibility(&self, user_id: &str) -> Result<String, ProviderFailure> {
        off_worker(|| self.read_visibility(user_id))
    }
}

impl PlaybackDb {
    fn read_scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs {
        let defaults = || ScrobblePrefs {
            scrobble_to_lastfm: false,
            scrobble_to_listenbrainz: false,
            navidrome_handles_external: true,
        };
        let Some(guard) = self.lock() else {
            return defaults();
        };
        let mut query = match guard.prepare(
            "SELECT scrobble_to_lastfm, scrobble_to_listenbrainz, \
             navidrome_handles_external_scrobbles FROM user_listening_prefs \
             WHERE user_id = ?1",
        ) {
            Ok(query) => query,
            Err(_) => return defaults(),
        };
        let mut rows = match query.query([user_id]) {
            Ok(rows) => rows,
            Err(_) => return defaults(),
        };
        let Ok(Some(row)) = rows.next() else {
            // No row yet: the column defaults are the right answer.
            return defaults();
        };
        ScrobblePrefs {
            scrobble_to_lastfm: row.get::<_, i64>(0).unwrap_or(0) != 0,
            scrobble_to_listenbrainz: row.get::<_, i64>(1).unwrap_or(0) != 0,
            navidrome_handles_external: row.get::<_, i64>(2).unwrap_or(1) != 0,
        }
    }

    fn read_visibility(&self, user_id: &str) -> Result<String, ProviderFailure> {
        let Some(guard) = self.lock() else {
            return Err(ProviderFailure("prefs store unavailable".to_owned()));
        };
        let mut query = guard
            .prepare("SELECT now_playing_visibility FROM user_listening_prefs WHERE user_id = ?1")
            .map_err(|error| ProviderFailure(error.to_string()))?;
        let mut rows = query
            .query([user_id])
            .map_err(|error| ProviderFailure(error.to_string()))?;
        match rows.next() {
            Ok(Some(row)) => Ok(row
                .get::<_, String>(0)
                .unwrap_or_else(|_| VISIBILITY_FULL.to_owned())),
            Ok(None) => Ok(VISIBILITY_FULL.to_owned()),
            Err(error) => Err(ProviderFailure(error.to_string())),
        }
    }
}

impl ScrobbleLinks for PlaybackDb {
    fn linked(&self, user_id: &str) -> ScrobbleTargets {
        off_worker(|| self.read_links(user_id))
    }
}

impl PlaybackDb {
    /// Linked scrobble services from `user_connections`: a ListenBrainz
    /// row (its document is sealed whole, so presence is the signal) and
    /// a Last.fm row whose plain document carries a session key. A failed
    /// read logs and reads as unlinked.
    fn read_links(&self, user_id: &str) -> ScrobbleTargets {
        let mut targets = ScrobbleTargets::default();
        let Some(guard) = self.lock() else {
            return targets;
        };
        let rows = guard
            .prepare(
                "SELECT service, connection_data FROM user_connections \
                 WHERE user_id = ?1 AND enabled = 1 AND service IN ('lastfm', 'listenbrainz')",
            )
            .and_then(|mut query| {
                query
                    .query_map([user_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()
            });
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "scrobble link read failed; forwarding nothing");
                return targets;
            }
        };
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
        targets
    }
}

impl DisplayNames for PlaybackDb {
    fn display_name(&self, user_id: &str) -> String {
        off_worker(|| self.read_display_name(user_id))
    }
}

impl PlaybackDb {
    fn read_display_name(&self, user_id: &str) -> String {
        let Some(guard) = self.lock() else {
            return user_id.to_owned();
        };
        let mut query =
            match guard.prepare("SELECT display_name, username FROM auth_users WHERE id = ?1") {
                Ok(query) => query,
                Err(_) => return user_id.to_owned(),
            };
        let mut rows = match query.query([user_id]) {
            Ok(rows) => rows,
            Err(_) => return user_id.to_owned(),
        };
        let Ok(Some(row)) = rows.next() else {
            return user_id.to_owned();
        };
        let display: String = row.get(0).unwrap_or_default();
        if !display.trim().is_empty() {
            return display;
        }
        row.get::<_, Option<String>>(1)
            .ok()
            .flatten()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| user_id.to_owned())
    }
}
