//! Fake ports for stage 6: deterministic catalog, sinks, remotes, and
//! stores over in-memory state. Later slices swap these for real providers
//! behind the same traits; services never know.
//!
//! Fakes fail only when armed to: the `fail_*` methods take a log-only cause
//! the leak briefs assert never reaches the wire.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use super::{
    ports::{
        Clock, DisplayNames, ListeningPrefs, PlayHistory, PlayRecord, ProviderFailure,
        RemoteReport, RemoteReporters, ReportTrack, ScrobblePrefs, ScrobbleSinks, ServiceOutcome,
        TrackCatalog, TrackInfo, VISIBILITY_FULL,
    },
    warmup::{WarmupScope, WarmupStats},
};

/// Manual clock for TTL and timestamp tests. Starts at `start`, moves only
/// when told.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<i64>>,
}

impl ManualClock {
    /// Build a clock pinned at `start` (unix seconds).
    pub fn new(start: i64) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Move the clock forward by `secs`.
    pub fn advance(&self, secs: i64) {
        if let Ok(mut now) = self.now.lock() {
            *now += secs;
        }
    }
}

impl Clock for ManualClock {
    fn now_unix(&self) -> i64 {
        self.now.lock().map(|now| *now).unwrap_or(0)
    }
}

/// Scripted catalog: tracks the briefs register, misses for the rest.
#[derive(Debug, Default)]
pub struct FakeCatalog {
    tracks: Mutex<HashMap<String, TrackInfo>>,
}

impl FakeCatalog {
    /// Build a catalog serving `tracks`.
    pub fn with_tracks(tracks: Vec<TrackInfo>) -> Self {
        let indexed = tracks
            .into_iter()
            .map(|track| (track.track_id.clone(), track))
            .collect();
        Self {
            tracks: Mutex::new(indexed),
        }
    }

    /// One four-minute library track with MBIDs, the briefs' workhorse.
    pub fn sample_track() -> TrackInfo {
        TrackInfo {
            track_id: "track-1".to_owned(),
            title: "Roads".to_owned(),
            artist_name: "Portishead".to_owned(),
            album_title: "Dummy".to_owned(),
            duration_ms: 240_000,
            recording_mbid: Some("recording-mbid-1".to_owned()),
            rg_mbid: Some("rg-mbid-1".to_owned()),
        }
    }

    /// One twenty-second short track (under the 30s forward gate).
    pub fn short_track() -> TrackInfo {
        TrackInfo {
            track_id: "track-short".to_owned(),
            title: "Interlude".to_owned(),
            artist_name: "Portishead".to_owned(),
            album_title: "Dummy".to_owned(),
            duration_ms: 20_000,
            recording_mbid: None,
            rg_mbid: None,
        }
    }
}

impl TrackCatalog for FakeCatalog {
    fn get_track(&self, track_id: &str) -> Option<TrackInfo> {
        self.tracks
            .lock()
            .map(|tracks| tracks.get(track_id).cloned())
            .unwrap_or(None)
    }
}

/// Scripted sinks: record every forward, answer per armed outcome.
#[derive(Debug, Default)]
pub struct FakeSinks {
    now_playing_calls: Mutex<Vec<(String, ReportTrack)>>,
    scrobble_calls: Mutex<Vec<(String, ReportTrack)>>,
    outcomes: Mutex<HashMap<String, ServiceOutcome>>,
}

impl FakeSinks {
    /// Build sinks that accept everything on both services.
    pub fn accepting() -> Self {
        let sinks = Self::default();
        sinks.arm(
            "lastfm",
            ServiceOutcome {
                success: true,
                error: None,
            },
        );
        sinks.arm(
            "listenbrainz",
            ServiceOutcome {
                success: true,
                error: None,
            },
        );
        sinks
    }

    /// Build sinks with no linked account (empty outcomes).
    pub fn unlinked() -> Self {
        Self::default()
    }

    /// Script one service outcome.
    pub fn arm(&self, service: &str, outcome: ServiceOutcome) {
        if let Ok(mut outcomes) = self.outcomes.lock() {
            outcomes.insert(service.to_owned(), outcome);
        }
    }

