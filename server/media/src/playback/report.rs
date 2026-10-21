//! Counted plays and external forwarding: the session-driven scrobble
//! (v2 compat-adapter path), the native submit and now-playing forward
//! (v2 `ScrobbleService` path), and the dedup caches both consult.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use super::{
    PlaybackDeps,
    forwarding::Service,
    listening::ScrobblePrefs,
    models::{ScrobbleNowPlayingRequest, ScrobbleResponse, ScrobbleSubmitRequest, ServiceResult},
    ports::{PlayRecord, ReportTrack, ScrobbleTargets, TrackInfo},
    rules::{ServiceError, normalize_client},
    sessions::SessionKey,
};

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
/// Scrobble timestamps older than this are rejected
/// (v2 `max_age`, 14 days).
pub const SCROBBLE_TIMESTAMP_MAX_AGE_SECS: i64 = 14 * 24 * 60 * 60;
/// Clock skew tolerated for future scrobble timestamps (v2, 60s).
pub const SCROBBLE_FUTURE_SKEW_SECS: i64 = 60;

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
    let response = submit_counted_play(deps, user_id, &report, Some(&track.track_id), now);
    if played_at.is_none() {
        deps.mixed.record(key.clone(), now);
    }
    deps.presence.remove(presence);
    response
}

/// Submit one native scrobble (v2 `ScrobbleService.submit_scrobble`:
/// validate, name-dedup, short-track gate, Navidrome delegation, then the
/// play recorded with its forwards; accepted once recorded).
///
/// A play from the library may name its track. The play is then filed
/// under the track that id names now, with that track's recording and
/// release group; names, length, time and source stay as sent. An id the
/// library does not know keeps the play by name with no ids: a play is
/// never lost to a lookup. A track id on a play from anywhere else is
/// refused, as such a play is filed only by its recording id.
pub fn submit_scrobble(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &ScrobbleSubmitRequest,
) -> Result<ScrobbleResponse, ServiceError> {
    let Some(track_id) = request.track_id.as_deref().filter(|id| !id.is_empty()) else {
        return submit_named(deps, user_id, request, None);
    };
    if normalize_client(request.source.as_deref()).is_some_and(|source| source != "local") {
        return Err(ServiceError::InvalidInput(
            "track_id is only for a play from the library".to_owned(),
        ));
    }
    match deps.store.get_track(track_id) {
        Some(track) => {
            let filed = ScrobbleSubmitRequest {
                mbid: track.recording_mbid.clone(),
                release_group_mbid: track.rg_mbid.clone(),
                ..request.clone()
            };
            submit_named(deps, user_id, &filed, Some(&track.track_id))
        }
        None => {
            tracing::info!(track_id, "scrobble names no library track; kept by name");
            let by_name = ScrobbleSubmitRequest {
                mbid: None,
                release_group_mbid: None,
                ..request.clone()
            };
            submit_named(deps, user_id, &by_name, None)
        }
    }
}

/// [`submit_scrobble`] for a play of a library track the server looked up
/// itself (the player-app paths): history files the play under that track.
pub fn submit_track_scrobble(
    deps: &PlaybackDeps,
    user_id: &str,
    track_id: &str,
    request: &ScrobbleSubmitRequest,
) -> Result<ScrobbleResponse, ServiceError> {
    submit_named(deps, user_id, request, Some(track_id))
}

