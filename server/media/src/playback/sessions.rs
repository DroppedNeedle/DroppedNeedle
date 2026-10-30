//! Bounded playback sessions and their lifecycle: a start resolves the
//! track and the listener's display name once and the session keeps them,
//! so progress and stop read neither again. Session reports write presence
//! and count a scrobble past threshold. A track played from a media server
//! takes none of that: its reports are queued for that server and the
//! route answers at once. Only unknown tracks and malformed input error.

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
    ports::{RemoteReport, TrackInfo},
    presence::{presence_key, write_session_presence},
    report::{forward_now_playing, submit_session_scrobble},
    rules::{ServiceError, normalize_client, should_scrobble_native},
};
use crate::remotes::adapter::request_id;

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

/// What a session resolves once and keeps: the track and the name its
/// presence entry shows.
#[derive(Debug, Clone)]
struct Resolved {
    track: TrackInfo,
    user_name: String,
}

/// One bounded playback session (v2 `_PlaybackSession`, plus the start
/// instant that becomes the scrobble timestamp on the Jellyfin path).
#[derive(Debug, Clone)]
struct Session {
    resolved: Resolved,
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

    /// What the live session under `key` resolved at its start.
    fn resolved(&self, key: &SessionKey, now: i64) -> Option<Resolved> {
        let mut sessions = self.inner.lock().ok()?;
        evict_stale_sessions(&mut sessions, now);
        sessions.get(key).map(|session| session.resolved.clone())
    }

