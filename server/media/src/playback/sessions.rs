//! Bounded playback sessions and their lifecycle: session reports
//! (`start`/`progress`/`stop`) resolve the track, keep the session, write
//! presence, attribute the play to its remote server, and count a scrobble
//! past threshold. Reporting never fails the player, so remote failures are
//! logged and swallowed, and only unknown tracks and malformed input error.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use super::{
    PlaybackDeps,
    models::{
        PlaybackProgressRequest, PlaybackProgressResponse, PlaybackStartRequest,
        PlaybackStartResponse, PlaybackStopRequest, PlaybackStopResponse,
    },
    ports::{ProviderFailure, RemoteReport, RemoteReporters, TrackInfo},
    presence::{presence_key, write_session_presence},
    report::{forward_now_playing, submit_session_scrobble},
    rules::{ServiceError, has_remote, normalize_client, should_scrobble_native},
};

/// Session time-to-live in seconds (v2 `_SESSION_TTL_SECONDS`, 2h).
pub const SESSION_TTL_SECS: i64 = 2 * 60 * 60;
/// Session table cap (v2 `max_sessions`, oldest-first eviction).
pub const MAX_SESSIONS: usize = 1_000;

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
