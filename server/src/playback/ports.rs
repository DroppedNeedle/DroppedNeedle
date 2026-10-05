//! Ports behind playback reporting.
//!
//! Services reach the outside world through these traits: the catalog for
//! track resolution, the scrobble sinks for Last.fm/ListenBrainz forwarding,
//! the remote reporters for Jellyfin/Navidrome/Plex session attribution, the
//! history store for the local source of truth, and the prefs for per-user
//! forwarding and visibility choices. Tests run these on fakes (see
//! `fakes.rs`); production binds the SQLite stores in `sqlite.rs` behind
//! the same traits. Every fallible method returns a plain
//! string cause: provider detail stays in the log, never on the wire.

use std::collections::HashMap;

/// Failure talking to a provider. The string is a log-only cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure(pub String);

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Clock seam. Sessions, presence TTLs, and scrobble timestamps read time
/// through this so tests pin it.
pub trait Clock: Send + Sync {
    /// Current unix timestamp in seconds.
    fn now_unix(&self) -> i64;
}

/// System clock for production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0)
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

/// Catalog lookups for reporting. Unknown ids resolve to `None` (the
/// handler 404s); the catalog itself never fails a report.
pub trait TrackCatalog: Send + Sync {
    /// Resolve one track for reporting.
    fn get_track(&self, track_id: &str) -> Option<TrackInfo>;
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

/// Outcome of one external forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceOutcome {
    /// True when that service accepted the report.
    pub success: bool,
    /// Failure text, when the service refused or errored.
    pub error: Option<String>,
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
}

/// External scrobble sinks (Last.fm / ListenBrainz). Sinks forward only to
/// the `targets` asked for, and only for accounts the user linked. The map
/// key is the service name; an empty map means no linked account, not a
/// failure.
pub trait ScrobbleSinks: Send + Sync {
    /// Forward a now-playing report for the user's linked accounts.
    fn report_now_playing(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> HashMap<String, ServiceOutcome>;
    /// Forward one scrobble for the user's linked accounts.
    fn submit_scrobble(
        &self,
        user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> HashMap<String, ServiceOutcome>;
}

/// Which scrobble services a user has linked. Synchronous like the other
/// playback ports; a failed read reads as unlinked.
pub trait ScrobbleLinks: Send + Sync {
    /// The user's linked services.
    fn linked(&self, user_id: &str) -> ScrobbleTargets;
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
/// their own server), so production no-ops those sources.
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

/// Local play-history store. Every accepted play lands here regardless of
/// external linkage (v2 `PlayHistoryStore` rule).
pub trait PlayHistory: Send + Sync {
    /// Record one play for the user.
    fn record(&self, user_id: &str, record: &PlayRecord);
}

/// Per-user external-forwarding choices (v2 listening prefs, scrobble half).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrobblePrefs {
    /// Forward to the user's linked Last.fm account.
    pub scrobble_to_lastfm: bool,
    /// Forward to the user's linked ListenBrainz account.
    pub scrobble_to_listenbrainz: bool,
    /// Navidrome owns external scrobbles: skip our forwarding for
    /// Navidrome-sourced plays (history still records).
    pub navidrome_handles_external: bool,
}

/// Full track detail for other listeners.
pub const VISIBILITY_FULL: &str = "full";
/// Identity and progress only; the song stays hidden.
pub const VISIBILITY_TRACK_HIDDEN: &str = "track_hidden";
/// Absent from the shared feed entirely.
pub const VISIBILITY_OFFLINE: &str = "offline";

/// Per-user listening preferences: forwarding toggles plus the presence
/// visibility behind the shared feed.
pub trait ListeningPrefs: Send + Sync {
    /// External-forwarding toggles for the user.
    fn scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs;
    /// Presence visibility for the user. Fallible so a transient store
    /// error redacts fail-closed instead of failing the report (v2
    /// `NowPlayingService._load_visibility` rule).
    fn visibility(&self, user_id: &str) -> Result<String, ProviderFailure>;
}

/// Display-name resolution for presence entries. Kept behind a port so the
/// playback code never joins the user store itself.
pub trait DisplayNames: Send + Sync {
    /// Display name for the user.
    fn display_name(&self, user_id: &str) -> String;
}
