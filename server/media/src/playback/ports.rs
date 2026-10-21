//! Ports and shared types of playback reporting.
//!
//! Only what lies outside the process sits behind a trait here: the
//! scrobble sinks (Last.fm and ListenBrainz), the remote reporters (the
//! user's media servers) and the play listeners (the plugins host, another
//! area). The database side is the concrete store in `sqlite.rs`. Every
//! fallible method returns a plain string cause: provider detail stays in
//! the log, never on the wire.

use super::forwarding::Service;

/// Failure talking to a provider. The string is a log-only cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure(pub String);

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One catalog track, resolved for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// Catalog track id.
    pub track_id: String,
    /// Track title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// Track length in milliseconds.
    pub duration_ms: i64,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Release-group MBID, when known.
    pub rg_mbid: Option<String>,
}

/// One play, by name, for external forwarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportTrack {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Track length in milliseconds (0 when unknown).
    pub duration_ms: i64,
    /// Recording MBID, when known.
    pub mbid: Option<String>,
    /// Release-group MBID, when known.
    pub release_group_mbid: Option<String>,
    /// Normalized reporting source, when known.
    pub source: Option<String>,
    /// Play time, unix seconds (`None` for now-playing forwards).
    pub played_at: Option<i64>,
}

/// Which external services one forward may go to, from the user's
/// scrobble preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScrobbleTargets {
    /// Forward to Last.fm.
    pub lastfm: bool,
    /// Forward to ListenBrainz.
    pub listenbrainz: bool,
}

impl ScrobbleTargets {
    /// True when no service is wanted.
    pub fn is_empty(self) -> bool {
        !self.lastfm && !self.listenbrainz
    }

    /// Whether `service` is wanted.
    pub fn wants(self, service: Service) -> bool {
        match service {
            Service::LastFm => self.lastfm,
            Service::ListenBrainz => self.listenbrainz,
        }
    }

    /// The services both sets want.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self {
            lastfm: self.lastfm && other.lastfm,
            listenbrainz: self.listenbrainz && other.listenbrainz,
        }
    }
}

/// External now-playing sinks (Last.fm / ListenBrainz). A report goes only
/// to the `targets` asked for, and only to accounts the user linked; the
/// answer is the services it was handed to. Scrobbles do not come here:
/// they wait as jobs written with the play. A trait because it stands for
/// two outside services.
pub trait ScrobbleSinks: Send + Sync {
    /// Hand a now-playing report on for the user's linked accounts.
    fn report_now_playing(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> ScrobbleTargets;
}

/// One outbound session report to a remote music server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteReport {
    /// Owning user id (per-user attribution, fail closed).
    pub user_id: String,
    /// Remote item id (track id as the remote knows it).
    pub item_id: String,
    /// Reporting device slug.
    pub device: String,
    /// Position in milliseconds, when known.
    pub position_ms: Option<i64>,
    /// True while paused (progress only).
    pub is_paused: bool,
}

/// Outbound session attribution to Jellyfin/Navidrome/Plex. Local and
/// YouTube plays have no upstream (v2 only ever reports remote plays to
/// their own server), so production no-ops those sources. A trait because
/// it stands for the user's media servers, outside the process.
pub trait RemoteReporters: Send + Sync {
    /// Report a playback start to `source`.
    fn report_start(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure>;
    /// Report playback progress to `source`.
    fn report_progress(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure>;
    /// Report a playback stop to `source`.
    fn report_stop(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure>;
    /// Count one scrobble on `source`.
    fn scrobble(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure>;
}

/// One recorded play: the local source of truth (v2 `play_history` row,
/// with the played-at instant as unix seconds instead of ISO text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayRecord {
    /// The library track that was played, when the server resolved it
    /// itself. Never taken from a request body: a client must not name the
    /// row a play is filed under.
    pub track_id: Option<String>,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Release-group MBID, when known.
    pub release_group_mbid: Option<String>,
    /// Track length in milliseconds, when known.
    pub duration_ms: Option<i64>,
    /// Normalized reporting source, when known.
    pub source: Option<String>,
    /// Play time, unix seconds.
    pub played_at: i64,
}

/// Listeners for accepted plays (plugins). Called after dedup, the
/// short-track gate and Navidrome delegation, whatever the user's Last.fm
/// and ListenBrainz choices are. Must not block: implementations hand the
/// play off and return. A trait because the listener is the plugins host,
/// another area.
pub trait PlayEvents: Send + Sync {
    /// One play was accepted for `user_id`.
    fn play_accepted(&self, user_id: &str, report: &ReportTrack, played_at: i64);
}

/// No listeners.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPlayEvents;

impl PlayEvents for NoPlayEvents {
    fn play_accepted(&self, _user_id: &str, _report: &ReportTrack, _played_at: i64) {}
}

/// Full track detail for other listeners.
pub const VISIBILITY_FULL: &str = "full";
/// Identity and progress only; the song stays hidden.
pub const VISIBILITY_TRACK_HIDDEN: &str = "track_hidden";
/// Absent from the shared feed entirely.
pub const VISIBILITY_OFFLINE: &str = "offline";