fn submit_named(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &ScrobbleSubmitRequest,
    track_id: Option<&str>,
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
    Ok(submit_counted_play(deps, user_id, &report, track_id, now))
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
    let prefs = deps.store.scrobble_prefs(user_id);
    if source.as_deref() == Some("navidrome") && prefs.navidrome_handles_external_scrobbles {
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
    let handed = deps.sinks.report_now_playing(user_id, &track, targets);
    Ok(ScrobbleResponse {
        accepted: !handed.is_empty(),
        services: service_results(handed, true),
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

/// Shared submit core: name-dedup, then the short-track gate, Navidrome
/// delegation and the services the play goes to, then one write of the
/// play with a scrobble job per service, then the plugin event.
/// `played_at` is always set on this path. `track_id` is the library track
/// when the caller resolved one. A play that could not be saved is not
/// accepted, and nothing waits to be sent for it.
fn submit_counted_play(
    deps: &PlaybackDeps,
    user_id: &str,
    report: &ReportTrack,
    track_id: Option<&str>,
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
    let (prefs, links) = forwarding(deps, user_id);
    let short = report.duration_ms > 0 && report.duration_ms < MIN_TRACK_DURATION_MS;
    let delegated =
        report.source.as_deref() == Some("navidrome") && prefs.navidrome_handles_external_scrobbles;
    let counted = !short && !delegated;
    let forward = if counted {
        // A Last.fm section that cannot be read counts as on: the job reads
        // it again and holds the play until it can.
        let lastfm_on = deps
            .lastfm_switch
            .read()
            .map_or(true, |state| state.enabled);
        targets(&prefs).and(ScrobbleTargets {
            lastfm: links.lastfm && lastfm_on,
            listenbrainz: links.listenbrainz,
        })
    } else {
        ScrobbleTargets::default()
    };
    let saved = deps.store.record(
        user_id,
        &PlayRecord {
            track_id: track_id.map(str::to_owned),
            track_name: report.track_name.clone(),
            artist_name: report.artist_name.clone(),
            album_name: report.album_name.clone(),
            recording_mbid: report.mbid.clone(),
            release_group_mbid: report.release_group_mbid.clone(),
            duration_ms: Some(report.duration_ms).filter(|duration| *duration > 0),
            source: report.source.clone(),
            played_at,
        },
        forward,
    );
    if !saved {
        return ScrobbleResponse {
            accepted: false,
            services: service_results(forward, false),
        };
    }
    deps.dedup.record(dedup_key, now);
    if counted {
        deps.events.play_accepted(user_id, report, played_at);
    }
    ScrobbleResponse {
        accepted: true,
        services: service_results(forward, true),
    }
}

/// The user's preferences and links for a counted play. A read that fails
/// asks for both services as linked, with Navidrome delegation at its
/// default: each job reads them again and decides, so a failed read never
/// drops a play's scrobbles.
fn forwarding(deps: &PlaybackDeps, user_id: &str) -> (ScrobblePrefs, ScrobbleTargets) {
    deps.store.forwarding(user_id).unwrap_or_else(|error| {
        tracing::warn!(%error, "scrobble preferences read failed; the jobs will decide");
        let both = ScrobbleTargets {
            lastfm: true,
            listenbrainz: true,
        };
        let prefs = ScrobblePrefs {
            scrobble_to_lastfm: true,
            scrobble_to_listenbrainz: true,
            ..ScrobblePrefs::default()
        };
        (prefs, both)
    })
}

/// One result per service in `services`: waiting to be sent, or not
/// because the play was not saved.
fn service_results(services: ScrobbleTargets, queued: bool) -> HashMap<String, ServiceResult> {
    Service::ALL
        .into_iter()
        .filter(|service| services.wants(*service))
        .map(|service| {
            (
                service.as_str().to_owned(),
                ServiceResult {
                    queued,
                    error: (!queued).then(|| "Not saved; try again".to_owned()),
                },
            )
        })
        .collect()
}

/// Sink-side now-playing for a session start (same delegation and
/// preference rules as the direct forward; the outcome only feeds the log).
pub(super) fn forward_now_playing(
    deps: &PlaybackDeps,
    user_id: &str,
    track: &TrackInfo,
    source: Option<&str>,
) {
    let prefs = deps.store.scrobble_prefs(user_id);
    if source == Some("navidrome") && prefs.navidrome_handles_external_scrobbles {
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
    deps.sinks.report_now_playing(user_id, &report, targets);
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
