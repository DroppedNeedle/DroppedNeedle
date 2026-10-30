//! Compat playback: scrobbles and presence over the playback reporting
//! services.
//!
//! Both protocols share one adapter. Scrobbles go through
//! [`submit_track_scrobble`](droppedneedle_media::playback::report::submit_track_scrobble)
//! (history files the play under the track, sinks forward per prefs);
//! presence goes through
//! [`heartbeat`](droppedneedle_media::playback::presence::heartbeat) /
//! [`clear_presence`](droppedneedle_media::playback::presence::clear_presence); the
//! now-playing list reads
//! [`PresenceRegistry::compat_now_playing`](droppedneedle_media::playback::presence::PresenceRegistry::compat_now_playing),
//! which already skips redacted and trackless rows. Failures never fail a
//! report: unknown files resolve to nothing and service errors map to the
//! caller's store error (code 0 on Subsonic).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::subsonic::records::NowPlayingRow;
use droppedneedle_media::playback::PlaybackDeps;
use droppedneedle_media::playback::models::{
    NowPlayingHeartbeat, ScrobbleNowPlayingRequest, ScrobbleSubmitRequest,
};
use droppedneedle_media::playback::ports::TrackInfo;
use droppedneedle_media::playback::presence::{
    PRESENCE_TTL_SECS, clear_presence, compat_heartbeat,
};
use droppedneedle_media::playback::report::{forward_sink_now_playing, submit_track_scrobble};
use droppedneedle_media::playback::rules::{normalize_client, presence_cover_url};

/// The longest a Subsonic report holds presence for: past it a listener
/// that never said `stopped` is gone.
pub const MAX_PRESENCE_HOLD_SECS: i64 = 2 * 60 * 60;

/// Which protocol a report came in on. They report differently, so presence
/// and the scrobble services' now-playing follow each one's rhythm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporter {
    /// Subsonic: a report on start, pause, resume and seek, and no
    /// heartbeat. Presence lasts for the rest of the track, and the scrobble
    /// services hear about a track once.
    Subsonic,
    /// Jellyfin: a progress report every few seconds. Presence lasts
    /// [`PRESENCE_TTL_SECS`] past each report, and every start is forwarded.
    Jellyfin,
}

/// A track the scrobble services were told is playing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Forwarded {
    file_id: String,
    at: i64,
}

/// Reporting adapter shared by both compat protocols.
#[derive(Clone)]
pub struct CompatPlayback {
    deps: PlaybackDeps,
    started: Arc<Mutex<HashSet<(String, String)>>>,
    /// Per user, the last track a Subsonic report forwarded as now playing.
    forwarded: Arc<Mutex<HashMap<String, Forwarded>>>,
}