    /// Open a session, discarding any previous one: a start always resets
    /// (v2 `state == "starting"` branch).
    fn begin(&self, key: SessionKey, resolved: Resolved, now: i64) {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            sessions.insert(
                key,
                Session {
                    resolved,
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
    fn heartbeat(&self, key: SessionKey, resolved: Resolved, position_ms: Option<i64>, now: i64) {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            let previous = sessions.remove(&key);
            let restart = previous.as_ref().is_some_and(|session| {
                session.stopped && position_ms.unwrap_or(0) < session.position_ms.unwrap_or(0)
            });
            let mut session = if restart { None } else { previous }.unwrap_or(Session {
                resolved,
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
    fn finish(
        &self,
        key: SessionKey,
        resolved: Resolved,
        position_ms: Option<i64>,
        now: i64,
    ) -> FinishView {
        if let Ok(mut sessions) = self.inner.lock() {
            evict_stale_sessions(&mut sessions, now);
            let previous = sessions.remove(&key);
            let mut session = previous.unwrap_or(Session {
                resolved,
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

/// Open a playback session. A media-server track is only reported to its
/// server. A library or YouTube track resolves, resets the session,
/// forwards now-playing to the sinks and writes presence (v2
/// `PlaybackReportService` starting branch feeding
/// `CompatScrobbleAdapter.now_playing`).
pub fn start_playback(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackStartRequest,
) -> Result<PlaybackStartResponse, ServiceError> {
    let client = normalize_client(Some(&request.device)).unwrap_or_else(|| "app".to_owned());
    let session = format!("{user_id}:{client}:{}", request.track_id);
    if let Some(source) = request.source.remote() {
        let report = remote_report(
            user_id,
            &request.track_id,
            request.position_ms,
            request.duration_ms,
            false,
        )?;
        deps.remotes.report_start(source, &report);
        return Ok(PlaybackStartResponse {
            accepted: true,
            session,
        });
    }
    let resolved = resolve(deps, user_id, &request.track_id)?;
    let now = deps.clock.now_unix();
    let key = SessionKey {
        user_id: user_id.to_owned(),
        client,
        track_id: request.track_id.clone(),
    };
    deps.sessions.begin(key.clone(), resolved.clone(), now);
    deps.mixed.clear(&key);
    let Resolved { track, user_name } = resolved;

    let source = Some(request.source.as_str());
    forward_now_playing(deps, user_id, &track, source);
    write_session_presence(
        deps,
        user_id,
        &user_name,
        &request.device,
        &track,
        source,
        false,
        None,
        now,
    );
    Ok(PlaybackStartResponse {
        accepted: true,
        session,
    })
}

/// Record a heartbeat. A media-server track is only reported to its
/// server; a library or YouTube track joins the session and refreshes
/// presence. Heartbeats never count a play (v2 `progress` quirk: counting
/// happens on stop).
pub fn report_progress(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackProgressRequest,
) -> Result<PlaybackProgressResponse, ServiceError> {
    if let Some(source) = request.source.remote() {
        let report = remote_report(
            user_id,
            &request.track_id,
            request.position_ms,
            request.duration_ms,
            request.is_paused,
        )?;
        deps.remotes.report_progress(source, &report);
        return Ok(PlaybackProgressResponse { accepted: true });
    }
    let now = deps.clock.now_unix();
    let client = normalize_client(Some(&request.device)).unwrap_or_else(|| "app".to_owned());
    let key = SessionKey {
        user_id: user_id.to_owned(),
        client,
        track_id: request.track_id.clone(),
    };
    let resolved = session_resolved(deps, &key, now)?;
    deps.sessions
        .heartbeat(key, resolved.clone(), request.position_ms, now);
    let Resolved { track, user_name } = resolved;
    write_session_presence(
        deps,
        user_id,
        &user_name,
        &request.device,
        &track,
        Some(request.source.as_str()),
        request.is_paused,
        request.position_ms,
        now,
    );
    Ok(PlaybackProgressResponse { accepted: true })
}

/// Close a session. A media-server track is only reported to its server
/// and never counts here: the web player's scrobble submit records it. A
/// library or YouTube track marks the session stopped, always clears
/// presence (even below threshold, so nothing lingers to the TTL: v2
/// `_playing_stopped` rule), and counts the play past the native threshold
/// unless told to skip or already counted (v2 once-only `submitted` flag).
pub fn stop_playback(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackStopRequest,
) -> Result<PlaybackStopResponse, ServiceError> {
    if let Some(source) = request.source.remote() {
        let report = remote_report(
            user_id,
            &request.track_id,
            request.position_ms,
            request.duration_ms,
            false,
        )?;
        deps.remotes.report_stop(source, &report);
        return Ok(PlaybackStopResponse {
            accepted: true,
            scrobbled: false,
        });
    }
    let now = deps.clock.now_unix();
    let client = normalize_client(Some(&request.device)).unwrap_or_else(|| "app".to_owned());
    let key = SessionKey {
        user_id: user_id.to_owned(),
        client,
        track_id: request.track_id.clone(),
    };
    let resolved = session_resolved(deps, &key, now)?;
    let track = resolved.track.clone();
    let finish = deps
        .sessions
        .finish(key.clone(), resolved, request.position_ms, now);
    deps.presence
        .remove(&presence_key(user_id, &request.device));

    let past_threshold = should_scrobble_native(request.position_ms, Some(track.duration_ms));
    if request.ignore_scrobble || finish.submitted || !past_threshold {
        return Ok(PlaybackStopResponse {
            accepted: true,
            scrobbled: false,
        });
    }
    let counted = submit_session_scrobble(
        deps,
        user_id,
        &key,
        &presence_key(user_id, &request.device),
        &track,
        Some(request.source.as_str()),
        Some(finish.started_at),
        now,
    );
    // A play that was not saved is not scrobbled: a stop sent again counts it.
    if !counted.accepted {
        return Ok(PlaybackStopResponse {
            accepted: true,
            scrobbled: false,
        });
    }
    deps.sessions.mark_submitted(&key);
    Ok(PlaybackStopResponse {
        accepted: true,
        scrobbled: true,
    })
}

/// What the live session under `key` resolved at its start, else a fresh
/// resolution (a player that never said hello, or a session past its TTL).
fn session_resolved(
    deps: &PlaybackDeps,
    key: &SessionKey,
    now: i64,
) -> Result<Resolved, ServiceError> {
    match deps.sessions.resolved(key, now) {
        Some(resolved) => Ok(resolved),
        None => resolve(deps, &key.user_id, &key.track_id),
    }
}

/// Resolve the track or 404 (v2 `LibraryViewService.get_track` miss), and
/// the listener's display name.
fn resolve(deps: &PlaybackDeps, user_id: &str, track_id: &str) -> Result<Resolved, ServiceError> {
    let track = deps
        .store
        .get_track(track_id)
        .ok_or(ServiceError::TrackNotFound)?;
    Ok(Resolved {
        track,
        user_name: deps.store.display_name(user_id),
    })
}

/// A media-server report, once its values are checked: the item id is a
/// plain id the server's address can carry, and no time is negative.
fn remote_report(
    user_id: &str,
    item_id: &str,
    position_ms: Option<i64>,
    duration_ms: Option<i64>,
    is_paused: bool,
) -> Result<RemoteReport, ServiceError> {
    if request_id(item_id).is_err() {
        return Err(ServiceError::InvalidInput(
            "That is not a media server item id.".to_owned(),
        ));
    }
    if position_ms.is_some_and(i64::is_negative) || duration_ms.is_some_and(i64::is_negative) {
        return Err(ServiceError::InvalidInput(
            "A position or length cannot be negative.".to_owned(),
        ));
    }
    Ok(RemoteReport {
        user_id: user_id.to_owned(),
        item_id: item_id.to_owned(),
        position_ms,
        duration_ms,
        is_paused,
    })
}
