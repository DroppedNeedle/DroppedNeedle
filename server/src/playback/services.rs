//! Playback-reporting domain logic.
//!
//! One service layer over local and remote sources. Session reports
//! (`start`/`progress`/`stop`) resolve the track, keep the bounded session,
//! write presence, attribute the play to its remote server, and count a
//! scrobble past threshold; direct submits and now-playing forwards carry
//! the native by-name path. Every rule below is a v2 port with its citation;
//! reporting never fails the player, so remote and sink failures are logged
//! and swallowed, and only unknown tracks and malformed input error.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use super::{
    error::PlaybackError,
    models::{
        NowPlayingEntry, NowPlayingHeartbeat, NowPlayingSnapshot, PlaybackProgressRequest,
        PlaybackProgressResponse, PlaybackStartRequest, PlaybackStartResponse, PlaybackStopRequest,
        PlaybackStopResponse, ScrobbleNowPlayingRequest, ScrobbleResponse, ScrobbleSubmitRequest,
        ServiceResult,
    },
    ports::{
        Clock, DisplayNames, ListeningPrefs, PlayEvents, PlayHistory, PlayRecord, ProviderFailure,
        RemoteReport, RemoteReporters, ReportTrack, ScrobblePrefs, ScrobbleSinks, ScrobbleTargets,
        ServiceOutcome, TrackCatalog, TrackInfo, VISIBILITY_FULL, VISIBILITY_OFFLINE,
        VISIBILITY_TRACK_HIDDEN,
    },
};
use crate::ids::IdGenerator;

/// Session time-to-live in seconds (v2 `_SESSION_TTL_SECONDS`, 2h).
pub const SESSION_TTL_SECS: i64 = 2 * 60 * 60;
/// Session table cap (v2 `max_sessions`, oldest-first eviction).
pub const MAX_SESSIONS: usize = 1_000;
/// Presence time-to-live in seconds (v2 `ENTRY_TTL_SECONDS`).
pub const PRESENCE_TTL_SECS: i64 = 45;
/// Scrobble name-dedup window in seconds (v2 `DEDUP_TTL_SECONDS`, 1h).
pub const DEDUP_TTL_SECS: i64 = 3_600;
/// Scrobble name-dedup table cap (v2 `DEDUP_MAX_ENTRIES`).
pub const DEDUP_MAX_ENTRIES: usize = 200;
/// Tracks shorter than this record locally but never forward
/// (v2 `MIN_TRACK_DURATION_MS`, 30s).
pub const MIN_TRACK_DURATION_MS: i64 = 30_000;
/// Mixed-report dedup window in seconds (v2 `_MIXED_REPORT_DEDUP_SECONDS`).
pub const MIXED_REPORT_DEDUP_SECS: i64 = 5;
/// Mixed-report dedup table cap (v2 `_MIXED_REPORT_DEDUP_MAX`).
pub const MIXED_REPORT_DEDUP_MAX: usize = 1_000;
/// Subsonic scrobble ceiling in milliseconds (v2 `240_000`, 4 minutes).
pub const SUBSONIC_THRESHOLD_CAP_MS: i64 = 240_000;
/// Scrobble timestamps older than this are rejected
/// (v2 `max_age`, 14 days).
pub const SCROBBLE_TIMESTAMP_MAX_AGE_SECS: i64 = 14 * 24 * 60 * 60;
/// Clock skew tolerated for future scrobble timestamps (v2, 60s).
pub const SCROBBLE_FUTURE_SKEW_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Pure rules (v2 ports, directly covered by tests)
// ---------------------------------------------------------------------------

/// Normalize a reporting client name (v2 `CompatScrobbleAdapter._norm_client`:
/// strip, casefold, empty to `None`).
pub fn normalize_client(client: Option<&str>) -> Option<String> {
    let folded = caseless::default_case_fold_str(client.unwrap_or("").trim());
    if folded.is_empty() {
        None
    } else {
        Some(folded)
    }
}

/// Native scrobble threshold: the v2 Jellyfin stop rule, exact
/// (v2's Jellyfin router `_should_scrobble`).
///
/// - Omitted position counts (v2 "reference s6" quirk: a client that sends
///   no position is trusted to have finished).
/// - Past 90% of runtime counts.
/// - Within the last second counts (catches rounding at the tail).
/// - Unknown or non-positive runtime never counts on its own.
pub fn should_scrobble_native(position_ms: Option<i64>, duration_ms: Option<i64>) -> bool {
    let position = match position_ms {
        None => return true,
        Some(position) => position,
    };
    let runtime = duration_ms.unwrap_or(0);
    if runtime <= 0 {
        return false;
    }
    if position * 100 > 90 * runtime {
        return true;
    }
    position >= runtime - 1_000
}

/// Subsonic stop threshold in milliseconds (v2
/// `PlaybackReportService.report`: half the track, capped at 4 minutes;
/// unknown duration wants a full 4 minutes of play).
pub fn subsonic_scrobble_threshold_ms(duration_ms: i64) -> i64 {
    if duration_ms > 0 {
        (duration_ms / 2).min(SUBSONIC_THRESHOLD_CAP_MS)
    } else {
        SUBSONIC_THRESHOLD_CAP_MS
    }
}

/// True when `position_ms` reaches the Subsonic stop threshold.
pub fn meets_subsonic_threshold(position_ms: i64, duration_ms: i64) -> bool {
    position_ms >= subsonic_scrobble_threshold_ms(duration_ms)
}

/// True for sources with a remote server to attribute to. Local and YouTube
/// plays stay home (v2 only ever reports remote plays to their own server).
pub fn has_remote(source: &str) -> bool {
    matches!(source, "jellyfin" | "navidrome" | "plex")
}

