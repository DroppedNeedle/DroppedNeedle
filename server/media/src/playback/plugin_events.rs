//! Accepted plays reach the plugins: the plugin side of playback's play
//! events port.

use std::sync::Arc;

use super::ports::{PlayEvents, ReportTrack};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::runtime::{
    EventKind, EventPayload, PlaybackEvent, PluginEvent, ScrobbleEvent,
};

/// Accepted plays from playback, as plugin events: a `scrobble` event
/// (which also reaches `scrobbler` plugins) and a `playback_started`
/// event. Both are sent from this one place, as one delivery per plugin,
/// so the second never finds a plugin busy with the first.
pub struct PluginPlayEvents(pub Arc<PluginHost>);

impl PlayEvents for PluginPlayEvents {
    fn play_accepted(&self, user_id: &str, report: &ReportTrack, played_at: i64) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let causation = uuid::Uuid::new_v4().simple().to_string();
        let scrobble = PluginEvent {
            kind: EventKind::Scrobble,
            payload: EventPayload::Scrobble(ScrobbleEvent {
                artist: report.artist_name.clone(),
                track: report.track_name.clone(),
                album: report.album_name.clone(),
                timestamp: played_at,
                duration_ms: Some(report.duration_ms).filter(|duration| *duration > 0),
                recording_mbid: report.mbid.clone(),
            }),
            causation_id: causation.clone(),
        };
        let started = PluginEvent {
            kind: EventKind::PlaybackStarted,
            payload: EventPayload::Playback(PlaybackEvent {
                artist: report.artist_name.clone(),
                track: report.track_name.clone(),
                album: report.album_name.clone(),
                user_id: user_id.to_owned(),
            }),
            causation_id: format!("{causation}-playback"),
        };
        self.0.dispatch_events(vec![scrobble, started]);
    }
}
