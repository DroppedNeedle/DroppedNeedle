//! Production playback ports over the SQLite schema.
//!
//! The reporting traits are synchronous, so these stores open their own
//! rusqlite handle on the runtime database instead of sharing the async
//! pool: catalog reads hit `local_tracks`, plays land in
//! `library_play_history`, prefs read `user_listening_prefs`, and display
//! names resolve from `auth_users`. Every failure is fail-closed (misses
//! and defaults, never an error to the player), matching the
//! never-fail-the-player rule.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auth::times::to_iso;
use crate::ids::IdGenerator;

use super::ports::{
    DisplayNames, ListeningPrefs, PlayHistory, PlayRecord, ProviderFailure, ScrobblePrefs,
    TrackCatalog, TrackInfo, VISIBILITY_FULL,
};

/// Production playback stores over one rusqlite handle.
pub struct PlaybackDb {
    conn: Mutex<rusqlite::Connection>,
    ids: Arc<dyn IdGenerator>,
}

impl PlaybackDb {
    /// Open the runtime database for playback reads and writes. The lock
    /// wait mirrors the pool's busy timeout so history writes never wedge
    /// behind the writer lane.
    pub fn open(path: &Path, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        let conn =
            rusqlite::Connection::open(path).map_err(|error| format!("playback db: {error}"))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("playback db: {error}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            ids,
        })
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, rusqlite::Connection>> {
        self.conn.lock().ok()
    }
}

impl TrackCatalog for PlaybackDb {
    fn get_track(&self, track_id: &str) -> Option<TrackInfo> {
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
            // Release-group MBIDs live in the identity tables, which stage
            // 8 owns; presence covers stay empty until then.
            rg_mbid: None,
        })
    }
}

impl PlayHistory for PlaybackDb {
    fn record(&self, user_id: &str, record: &PlayRecord) {
        let Some(guard) = self.lock() else {
            return;
        };
        let outcome = guard.execute(
            "INSERT INTO library_play_history \
             (id, user_id, track_name, artist_name, album_name, recording_mbid, \
              release_group_mbid, duration_ms, source, played_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                self.ids.new_id(),
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
        if let Err(error) = outcome {
            tracing::warn!(%error, "play history write failed; continuing");
        }
    }
}

impl ListeningPrefs for PlaybackDb {
    fn scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs {
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

    fn visibility(&self, user_id: &str) -> Result<String, ProviderFailure> {
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

impl DisplayNames for PlaybackDb {
    fn display_name(&self, user_id: &str) -> String {
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