/// Presence cover URL for a resolved track: the v3 release-group cover
/// route when an MBID is known, else empty (v2
/// `CompatScrobbleAdapter._write_presence` built the old route; same rule,
/// new path).
pub fn presence_cover_url(track: &TrackInfo) -> String {
    track
        .rg_mbid
        .as_deref()
        .filter(|mbid| !mbid.is_empty())
        .map(|mbid| format!("/api/v3/covers/release-group/{mbid}"))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Service errors
// ---------------------------------------------------------------------------

/// Domain failures. Forwarding and attribution failures never surface here:
/// they are logged and swallowed per the never-fail-the-player rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// Unknown track id.
    TrackNotFound,
    /// Malformed input. The message is user-facing.
    InvalidInput(String),
}

impl ServiceError {
    /// Render into the playback envelope.
    pub fn into_playback_error(self, _ids: &dyn IdGenerator) -> PlaybackError {
        match self {
            Self::TrackNotFound => PlaybackError::NotFound,
            Self::InvalidInput(message) => PlaybackError::InvalidInput { message },
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Session key (v2 `(user_id, client.casefold(), file_id)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    /// Owning user id.
    pub user_id: String,
    /// Normalized reporting client (device slug for native reports).
    pub client: String,
    /// Catalog track id.
    pub track_id: String,
}

/// One bounded playback session (v2 `_PlaybackSession`, plus the start
/// instant that becomes the scrobble timestamp on the Jellyfin path).
#[derive(Debug, Clone)]
struct Session {
    position_ms: Option<i64>,
    stopped: bool,
    submitted: bool,
    started_at: i64,
    updated_at: i64,
}

/// What `finish` hands the stop path.
#[derive(Debug, Clone)]
struct FinishView {
    submitted: bool,
    started_at: i64,
}

/// Bounded session table (v2 `PlaybackReportService` storage: 2h TTL,
/// 1,000-entry cap, oldest-first eviction).
#[derive(Debug, Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<HashMap<SessionKey, Session>>>,
}

impl SessionStore {
    /// Build an empty table.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Live session count, after TTL eviction.
    pub fn len(&self, now: i64) -> usize {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            sessions.len()
        } else {
            0
        }
    }

    /// Open a session, discarding any previous one: a start always resets
    /// (v2 `state == "starting"` branch).
    fn begin(&self, key: SessionKey, now: i64) {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            sessions.insert(
                key,
                Session {
                    position_ms: None,
                    stopped: false,
                    submitted: false,
                    started_at: now,
                    updated_at: now,
                },
            );
            enforce_session_cap(&mut sessions);
        }
    }

    /// Record a heartbeat. A stopped session that rewinds restarts fresh
    /// (v2 `previous.stopped and position < previous.position` branch);
    /// otherwise the heartbeat joins the live session.
    fn heartbeat(&self, key: SessionKey, position_ms: Option<i64>, now: i64) {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            let previous = sessions.remove(&key);
            let restart = previous.as_ref().is_some_and(|session| {
                session.stopped && position_ms.unwrap_or(0) < session.position_ms.unwrap_or(0)
            });
            let mut session = if restart { None } else { previous }.unwrap_or(Session {
                position_ms: None,
                stopped: false,
                submitted: false,
                started_at: now,
                updated_at: now,
            });
            session.position_ms = position_ms;
            session.updated_at = now;
            sessions.insert(key, session);
            enforce_session_cap(&mut sessions);
        }
    }

    /// Mark a session stopped, creating it when the player never said
    /// hello (v2 `previous or fresh` branch on the stop path).
    fn finish(&self, key: SessionKey, position_ms: Option<i64>, now: i64) -> FinishView {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            let previous = sessions.remove(&key);
            let mut session = previous.unwrap_or(Session {
                position_ms: None,
                stopped: false,
                submitted: false,
                started_at: now,
                updated_at: now,
            });
            session.position_ms = position_ms;
            session.stopped = true;
            session.updated_at = now;
            let view = FinishView {
                submitted: session.submitted,
                started_at: session.started_at,
            };
            sessions.insert(key, session);
            enforce_session_cap(&mut sessions);
            view
        } else {
            FinishView {
                submitted: false,
                started_at: now,
            }
        }
    }

    /// Remember that the session's play counted (once-only scrobbles).
    fn mark_submitted(&self, key: &SessionKey) {
        if let Ok(mut sessions) = self.inner.lock()
            && let Some(session) = sessions.get_mut(key)
        {
            session.submitted = true;
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop sessions idle past the TTL (v2 `_evict`).
fn evict_stale_sessions(sessions: &mut HashMap<SessionKey, Session>, now: i64) {
    let cutoff = now - SESSION_TTL_SECS;
    sessions.retain(|_, session| session.updated_at >= cutoff);
}

/// Hold the cap by dropping the stalest sessions first (v2 pops
/// insertion-oldest; update time orders the same way here).
fn enforce_session_cap(sessions: &mut HashMap<SessionKey, Session>) {
    while sessions.len() > MAX_SESSIONS {
        let oldest = sessions
            .iter()
            .min_by_key(|(_, session)| session.updated_at)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(key) => {
                sessions.remove(&key);
            }
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// One live presence entry (v2 `_Entry`, times in unix seconds).
#[derive(Debug, Clone)]
struct PresenceEntry {
    key: String,
    user_id: Option<String>,
    user_name: String,
    source: String,
    device_name: String,
    track_name: String,
    artist_name: String,
    album_name: Option<String>,
    cover_url: String,
    is_paused: bool,
    progress_ms: Option<i64>,
    duration_ms: Option<i64>,
    updated_at: i64,
    track_file_id: Option<String>,
}

/// One mapped session from a polled upstream server (`user_id` is always
/// `None`: upstream listeners are not DroppedNeedle accounts, v2
/// `ExternalSession`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSession {
    /// Presence key (`{source}:{remote session id}`).
    pub key: String,
    /// Upstream display name.
    pub user_name: String,
    /// Upstream device label.
    pub device_name: String,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Cover art URL.
    pub cover_url: String,
    /// True while paused.
    pub is_paused: bool,
    /// Position in milliseconds, when known.
    pub progress_ms: Option<i64>,
    /// Track length in milliseconds, when known.
    pub duration_ms: Option<i64>,
}

/// Fields for a presence upsert (v2 `NowPlayingService.update`).
#[derive(Debug, Clone)]
pub struct PresenceUpdate {
    /// Presence key (`{user_id}:{device}` for native reports).
    pub key: String,
    /// Owning user id (`None` for upstream sessions).
    pub user_id: Option<String>,
    /// Display name of the listener.
    pub user_name: String,
    /// Playback source.
    pub source: String,
    /// Device label.
    pub device_name: String,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title, when known.
    pub album_name: Option<String>,
    /// Cover art URL.
    pub cover_url: String,
    /// True while paused.
    pub is_paused: bool,
    /// Position in milliseconds, when known.
    pub progress_ms: Option<i64>,
    /// Track length in milliseconds, when known.
    pub duration_ms: Option<i64>,
    /// Catalog track id, when the reporter knows it.
    pub track_file_id: Option<String>,
}

/// In-memory live presence registry (v2 `NowPlayingService`).
///
/// Presence is transient and process-local: it is intentionally lost on
/// restart (v2 single-process invariant). Privacy is enforced here,
/// server-side, keyed on the owner's visibility setting, so a hidden track
/// is never serialized to other clients. Each mutation that v2 would
/// broadcast over SSE bumps the generation instead; no SSE fan-out
/// consumes it yet.
#[derive(Debug, Clone)]
pub struct PresenceRegistry {
    entries: Arc<Mutex<HashMap<String, PresenceEntry>>>,
    visibility: Arc<Mutex<HashMap<String, String>>>,
    generation: Arc<AtomicU64>,
}

impl PresenceRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            visibility: Arc::new(Mutex::new(HashMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Broadcast count: bumped on every publish, so tests can assert the
    /// no-op-no-publish rule without an SSE bus.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn publish(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Upsert a session. `load_visibility` resolves the owner's setting on
    /// first sight; a failed load leaves the user uncached so the next
    /// report retries and the entry redacts fail-closed meanwhile (v2
    /// `_load_visibility` rule: never cache a fail-open default, and
    /// presence must never fail a play report).
    pub fn update(
        &self,
        update: PresenceUpdate,
        now: i64,
        load_visibility: impl FnOnce(&str) -> Result<String, ProviderFailure>,
    ) {
        if let Some(user_id) = update.user_id.as_deref()
            && !self.knows_visibility(user_id)
            && let Ok(visibility) = load_visibility(user_id)
            && let Ok(mut cached) = self.visibility.lock()
        {
            cached.insert(user_id.to_owned(), visibility);
        }
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                update.key.clone(),
                PresenceEntry {
                    key: update.key,
                    user_id: update.user_id,
                    user_name: update.user_name,
                    source: update.source,
                    device_name: update.device_name,
                    track_name: update.track_name,
                    artist_name: update.artist_name,
                    album_name: update.album_name,
                    cover_url: update.cover_url,
                    is_paused: update.is_paused,
                    progress_ms: update.progress_ms,
                    duration_ms: update.duration_ms,
                    updated_at: now,
                    track_file_id: update.track_file_id,
                },
            );
        }
        self.publish();
    }

    /// Drop one session, publishing only when something was there.
    pub fn remove(&self, key: &str) {
        let existed = self
            .entries
            .lock()
            .map(|mut entries| entries.remove(key).is_some())
            .unwrap_or(false);
        if existed {
            self.publish();
        }
    }

    /// Replace every polled-upstream entry for `source` with the fresh poll
    /// result. Idle stays silent: no entries before and none now means no
    /// publish (v2 `reconcile_source` no-op rule).
    pub fn reconcile_source(&self, source: &str, sessions: Vec<ExternalSession>, now: i64) {
        let mut changed = false;
        if let Ok(mut entries) = self.entries.lock() {
            let had_any = entries
                .values()
                .any(|entry| entry.source == source && entry.user_id.is_none());
            if sessions.is_empty() && !had_any {
                return;
            }
            entries.retain(|_, entry| !(entry.source == source && entry.user_id.is_none()));
            for session in sessions {
                entries.insert(
                    session.key.clone(),
                    PresenceEntry {
                        key: session.key,
                        user_id: None,
                        user_name: session.user_name,
                        source: source.to_owned(),
                        device_name: session.device_name,
                        track_name: session.track_name,
                        artist_name: session.artist_name,
                        album_name: session.album_name,
                        cover_url: session.cover_url,
                        is_paused: session.is_paused,
                        progress_ms: session.progress_ms,
                        duration_ms: session.duration_ms,
                        updated_at: now,
                        track_file_id: None,
                    },
                );
            }
            changed = true;
        }
        if changed {
            self.publish();
        }
    }

    /// Drop sessions that stopped heartbeating; publish only when any were
    /// removed (v2 `sweep`).
    pub fn sweep(&self, now: i64) -> usize {
        let removed = self
            .entries
            .lock()
            .map(|mut entries| {
                let cutoff = now - PRESENCE_TTL_SECS;
                let stale: Vec<String> = entries
                    .values()
                    .filter(|entry| entry.updated_at < cutoff)
                    .map(|entry| entry.key.clone())
                    .collect();
                for key in &stale {
                    entries.remove(key);
                }
                stale.len()
            })
            .unwrap_or(0);
        if removed > 0 {
            self.publish();
        }
        removed
    }

    /// Apply a user's privacy choice (unknown values normalize to full)
    /// and re-broadcast so it takes effect live (v2 `set_visibility`).
    pub fn set_visibility(&self, user_id: &str, visibility: &str) {
        let normalized = match visibility {
            VISIBILITY_TRACK_HIDDEN | VISIBILITY_OFFLINE => visibility.to_owned(),
            _ => VISIBILITY_FULL.to_owned(),
        };
        if let Ok(mut cached) = self.visibility.lock() {
            cached.insert(user_id.to_owned(), normalized);
        }
        self.publish();
    }

    /// Privacy-projected live sessions (v2 `snapshot`; no sweep here, the
    /// sweeper loop owns TTLs).
    pub fn snapshot(&self) -> Vec<NowPlayingEntry> {
        let (entries, visibility) = match (self.entries.lock(), self.visibility.lock()) {
            (Ok(entries), Ok(visibility)) => (entries.clone(), visibility.clone()),
            _ => return Vec::new(),
        };
        entries
            .values()
            .filter_map(|entry| project(entry, &visibility))
            .collect()
    }

    /// Sessions a Subsonic `getNowPlaying` can serve: a catalog track id is
    /// known and the owner's visibility is full. Redacted projections carry
    /// no track, so they are skipped, not leaked (v2 `compat_now_playing`).
    pub fn compat_now_playing(&self) -> Vec<(NowPlayingEntry, String, i64)> {
        let (entries, visibility) = match (self.entries.lock(), self.visibility.lock()) {
            (Ok(entries), Ok(visibility)) => (entries.clone(), visibility.clone()),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for entry in entries.values() {
            let Some(file_id) = entry.track_file_id.clone() else {
                continue;
            };
            let Some(projection) = project(entry, &visibility) else {
                continue;
            };
            if projection.redacted {
                continue;
            }
            out.push((projection, file_id, entry.updated_at));
        }
        out
    }

    fn knows_visibility(&self, user_id: &str) -> bool {
        self.visibility
            .lock()
            .map(|cached| cached.contains_key(user_id))
            .unwrap_or(true)
    }
}

/// Saved visibility changes from the preferences route take effect live.
impl crate::plugins::scrobble::VisibilityHook for PresenceRegistry {
    fn on_visibility_changed(&self, user_id: &str, visibility: &str) {
        self.set_visibility(user_id, visibility);
    }
}

impl Default for PresenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Privacy projection (v2 `_project`): upstream sessions have no owner
/// setting and always show full; uncached owners redact fail-closed;
/// offline owners vanish; hidden owners keep identity and progress with
/// the song stripped.
fn project(entry: &PresenceEntry, visibility: &HashMap<String, String>) -> Option<NowPlayingEntry> {
    let setting = match entry.user_id.as_deref() {
        None => VISIBILITY_FULL,
        Some(user_id) => visibility
            .get(user_id)
            .map(String::as_str)
            .unwrap_or(VISIBILITY_TRACK_HIDDEN),
    };
    if setting == VISIBILITY_OFFLINE {
        return None;
    }
    let redacted = setting == VISIBILITY_TRACK_HIDDEN;
    Some(NowPlayingEntry {
        id: entry.key.clone(),
        user_name: entry.user_name.clone(),
        track_name: if redacted {
            String::new()
        } else {
            entry.track_name.clone()
        },
        artist_name: if redacted {
            String::new()
        } else {
            entry.artist_name.clone()
        },
        album_name: if redacted {
            None
        } else {
            entry.album_name.clone()
        },
        cover_url: if redacted {
            String::new()
        } else {
            entry.cover_url.clone()
        },
        device_name: entry.device_name.clone(),
        is_paused: entry.is_paused,
        source: entry.source.clone(),
        progress_ms: entry.progress_ms,
        duration_ms: entry.duration_ms,
        redacted,
    })
}

// ---------------------------------------------------------------------------
// Dedup caches
// ---------------------------------------------------------------------------

/// Name-and-timestamp scrobble dedup (v2 `ScrobbleService` cache: user-,
/// artist-, track-, and timestamp-scoped keys, 1h TTL, 200-entry cap with
/// expired-then-oldest eviction).
#[derive(Debug, Clone)]
pub struct ScrobbleDedup {
    inner: Arc<Mutex<HashMap<String, i64>>>,
}

impl ScrobbleDedup {
    /// Build an empty cache.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Key for one scrobble (v2 `_dedup_key`: user-scoped so two users
    /// scrobbling the same track never cross-dedup).
    pub fn key(user_id: &str, artist: &str, track: &str, timestamp: i64) -> String {
        format!(
            "{user_id}::{}::{}::{timestamp}",
            artist.to_lowercase(),
            track.to_lowercase()
        )
    }

    /// True when the key landed within the window.
    pub fn is_duplicate(&self, key: &str, now: i64) -> bool {
        self.inner
            .lock()
            .map(|entries| {
                entries
                    .get(key)
                    .is_some_and(|seen| now - *seen < DEDUP_TTL_SECS)
            })
            .unwrap_or(false)
    }

    /// Record a submission, holding the cap (v2 `_record_dedup`: expired
    /// entries go first, then the oldest survivors).
    pub fn record(&self, key: String, now: i64) {
        if let Ok(mut entries) = self.inner.lock() {
            entries.insert(key, now);
            if entries.len() > DEDUP_MAX_ENTRIES {
                let cutoff = now - DEDUP_TTL_SECS;
                entries.retain(|_, seen| *seen >= cutoff);
            }
            while entries.len() > DEDUP_MAX_ENTRIES {
                let oldest = entries
                    .iter()
                    .min_by_key(|(_, seen)| **seen)
                    .map(|(key, _)| key.clone());
                match oldest {
                    Some(key) => {
                        entries.remove(&key);
                    }
                    None => break,
                }
            }
        }
    }
}

impl Default for ScrobbleDedup {
    fn default() -> Self {
        Self::new()
    }
}

/// Mixed-report dedup (v2 `CompatScrobbleAdapter._recent_submissions`):
/// a live stop right after a direct submit for the same track counts once.
/// Only live submissions (no explicit play time) consult and feed this
/// cache; backdated plays like Jellyfin stops bypass it (v2 exact).
#[derive(Debug, Clone)]
pub struct MixedDedup {
    inner: Arc<Mutex<Vec<(SessionKey, i64)>>>,
}

impl MixedDedup {
    /// Build an empty cache.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Drop one key (a fresh start clears the track's recent submission,
    /// v2 `now_playing` branch).
    pub fn clear(&self, key: &SessionKey) {
        if let Ok(mut entries) = self.inner.lock() {
            entries.retain(|(known, _)| known != key);
        }
    }

    /// True when the key landed within the window (evicting stale first).
    pub fn contains(&self, key: &SessionKey, now: i64) -> bool {
        if let Ok(mut entries) = self.inner.lock() {
            let cutoff = now - MIXED_REPORT_DEDUP_SECS;
            entries.retain(|(_, seen)| *seen >= cutoff);
            entries.iter().any(|(known, _)| known == key)
        } else {
            false
        }
    }

    /// Record a live submission, holding the cap oldest-first.
    pub fn record(&self, key: SessionKey, now: i64) {
        if let Ok(mut entries) = self.inner.lock() {
            entries.push((key, now));
            while entries.len() > MIXED_REPORT_DEDUP_MAX {
                entries.remove(0);
            }
        }
    }
}

impl Default for MixedDedup {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Deps
// ---------------------------------------------------------------------------

/// Everything the reporting services need, built once and cloned into
/// handlers. Ports stay behind traits; the session, presence, and dedup
/// state is process-local by design (v2 single-process invariant).
#[derive(Clone)]
pub struct PlaybackDeps {
    /// Track resolution for reporting.
    pub catalog: Arc<dyn TrackCatalog>,
    /// External scrobble sinks (Last.fm / ListenBrainz).
    pub sinks: Arc<dyn ScrobbleSinks>,
    /// Outbound remote attribution (Jellyfin / Navidrome / Plex).
    pub remotes: Arc<dyn RemoteReporters>,
    /// Local play-history source of truth.
    pub history: Arc<dyn PlayHistory>,
    /// Per-user forwarding and visibility choices.
    pub prefs: Arc<dyn ListeningPrefs>,
    /// Display-name resolution for presence entries.
    pub names: Arc<dyn DisplayNames>,
    /// Bounded playback sessions.
    pub sessions: SessionStore,
    /// Live presence registry.
    pub presence: PresenceRegistry,
    /// Scrobble name dedup.
    pub dedup: ScrobbleDedup,
    /// Mixed-report dedup.
    pub mixed: MixedDedup,
    /// Clock for TTLs and timestamps.
    pub clock: Arc<dyn Clock>,
    /// Ids for error correlation.
    pub ids: Arc<dyn IdGenerator>,
    /// Listeners for accepted plays (plugins).
    pub events: Arc<dyn PlayEvents>,
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

/// Open a playback session: resolve the track, reset the session, forward
/// now-playing to the sinks, write presence, and attribute the start to the
/// remote server (v2 `PlaybackReportService` starting branch feeding
/// `CompatScrobbleAdapter.now_playing`, plus the outbound start call).
pub fn start_playback(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackStartRequest,
) -> Result<PlaybackStartResponse, ServiceError> {
    let track = resolve_track(deps, &request.track_id)?;
    let now = deps.clock.now_unix();
    let client = normalize_client(Some(&request.device)).unwrap_or_else(|| "app".to_owned());
    let key = SessionKey {
        user_id: user_id.to_owned(),
        client,
        track_id: request.track_id.clone(),
    };
    deps.sessions.begin(key.clone(), now);
    deps.mixed.clear(&key);

    let source = normalize_client(Some(&request.source));
    forward_now_playing(deps, user_id, &track, source.as_deref());
    write_session_presence(
        deps,
        user_id,
        &request.device,
        &track,
        source.as_deref(),
        false,
        None,
        now,
    );
    let report = RemoteReport {
        user_id: user_id.to_owned(),
        item_id: track.track_id.clone(),
        device: request.device.clone(),
        position_ms: None,
        is_paused: false,
    };
    attribute(deps, source.as_deref(), |remotes, source| {
        remotes.report_start(source, &report)
    });
    Ok(PlaybackStartResponse {
        accepted: true,
        session: format!("{}:{}:{}", key.user_id, key.client, key.track_id),
    })
}

/// Record a heartbeat: resolve the track, join the session, refresh
/// presence, and attribute progress to the remote server. Heartbeats never
/// forward a scrobble (v2 `progress` quirk: counting happens on stop).
pub fn report_progress(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackProgressRequest,
) -> Result<PlaybackProgressResponse, ServiceError> {
    let track = resolve_track(deps, &request.track_id)?;
    let now = deps.clock.now_unix();
    let client = normalize_client(Some(&request.device)).unwrap_or_else(|| "app".to_owned());
    deps.sessions.heartbeat(
        SessionKey {
            user_id: user_id.to_owned(),
            client,
            track_id: request.track_id.clone(),
        },
        request.position_ms,
        now,
    );
    let source = normalize_client(Some(&request.source));
    write_session_presence(
        deps,
        user_id,
        &request.device,
        &track,
        source.as_deref(),
        request.is_paused,
        request.position_ms,
        now,
    );
    let report = RemoteReport {
        user_id: user_id.to_owned(),
        item_id: track.track_id.clone(),
        device: request.device.clone(),
        position_ms: request.position_ms,
        is_paused: request.is_paused,
    };
    attribute(deps, source.as_deref(), |remotes, source| {
        remotes.report_progress(source, &report)
    });
    Ok(PlaybackProgressResponse { accepted: true })
}

/// Close a session: resolve the track, mark it stopped, always clear
/// presence (even below threshold, so nothing lingers to the TTL: v2
/// `_playing_stopped` rule), attribute the stop, and count the play past
/// the native threshold unless told to skip or already counted (v2
/// once-only `submitted` flag).
pub fn stop_playback(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackStopRequest,
) -> Result<PlaybackStopResponse, ServiceError> {
    let track = resolve_track(deps, &request.track_id)?;
    let now = deps.clock.now_unix();
    let client = normalize_client(Some(&request.device)).unwrap_or_else(|| "app".to_owned());
    let key = SessionKey {
        user_id: user_id.to_owned(),
        client,
        track_id: request.track_id.clone(),
    };
    let finish = deps.sessions.finish(key.clone(), request.position_ms, now);
    deps.presence
        .remove(&presence_key(user_id, &request.device));

    let source = normalize_client(Some(&request.source));
    let report = RemoteReport {
        user_id: user_id.to_owned(),
        item_id: track.track_id.clone(),
        device: request.device.clone(),
        position_ms: request.position_ms,
        is_paused: false,
    };
    attribute(deps, source.as_deref(), |remotes, source| {
        remotes.report_stop(source, &report)
    });

    let past_threshold = should_scrobble_native(request.position_ms, Some(track.duration_ms));
    if request.ignore_scrobble || finish.submitted || !past_threshold {
        return Ok(PlaybackStopResponse {
            accepted: true,
            scrobbled: false,
        });
    }
    submit_session_scrobble(
        deps,
        user_id,
        &key,
        &presence_key(user_id, &request.device),
        &track,
        source.as_deref(),
        Some(finish.started_at),
        now,
    );
    deps.sessions.mark_submitted(&key);
    attribute(deps, source.as_deref(), |remotes, source| {
        remotes.scrobble(source, &report)
    });
    Ok(PlaybackStopResponse {
        accepted: true,
        scrobbled: true,
    })
}

// ---------------------------------------------------------------------------
// Session-driven scrobble (v2 compat-adapter path)
// ---------------------------------------------------------------------------

/// Count one session play through the v2 `CompatScrobbleAdapter.scrobble`
/// flow: live submissions (no explicit play time) consult the 5s
/// mixed-report dedup and feed it; backdated ones like session stops
/// bypass it; presence clears either way because the track is over.
#[allow(clippy::too_many_arguments)]
pub fn submit_session_scrobble(
    deps: &PlaybackDeps,
    user_id: &str,
    key: &SessionKey,
    presence: &str,
    track: &TrackInfo,
    source: Option<&str>,
    played_at: Option<i64>,
    now: i64,
) -> ScrobbleResponse {
    if played_at.is_none() && deps.mixed.contains(key, now) {
        deps.presence.remove(presence);
        return ScrobbleResponse {
            accepted: true,
            services: HashMap::new(),
        };
    }
    let report = ReportTrack {
        track_name: track.title.clone(),
        artist_name: track.artist_name.clone(),
        album_name: Some(track.album_title.clone()).filter(|name| !name.is_empty()),
        duration_ms: track.duration_ms,
        mbid: track.recording_mbid.clone(),
        release_group_mbid: track.rg_mbid.clone(),
        source: source.map(str::to_owned),
        played_at: Some(played_at.unwrap_or(now)),
    };
    let response = submit_counted_play(deps, user_id, &report, now);
    if played_at.is_none() {
        deps.mixed.record(key.clone(), now);
    }
    deps.presence.remove(presence);
    response
}

// ---------------------------------------------------------------------------
// Native submit / now-playing forward (v2 ScrobbleService path)
// ---------------------------------------------------------------------------

/// Submit one native scrobble by name (v2 `ScrobbleService.submit_scrobble`,
/// exact order: validate, name-dedup, record history, short-track gate,
/// Navidrome delegation, forward; accepted stays true once recorded).
pub fn submit_scrobble(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &ScrobbleSubmitRequest,
) -> Result<ScrobbleResponse, ServiceError> {
    let now = deps.clock.now_unix();
    validate_timestamp(request.timestamp, now)?;
    let duration_ms = request.duration_ms.unwrap_or(0);
    if duration_ms < 0 {
        return Err(ServiceError::InvalidInput(
            "duration_ms must be >= 0".to_owned(),
        ));
    }
    let report = ReportTrack {
        track_name: request.track_name.clone(),
        artist_name: request.artist_name.clone(),
        album_name: request.album_name.clone(),
        duration_ms,
        mbid: request.mbid.clone(),
        release_group_mbid: request.release_group_mbid.clone(),
        source: normalize_client(request.source.as_deref()),
        played_at: Some(request.timestamp),
    };
    Ok(submit_counted_play(deps, user_id, &report, now))
}

/// Forward one native now-playing report (v2
/// `ScrobbleService.report_now_playing`: Navidrome delegation first, then
/// per-preference forwarding; accepted is true only when at least one
/// service took it, and false with no linked account at all).
pub fn forward_sink_now_playing(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &ScrobbleNowPlayingRequest,
) -> Result<ScrobbleResponse, ServiceError> {
    let duration_ms = request.duration_ms.unwrap_or(0);
    if duration_ms < 0 {
        return Err(ServiceError::InvalidInput(
            "duration_ms must be >= 0".to_owned(),
        ));
    }
    let source = normalize_client(request.source.as_deref());
    let prefs = deps.prefs.scrobble_prefs(user_id);
    if source.as_deref() == Some("navidrome") && prefs.navidrome_handles_external {
        return Ok(ScrobbleResponse {
            accepted: true,
            services: HashMap::new(),
        });
    }
    let track = ReportTrack {
        track_name: request.track_name.clone(),
        artist_name: request.artist_name.clone(),
        album_name: request.album_name.clone(),
        duration_ms,
        mbid: request.mbid.clone(),
        release_group_mbid: request.release_group_mbid.clone(),
        source,
        played_at: None,
    };
    let targets = targets(&prefs);
    if targets.is_empty() {
        return Ok(ScrobbleResponse {
            accepted: false,
            services: HashMap::new(),
        });
    }
    let outcomes = deps.sinks.report_now_playing(user_id, &track, targets);
    let (services, any_success) = collect_outcomes(outcomes);
    Ok(ScrobbleResponse {
        accepted: any_success,
        services,
    })
}

/// The services a user's preferences forward to (v2: one task per enabled
/// toggle, and the sink then needs a linked account).
fn targets(prefs: &ScrobblePrefs) -> ScrobbleTargets {
    ScrobbleTargets {
        lastfm: prefs.scrobble_to_lastfm,
        listenbrainz: prefs.scrobble_to_listenbrainz,
    }
}

/// Shared submit core: name-dedup, history, short-track gate, delegation,
/// forward. `played_at` is always set on this path.
fn submit_counted_play(
    deps: &PlaybackDeps,
    user_id: &str,
    report: &ReportTrack,
    now: i64,
) -> ScrobbleResponse {
    let played_at = report.played_at.unwrap_or(now);
    let dedup_key = ScrobbleDedup::key(user_id, &report.artist_name, &report.track_name, played_at);
    if deps.dedup.is_duplicate(&dedup_key, now) {
        return ScrobbleResponse {
            accepted: true,
            services: HashMap::new(),
        };
    }
    deps.history.record(
        user_id,
        &PlayRecord {
            track_name: report.track_name.clone(),
            artist_name: report.artist_name.clone(),
            album_name: report.album_name.clone(),
            recording_mbid: report.mbid.clone(),
            release_group_mbid: report.release_group_mbid.clone(),
            duration_ms: Some(report.duration_ms).filter(|duration| *duration > 0),
            source: report.source.clone(),
            played_at,
        },
    );
    deps.dedup.record(dedup_key, now);

    if report.duration_ms > 0 && report.duration_ms < MIN_TRACK_DURATION_MS {
        return ScrobbleResponse {
            accepted: true,
            services: HashMap::new(),
        };
    }
    let prefs = deps.prefs.scrobble_prefs(user_id);
    if report.source.as_deref() == Some("navidrome") && prefs.navidrome_handles_external {
        return ScrobbleResponse {
            accepted: true,
            services: HashMap::new(),
        };
    }
    deps.events.play_accepted(user_id, report, played_at);
    let targets = targets(&prefs);
    if targets.is_empty() {
        return ScrobbleResponse {
            accepted: true,
            services: HashMap::new(),
        };
    }
    let outcomes = deps.sinks.submit_scrobble(user_id, report, targets);
    let (services, _) = collect_outcomes(outcomes);
    ScrobbleResponse {
        accepted: true,
        services,
    }
}

/// Render per-service outcomes and note whether any service took the
/// report (v2 `_gather_results`: failures become per-service errors, never
/// a failed call).
fn collect_outcomes(
    outcomes: HashMap<String, ServiceOutcome>,
) -> (HashMap<String, ServiceResult>, bool) {
    let mut services = HashMap::new();
    let mut any_success = false;
    for (name, outcome) in outcomes {
        if outcome.success {
            any_success = true;
        } else {
            tracing::warn!(
                service = name,
                error = outcome.error,
                "scrobble forward failed"
            );
        }
        services.insert(
            name,
            ServiceResult {
                success: outcome.success,
                error: outcome.error,
            },
        );
    }
    (services, any_success)
}

/// Sink-side now-playing for a session start (same delegation and
/// preference rules as the direct forward; the outcome only feeds the log).
fn forward_now_playing(
    deps: &PlaybackDeps,
    user_id: &str,
    track: &TrackInfo,
    source: Option<&str>,
) {
    let prefs = deps.prefs.scrobble_prefs(user_id);
    if source == Some("navidrome") && prefs.navidrome_handles_external {
        return;
    }
    let report = ReportTrack {
        track_name: track.title.clone(),
        artist_name: track.artist_name.clone(),
        album_name: Some(track.album_title.clone()).filter(|name| !name.is_empty()),
        duration_ms: track.duration_ms,
        mbid: track.recording_mbid.clone(),
        release_group_mbid: track.rg_mbid.clone(),
        source: source.map(str::to_owned),
        played_at: None,
    };
    let targets = targets(&prefs);
    if targets.is_empty() {
        return;
    }
    let (services, _) = collect_outcomes(deps.sinks.report_now_playing(user_id, &report, targets));
    if services.values().any(|result| !result.success) {
        tracing::debug!("session-start now-playing forward partially failed");
    }
}

/// Timestamp bounds (v2 `ScrobbleRequest.__post_init__`: no future beyond
/// skew, nothing older than 14 days).
fn validate_timestamp(timestamp: i64, now: i64) -> Result<(), ServiceError> {
    if timestamp > now + SCROBBLE_FUTURE_SKEW_SECS {
        return Err(ServiceError::InvalidInput(
            "Timestamp cannot be in the future".to_owned(),
        ));
    }
    if timestamp < now - SCROBBLE_TIMESTAMP_MAX_AGE_SECS {
        return Err(ServiceError::InvalidInput(
            "Timestamp cannot be older than 14 days".to_owned(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Native presence writes
// ---------------------------------------------------------------------------

/// Native presence key (v2 route: `{user_id}:{device}`).
pub fn presence_key(user_id: &str, device: &str) -> String {
    format!("{user_id}:{device}")
}

/// Device label for presence. V2 hardcodes the `Web` label on the native
/// route; other device slugs pass through so a phone does not show as a
/// browser (default path unchanged).
pub fn device_label(device: &str) -> String {
    if device == "web" {
        "Web".to_owned()
    } else {
        device.to_owned()
    }
}

/// Record a native heartbeat (v2 `POST /now-playing`).
pub fn heartbeat(deps: &PlaybackDeps, user_id: &str, request: &NowPlayingHeartbeat) {
    let now = deps.clock.now_unix();
    let user_name = deps.names.display_name(user_id);
    deps.presence.update(
        PresenceUpdate {
            key: presence_key(user_id, &request.device),
            user_id: Some(user_id.to_owned()),
            user_name,
            source: request.source.clone(),
            device_name: device_label(&request.device),
            track_name: request.track_name.clone(),
            artist_name: request.artist_name.clone(),
            album_name: request.album_name.clone(),
            cover_url: request.cover_url.clone(),
            is_paused: request.is_paused,
            progress_ms: request.progress_ms,
            duration_ms: request.duration_ms,
            track_file_id: None,
        },
        now,
        |owner| deps.prefs.visibility(owner),
    );
}

/// Clear one native device (v2 `DELETE /now-playing`).
pub fn clear_presence(deps: &PlaybackDeps, user_id: &str, device: &str) {
    deps.presence.remove(&presence_key(user_id, device));
}

/// Privacy-projected live snapshot (v2 `GET /now-playing`).
pub fn snapshot(deps: &PlaybackDeps) -> NowPlayingSnapshot {
    NowPlayingSnapshot {
        sessions: deps.presence.snapshot(),
    }
}

/// Session-driven presence write (v2
/// `CompatScrobbleAdapter._write_presence`: starts arrive unpaused with no
/// position; progress carries both; failures never fail the report).
#[allow(clippy::too_many_arguments)]
fn write_session_presence(
    deps: &PlaybackDeps,
    user_id: &str,
    device: &str,
    track: &TrackInfo,
    source: Option<&str>,
    is_paused: bool,
    progress_ms: Option<i64>,
    now: i64,
) {
    let user_name = deps.names.display_name(user_id);
    deps.presence.update(
        PresenceUpdate {
            key: presence_key(user_id, device),
            user_id: Some(user_id.to_owned()),
            user_name,
            source: source.unwrap_or("local").to_owned(),
            device_name: device_label(device),
            track_name: track.title.clone(),
            artist_name: track.artist_name.clone(),
            album_name: Some(track.album_title.clone()).filter(|name| !name.is_empty()),
            cover_url: presence_cover_url(track),
            is_paused,
            progress_ms,
            duration_ms: Some(track.duration_ms).filter(|duration| *duration > 0),
            track_file_id: Some(track.track_id.clone()),
        },
        now,
        |owner| deps.prefs.visibility(owner),
    );
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Resolve the track or 404 (v2 `LibraryViewService.get_track` miss).
fn resolve_track(deps: &PlaybackDeps, track_id: &str) -> Result<TrackInfo, ServiceError> {
    deps.catalog
        .get_track(track_id)
        .ok_or(ServiceError::TrackNotFound)
}

/// Run one attribution call for remote sources only. Failures are logged
/// and swallowed (v2 playback services return false; the player never
/// sees them), and per-user attribution fails closed inside the port.
fn attribute(
    deps: &PlaybackDeps,
    source: Option<&str>,
    call: impl FnOnce(&dyn RemoteReporters, &str) -> Result<(), ProviderFailure>,
) {
    let Some(source) = source.filter(|source| has_remote(source)) else {
        return;
    };
    if let Err(cause) = call(deps.remotes.as_ref(), source) {
        tracing::warn!(%source, %cause, "remote playback report failed; continuing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_position_counts() {
        assert!(should_scrobble_native(None, Some(200_000)));
        assert!(should_scrobble_native(None, None));
    }

    #[test]
    fn past_ninety_percent_counts() {
        assert!(should_scrobble_native(Some(181_000), Some(200_000)));
        assert!(!should_scrobble_native(Some(180_000), Some(200_000)));
    }

    #[test]
    fn last_second_counts() {
        assert!(should_scrobble_native(Some(199_500), Some(200_000)));
        assert!(!should_scrobble_native(Some(100_000), Some(200_000)));
    }

    #[test]
    fn unknown_runtime_never_counts() {
        assert!(!should_scrobble_native(Some(100_000), None));
        assert!(!should_scrobble_native(Some(100_000), Some(0)));
    }

    #[test]
    fn subsonic_threshold_halves_and_caps() {
        assert_eq!(subsonic_scrobble_threshold_ms(600_000), 240_000);
        assert_eq!(subsonic_scrobble_threshold_ms(200_000), 100_000);
        assert_eq!(subsonic_scrobble_threshold_ms(0), 240_000);
        assert!(meets_subsonic_threshold(100_000, 200_000));
        assert!(!meets_subsonic_threshold(99_999, 200_000));
    }

    #[test]
    fn client_normalization_trims_and_folds() {
        assert_eq!(normalize_client(None), None);
        assert_eq!(normalize_client(Some("  ")), None);
        assert_eq!(
            normalize_client(Some(" Symfonium ")),
            Some("symfonium".to_owned())
        );
    }
}
