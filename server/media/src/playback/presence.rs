//! Live presence: the in-memory registry and the native and session-driven
//! presence writes.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use super::{
    PlaybackDeps,
    models::NowPlayingHeartbeat,
    ports::{
        ProviderFailure, TrackInfo, VISIBILITY_FULL, VISIBILITY_OFFLINE, VISIBILITY_TRACK_HIDDEN,
    },
    rules::presence_cover_url,
};
use droppedneedle_kernel::events::{NowPlayingEntry, NowPlayingSnapshot};

/// Presence time-to-live in seconds (v2 `ENTRY_TTL_SECONDS`).
pub const PRESENCE_TTL_SECS: i64 = 45;

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
/// is never serialized to other clients. Each change broadcasts the
/// projected snapshot as the `snapshot` event on the event hub, and bumps
/// the generation.
#[derive(Debug, Clone)]
pub struct PresenceRegistry {
    entries: Arc<Mutex<HashMap<String, PresenceEntry>>>,
    visibility: Arc<Mutex<HashMap<String, String>>>,
    generation: Arc<AtomicU64>,
    events: droppedneedle_kernel::events::EventHub,
}

impl PresenceRegistry {
    /// An empty registry that broadcasts on `events`.
    pub fn new(events: droppedneedle_kernel::events::EventHub) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            visibility: Arc::new(Mutex::new(HashMap::new())),
            generation: Arc::new(AtomicU64::new(0)),
            events,
        }
    }

    /// Broadcast count: bumped on every publish, so tests can assert the
    /// no-op-no-publish rule without an SSE bus.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Broadcast the projected snapshot of `entries`. Callers hold the
    /// entries lock across this call, so two concurrent changes publish in
    /// the order they were applied and the hub keeps the latest one.
    fn publish(&self, entries: &HashMap<String, PresenceEntry>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let Ok(visibility) = self.visibility.lock() else {
            tracing::warn!("presence visibility lock is poisoned; snapshot not published");
            return;
        };
        let sessions = entries
            .values()
            .filter_map(|entry| project(entry, &visibility))
            .collect();
        drop(visibility);
        self.events
            .publish(droppedneedle_kernel::events::Event::NowPlaying(
                NowPlayingSnapshot { sessions },
            ));
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
            self.publish(&entries);
        }
    }

    /// Drop one session, publishing only when something was there.
    pub fn remove(&self, key: &str) {
        if let Ok(mut entries) = self.entries.lock()
            && entries.remove(key).is_some()
        {
            self.publish(&entries);
        }
    }

    /// Replace every polled-upstream entry for `source` with the fresh poll
    /// result. Idle stays silent: no entries before and none now means no
    /// publish (v2 `reconcile_source` no-op rule).
    pub fn reconcile_source(&self, source: &str, sessions: Vec<ExternalSession>, now: i64) {
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
            self.publish(&entries);
        }
    }

    /// Drop sessions that stopped heartbeating; publish only when any were
    /// removed (v2 `sweep`).
    pub fn sweep(&self, now: i64) -> usize {
        self.entries
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
                if !stale.is_empty() {
                    self.publish(&entries);
                }
                stale.len()
            })
            .unwrap_or(0)
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
        if let Ok(entries) = self.entries.lock() {
            self.publish(&entries);
        }
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
    write_heartbeat(deps, user_id, request, None);
}

/// Record a compat now-playing report for a catalog track. The track id is
/// kept so `getNowPlaying` can list the entry.
pub fn compat_heartbeat(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &NowPlayingHeartbeat,
    track_file_id: &str,
) {
    write_heartbeat(deps, user_id, request, Some(track_file_id.to_owned()));
}

fn write_heartbeat(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &NowPlayingHeartbeat,
    track_file_id: Option<String>,
) {
    let now = deps.clock.now_unix();
    let user_name = deps.store.display_name(user_id);
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
            track_file_id,
        },
        now,
        |owner| deps.store.visibility(owner),
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
pub(super) fn write_session_presence(
    deps: &PlaybackDeps,
    user_id: &str,
    user_name: &str,
    device: &str,
    track: &TrackInfo,
    source: Option<&str>,
    is_paused: bool,
    progress_ms: Option<i64>,
    now: i64,
) {
    deps.presence.update(
        PresenceUpdate {
            key: presence_key(user_id, device),
            user_id: Some(user_id.to_owned()),
            user_name: user_name.to_owned(),
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
        |owner| deps.store.visibility(owner),
    );
}