    /// Now-playing forwards seen so far.
    pub fn now_playing_calls(&self) -> Vec<(String, ReportTrack)> {
        self.now_playing_calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    /// Scrobble forwards seen so far.
    pub fn scrobble_calls(&self) -> Vec<(String, ReportTrack)> {
        self.scrobble_calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    fn answer(
        &self,
        calls: &Mutex<Vec<(String, ReportTrack)>>,
        user_id: &str,
        track: &ReportTrack,
    ) -> HashMap<String, ServiceOutcome> {
        if let Ok(mut seen) = calls.lock() {
            seen.push((user_id.to_owned(), track.clone()));
        }
        self.outcomes
            .lock()
            .map(|outcomes| outcomes.clone())
            .unwrap_or_default()
    }
}

impl ScrobbleSinks for FakeSinks {
    fn report_now_playing(
        &self,
        user_id: &str,
        track: &ReportTrack,
    ) -> HashMap<String, ServiceOutcome> {
        self.answer(&self.now_playing_calls, user_id, track)
    }

    fn submit_scrobble(
        &self,
        user_id: &str,
        track: &ReportTrack,
    ) -> HashMap<String, ServiceOutcome> {
        self.answer(&self.scrobble_calls, user_id, track)
    }
}

/// One outbound attribution call the fake remote saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCall {
    /// `start`, `progress`, `stop`, or `scrobble`.
    pub op: String,
    /// Remote source key.
    pub source: String,
    /// Attributed report.
    pub report: RemoteReport,
}

/// Scripted remotes: record every attribution, fail armed calls.
#[derive(Debug, Default)]
pub struct FakeRemotes {
    calls: Mutex<Vec<RemoteCall>>,
    failures: Mutex<HashMap<(String, String), String>>,
}

impl FakeRemotes {
    /// Attribution calls seen so far.
    pub fn calls(&self) -> Vec<RemoteCall> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    /// Fail the next matching calls with a log-only cause.
    pub fn fail(&self, source: &str, op: &str, cause: &str) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.insert((source.to_owned(), op.to_owned()), cause.to_owned());
        }
    }

    fn answer(&self, op: &str, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(RemoteCall {
                op: op.to_owned(),
                source: source.to_owned(),
                report: report.clone(),
            });
        }
        let failure = self
            .failures
            .lock()
            .map(|failures| failures.get(&(source.to_owned(), op.to_owned())).cloned())
            .unwrap_or(None);
        match failure {
            Some(cause) => Err(ProviderFailure(cause)),
            None => Ok(()),
        }
    }
}

impl RemoteReporters for FakeRemotes {
    fn report_start(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("start", source, report)
    }

    fn report_progress(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("progress", source, report)
    }

    fn report_stop(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("stop", source, report)
    }

    fn scrobble(&self, source: &str, report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("scrobble", source, report)
    }
}

/// Scripted history: keeps every recorded play.
#[derive(Debug, Default)]
pub struct FakeHistory {
    records: Mutex<Vec<(String, PlayRecord)>>,
}

impl FakeHistory {
    /// Recorded plays seen so far.
    pub fn records(&self) -> Vec<(String, PlayRecord)> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }
}

impl PlayHistory for FakeHistory {
    fn record(&self, user_id: &str, record: &PlayRecord) {
        if let Ok(mut records) = self.records.lock() {
            records.push((user_id.to_owned(), record.clone()));
        }
    }
}

/// Scripted prefs: per-user forwarding and visibility, with defaults that
/// forward everywhere and show full.
#[derive(Debug, Default)]
pub struct FakePrefs {
    scrobble: Mutex<HashMap<String, ScrobblePrefs>>,
    visibility: Mutex<HashMap<String, String>>,
    visibility_failures: Mutex<HashSet<String>>,
}

impl FakePrefs {
    /// Forwarding defaults for a fresh user.
    pub fn default_scrobble() -> ScrobblePrefs {
        ScrobblePrefs {
            scrobble_to_lastfm: true,
            scrobble_to_listenbrainz: true,
            navidrome_handles_external: false,
        }
    }