impl CompatPlayback {
    /// Wrap the playback reporting deps.
    pub fn new(deps: PlaybackDeps) -> Self {
        Self {
            deps,
            started: Arc::new(Mutex::new(HashSet::new())),
            forwarded: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Presence source: the normalized client, defaulting to `compat`.
    fn source(client: Option<&str>) -> String {
        normalize_client(client).unwrap_or_else(|| "compat".to_owned())
    }

    /// Record now-playing presence for a file, with the position and paused
    /// state its report carries (`None` and `false` on a start), and tell
    /// the user's scrobble services it is playing (best-effort). A paused
    /// report never tells them; a Subsonic report tells them once per track
    /// ([`Self::first_report_of_track`]). Unknown files are skipped (no
    /// names to project).
    pub fn now_playing(
        &self,
        reporter: Reporter,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        position_ms: Option<i64>,
        paused: bool,
    ) {
        let Some(track) =
            self.presence_for(reporter, file_id, user_id, client, position_ms, paused)
        else {
            return;
        };
        if paused
            || (reporter == Reporter::Subsonic
                && !self.first_report_of_track(user_id, file_id, track.duration_ms))
        {
            return;
        }
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

    /// Whether this report starts a play the scrobble services have not
    /// heard of: another track than the one last forwarded for the user, or
    /// the same one after its length has passed (repeat one, or a resume
    /// after a long pause; their now-playing has run out by then). With no
    /// known length only a track change counts. Notes the forward.
    fn first_report_of_track(&self, user_id: &str, file_id: &str, duration_ms: i64) -> bool {
        let now = self.deps.clock.now_unix();
        let Ok(mut forwarded) = self.forwarded.lock() else {
            return true;
        };
        let again = match forwarded.get(user_id) {
            Some(last) if last.file_id == file_id => {
                duration_ms > 0 && now - last.at >= (duration_ms + 999) / 1000
            }
            _ => true,
        };
        if again {
            forwarded.insert(
                user_id.to_owned(),
                Forwarded {
                    file_id: file_id.to_owned(),
                    at: now,
                },
            );
        }
        again
    }

    /// Update presence only, for a progress report: the scrobble services
    /// heard about the play when it started.
    pub fn progress(
        &self,
        reporter: Reporter,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        position_ms: Option<i64>,
        paused: bool,
    ) {
        self.presence_for(reporter, file_id, user_id, client, position_ms, paused);
    }

    /// How long a report keeps its presence entry. A Subsonic report of a
    /// playing track with a known length holds it for the rest of the track
    /// plus [`PRESENCE_TTL_SECS`] (a start or a report without a position
    /// counts from 0), at most [`MAX_PRESENCE_HOLD_SECS`]. Anything else,
    /// a pause included, holds it for [`PRESENCE_TTL_SECS`]: a paused
    /// listener is not playing, and the resume reports again.
    fn hold_secs(
        reporter: Reporter,
        duration_ms: i64,
        position_ms: Option<i64>,
        paused: bool,
    ) -> i64 {
        if reporter != Reporter::Subsonic || paused || duration_ms <= 0 {
            return PRESENCE_TTL_SECS;
        }
        let remaining_ms = (duration_ms - position_ms.unwrap_or(0).max(0)).max(0);
        ((remaining_ms + 999) / 1000 + PRESENCE_TTL_SECS).min(MAX_PRESENCE_HOLD_SECS)
    }

    /// Write the presence entry and hand back the resolved track. The
    /// device is always `compat`, so one listener has one entry whichever
    /// report came last; the client rides `source`.
    fn presence_for(
        &self,
        reporter: Reporter,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        position_ms: Option<i64>,
        paused: bool,
    ) -> Option<TrackInfo> {
        let track = self.deps.store.get_track(file_id)?;
        compat_heartbeat(
            &self.deps,
            user_id,
            &NowPlayingHeartbeat {
                track_name: track.title.clone(),
                artist_name: track.artist_name.clone(),
                album_name: Some(track.album_title.clone()).filter(|name| !name.is_empty()),
                cover_url: presence_cover_url(&track),
                source: Self::source(client),
                device: "compat".to_owned(),
                is_paused: paused,
                progress_ms: position_ms,
                duration_ms: Some(track.duration_ms).filter(|duration| *duration > 0),
            },
            &track.track_id,
            Self::hold_secs(reporter, track.duration_ms, position_ms, paused),
        );
        Some(track)
    }

    /// Record a scrobble. Unknown files are skipped; played_at defaults to
    /// now. Returns false when the service refused the report.
    pub fn scrobble(&self, file_id: &str, user_id: &str, played_at: Option<f64>) -> bool {
        let Some(track) = self.deps.store.get_track(file_id) else {
            return true;
        };
        let now = self.deps.clock.now_unix();
        let timestamp = played_at.map(|at| at as i64).unwrap_or(now);
        submit_track_scrobble(
            &self.deps,
            user_id,
            &track.track_id,
            &ScrobbleSubmitRequest {
                track_name: track.title,
                artist_name: track.artist_name,
                timestamp,
                album_name: Some(track.album_title).filter(|name| !name.is_empty()),
                duration_ms: Some(track.duration_ms).filter(|duration| *duration > 0),
                mbid: track.recording_mbid,
                release_group_mbid: track.rg_mbid,
                source: Some("local".to_owned()),
                track_id: None,
            },
        )
        .is_ok()
    }

    /// Subsonic playback report: starting sets presence without a position,
    /// playing and paused set it with the reported position, stopped drops
    /// it and scrobbles unless ignored.
    pub fn report_playback(
        &self,
        file_id: &str,
        user_id: &str,
        client: &str,
        state: &str,
        position_ms: i64,
        ignore_scrobble: bool,
    ) -> bool {
        if state == "stopped" {
            clear_presence(&self.deps, user_id, "compat");
            if let Ok(mut forwarded) = self.forwarded.lock() {
                forwarded.remove(user_id);
            }
            if ignore_scrobble {
                return true;
            }
            return self.scrobble(file_id, user_id, None);
        }
        let position = (state != "starting").then_some(position_ms);
        self.now_playing(
            Reporter::Subsonic,
            file_id,
            user_id,
            Some(client),
            position,
            state == "paused",
        );
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

    /// Drop the caller's presence (a Jellyfin stop report).
    pub fn clear_presence(&self, user_id: &str) {
        clear_presence(&self.deps, user_id, "compat");
    }

    /// Note that the caller started the play keyed `key` (a Jellyfin
    /// session or item id).
    pub fn mark_started(&self, user_id: &str, key: &str) {
        if let Ok(mut started) = self.started.lock() {
            started.insert((user_id.to_owned(), key.to_owned()));
        }
    }

    /// Forget the play keyed `key`; `Some` when it was started.
    pub fn pop_started(&self, user_id: &str, key: &str) -> Option<String> {
        self.started.lock().ok().and_then(|mut started| {
            started
                .remove(&(user_id.to_owned(), key.to_owned()))
                .then(|| "started".to_owned())
        })
    }
}
