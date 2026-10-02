//! Compat playback: scrobbles and presence over the stage-6 reporting
//! services.
//!
//! Both protocols share one adapter. Scrobbles go through
//! [`submit_scrobble`](crate::playback::services::submit_scrobble) (history
//! records, sinks forward per prefs); presence goes through
//! [`heartbeat`](crate::playback::services::heartbeat) /
//! [`clear_presence`](crate::playback::services::clear_presence); the
//! now-playing list reads
//! [`PresenceRegistry::compat_now_playing`](crate::playback::services::PresenceRegistry::compat_now_playing),
//! which already skips redacted and trackless rows. Failures never fail a
//! report: unknown files resolve to nothing and service errors map to the
//! caller's store error (code 0 on Subsonic).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::compat::jellyfin::seams::PlaybackSessions;
use crate::compat::subsonic::store::NowPlayingRow;
use crate::playback::models::{
    NowPlayingHeartbeat, ScrobbleNowPlayingRequest, ScrobbleSubmitRequest,
};
use crate::playback::services::{
    PlaybackDeps, clear_presence, forward_sink_now_playing, heartbeat, normalize_client,
    submit_scrobble,
};

/// Reporting adapter shared by both compat protocols.
#[derive(Clone)]
pub struct CompatPlayback {
    deps: PlaybackDeps,
    started: Arc<Mutex<HashSet<(String, String)>>>,
}

impl CompatPlayback {
    /// Wrap the stage-6 reporting deps.
    pub fn new(deps: PlaybackDeps) -> Self {
        Self {
            deps,
            started: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Presence source: the normalized client, defaulting to `compat`
    /// (v2 `_write_presence`: `source=_norm_client(client)`).
    fn source(client: Option<&str>) -> String {
        normalize_client(client).unwrap_or_else(|| "compat".to_owned())
    }

    /// Record now-playing presence for a file. Unknown files are skipped
    /// (no names to project); sink forwarding is best-effort. The device is
    /// always `compat`: the Jellyfin progress seam carries no client, so a
    /// per-client device would split one listener across two presence rows;
    /// the client rides `source` instead (v2 parity).
    pub fn now_playing(&self, file_id: &str, user_id: &str, client: Option<&str>) {
        let Some(track) = self.deps.catalog.get_track(file_id) else {
            return;
        };
        heartbeat(
            &self.deps,
            user_id,
            &NowPlayingHeartbeat {
                track_name: track.title.clone(),
                artist_name: track.artist_name.clone(),
                album_name: Some(track.album_title.clone()).filter(|name| !name.is_empty()),
                cover_url: String::new(),
                source: Self::source(client),
                device: "compat".to_owned(),
                is_paused: false,
                progress_ms: None,
                duration_ms: Some(track.duration_ms).filter(|duration| *duration > 0),
            },
        );
        let _ = forward_sink_now_playing(
            &self.deps,
            user_id,
            &ScrobbleNowPlayingRequest {
                track_name: track.title,
                artist_name: track.artist_name,
                album_name: Some(track.album_title).filter(|name| !name.is_empty()),
                duration_ms: Some(track.duration_ms).filter(|duration| *duration > 0),
                mbid: track.recording_mbid,
                release_group_mbid: track.rg_mbid,
                source: Some("local".to_owned()),
            },
        );
    }

    /// Record a scrobble. Unknown files are skipped; played_at defaults to
    /// now. Returns false when the service refused the report.
    pub fn scrobble(&self, file_id: &str, user_id: &str, played_at: Option<f64>) -> bool {
        let Some(track) = self.deps.catalog.get_track(file_id) else {
            return true;
        };
        let now = self.deps.clock.now_unix();
        let timestamp = played_at.map(|at| at as i64).unwrap_or(now);
        submit_scrobble(
            &self.deps,
            user_id,
            &ScrobbleSubmitRequest {
                track_name: track.title,
                artist_name: track.artist_name,
                timestamp,
                album_name: Some(track.album_title).filter(|name| !name.is_empty()),
                duration_ms: Some(track.duration_ms).filter(|duration| *duration > 0),
                mbid: track.recording_mbid,
                release_group_mbid: track.rg_mbid,
                source: Some("local".to_owned()),
            },
        )
        .is_ok()
    }

    /// Subsonic playback report: starting/playing/paused refresh presence,
    /// stopped drops it and scrobbles unless ignored.
    pub fn report_playback(
        &self,
        file_id: &str,
        user_id: &str,
        client: &str,
        state: &str,
        ignore_scrobble: bool,
    ) -> bool {
        if state == "stopped" {
            clear_presence(&self.deps, user_id, "compat");
            if ignore_scrobble {
                return true;
            }
            return self.scrobble(file_id, user_id, None);
        }
        self.now_playing(file_id, user_id, Some(client));
        true
    }

    /// Live presence rows servable over compat (file id + update time
    /// included, redacted and trackless rows already skipped).
    pub fn compat_rows(&self) -> Vec<NowPlayingRow> {
        self.deps
            .presence
            .compat_now_playing()
            .into_iter()
            .map(|(entry, file_id, updated_at)| NowPlayingRow {
                user_name: entry.user_name,
                file_id,
                updated_at: updated_at as f64,
                source: Some(entry.source),
                device_name: Some(entry.device_name),
            })
            .collect()
    }

    fn mark_started(&self, user_id: &str, key: &str) {
        if let Ok(mut started) = self.started.lock() {
            started.insert((user_id.to_owned(), key.to_owned()));
        }
    }

    fn pop_started(&self, user_id: &str, key: &str) -> Option<String> {
        self.started.lock().ok().and_then(|mut started| {
            started
                .remove(&(user_id.to_owned(), key.to_owned()))
                .then(|| "started".to_owned())
        })
    }
}

impl PlaybackSessions for CompatPlayback {
    async fn mark_started(&self, user_id: &str, key: &str) {
        self.mark_started(user_id, key);
    }

    async fn pop_started(&self, user_id: &str, key: &str) -> Option<String> {
        self.pop_started(user_id, key)
    }

    async fn now_playing(&self, user_id: &str, file_id: &str, client: Option<&str>) {
        self.now_playing(file_id, user_id, client);
    }

    async fn progress(
        &self,
        user_id: &str,
        file_id: &str,
        _position_ms: Option<i64>,
        _paused: bool,
    ) {
        // Heartbeat refresh keeps presence alive; the position rides the
        // next stop report's threshold check, never a scrobble here.
        self.now_playing(file_id, user_id, None);
    }

    async fn clear_presence(&self, user_id: &str, _client: Option<&str>) {
        clear_presence(&self.deps, user_id, "compat");
    }

    async fn scrobble(&self, user_id: &str, file_id: &str, _client: Option<&str>) {
        let _ = self.scrobble(file_id, user_id, None);
    }
}