    /// Script forwarding prefs for one user.
    pub fn set_scrobble(&self, user_id: &str, prefs: ScrobblePrefs) {
        if let Ok(mut scrobble) = self.scrobble.lock() {
            scrobble.insert(user_id.to_owned(), prefs);
        }
    }

    /// Script visibility for one user.
    pub fn set_visibility(&self, user_id: &str, visibility: &str) {
        if let Ok(mut visible) = self.visibility.lock() {
            visible.insert(user_id.to_owned(), visibility.to_owned());
        }
    }

    /// Fail visibility loads for one user with a log-only cause.
    pub fn fail_visibility(&self, user_id: &str) {
        if let Ok(mut failures) = self.visibility_failures.lock() {
            failures.insert(user_id.to_owned());
        }
    }
}

impl ListeningPrefs for FakePrefs {
    fn scrobble_prefs(&self, user_id: &str) -> ScrobblePrefs {
        self.scrobble
            .lock()
            .map(|prefs| {
                prefs
                    .get(user_id)
                    .cloned()
                    .unwrap_or_else(Self::default_scrobble)
            })
            .unwrap_or_else(|_| Self::default_scrobble())
    }

    fn visibility(&self, user_id: &str) -> Result<String, ProviderFailure> {
        let failed = self
            .visibility_failures
            .lock()
            .map(|failures| failures.contains(user_id))
            .unwrap_or(false);
        if failed {
            return Err(ProviderFailure("prefs store down".to_owned()));
        }
        Ok(self
            .visibility
            .lock()
            .map(|visible| {
                visible
                    .get(user_id)
                    .cloned()
                    .unwrap_or_else(|| VISIBILITY_FULL.to_owned())
            })
            .unwrap_or_else(|_| VISIBILITY_FULL.to_owned()))
    }
}

/// Scripted display names: per-user names over a fixed default.
#[derive(Debug, Default)]
pub struct FakeNames {
    names: Mutex<HashMap<String, String>>,
}

impl FakeNames {
    /// Script one display name.
    pub fn set(&self, user_id: &str, name: &str) {
        if let Ok(mut names) = self.names.lock() {
            names.insert(user_id.to_owned(), name.to_owned());
        }
    }
}

impl DisplayNames for FakeNames {
    fn display_name(&self, user_id: &str) -> String {
        self.names
            .lock()
            .map(|names| {
                names
                    .get(user_id)
                    .cloned()
                    .unwrap_or_else(|| "Test Listener".to_owned())
            })
            .unwrap_or_else(|_| "Test Listener".to_owned())
    }
}

/// Scripted warmup work: counts passes per scope, fails armed scopes.
#[derive(Debug, Default)]
pub struct FakeWarmup {
    passes: Mutex<Vec<String>>,
    failures: Mutex<HashMap<String, String>>,
}

impl FakeWarmup {
    /// Passes run so far, in order.
    pub fn passes(&self) -> Vec<String> {
        self.passes
            .lock()
            .map(|passes| passes.clone())
            .unwrap_or_default()
    }

    /// Fail the scope with a log-only cause.
    pub fn fail(&self, scope: WarmupScope, cause: &str) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.insert(scope.key().to_owned(), cause.to_owned());
        }
    }

    /// One pass of work for `scope`.
    pub fn run(&self, scope: WarmupScope) -> Result<WarmupStats, String> {
        if let Ok(mut passes) = self.passes.lock() {
            passes.push(scope.key().to_owned());
        }
        let failure = self
            .failures
            .lock()
            .map(|failures| failures.get(scope.key()).cloned())
            .unwrap_or(None);
        match failure {
            Some(cause) => Err(cause),
            None => Ok(WarmupStats {
                scope: scope.key(),
                warmed: 2,
                pruned: 1,
            }),
        }
    }
}

/// Boxed loop work over a shared fake (the shape `spawn_warmup_loops`
/// takes).
pub fn fake_work(
    fake: Arc<FakeWarmup>,
    scope: WarmupScope,
) -> impl FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<WarmupStats, String>> + Send>>
+ Send
+ 'static {
    move || {
        let fake = fake.clone();
        Box::pin(async move { fake.run(scope) })
    }
}
