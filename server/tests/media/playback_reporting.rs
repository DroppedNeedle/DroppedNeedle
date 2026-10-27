//! Playback reporting on the playback store over a migrated database: the
//! start, progress and stop lifecycle, the scrobble threshold rules, dedup
//! across duplicate and mixed reports, presence visibility, the session
//! gate, hidden faults, and the MBID warmup loop cadence. Only the outside
//! services (the now-playing sinks and the media servers) are doubles; a
//! counted play's scrobbles are read as the jobs written with it. Journeys
//! run the whole app on its own database: what a play from each kind of
//! player leaves in history, and a scrobble that outlives a restart.

use droppedneedle_media::playback;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_auth::Principal;
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_kernel::clock::{Clock, ManualClock};
use droppedneedle_media::remotes::models::SourceName;
use droppedneedle_media::remotes::warmup::{self, WarmupStats};
use playback::PlaybackDeps;
use playback::ports::{
    ProviderFailure, RemoteReport, RemoteReporters, ReportTrack, ScrobbleSinks, ScrobbleTargets,
};
use playback::presence::{PresenceRegistry, presence_key};
use playback::report::{MixedDedup, ScrobbleDedup, submit_session_scrobble};
use playback::rules::{
    meets_subsonic_threshold, should_scrobble_native, subsonic_scrobble_threshold_ms,
};
use playback::sessions::{SESSION_TTL_SECS, SessionKey, SessionStore};
use playback::sqlite::PlaybackStore;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{test_principal, with_test_principal};
use crate::common::compat::{CompatApp, PLAYER, named, rest};
use crate::common::{ScratchDir, TestApp};

const NOW: i64 = 1_790_000_000;

/// Last.fm and ListenBrainz now-playing: records every report and takes it.
#[derive(Default)]
struct FakeSinks {
    now_playing: Mutex<Vec<ReportTrack>>,
}

impl FakeSinks {
    fn now_playing_calls(&self) -> usize {
        self.now_playing.lock().expect("lock").len()
    }
}

impl ScrobbleSinks for FakeSinks {
    fn report_now_playing(
        &self,
        _user_id: &str,
        track: &ReportTrack,
        targets: ScrobbleTargets,
    ) -> ScrobbleTargets {
        self.now_playing.lock().expect("lock").push(track.clone());
        targets
    }
}

/// The user's media servers: records every attribution call.
#[derive(Default)]
struct FakeRemotes {
    calls: Mutex<Vec<(&'static str, String)>>,
}

impl FakeRemotes {
    fn answer(&self, op: &'static str, source: &str) -> Result<(), ProviderFailure> {
        self.calls
            .lock()
            .expect("lock")
            .push((op, source.to_owned()));
        Ok(())
    }

    fn calls(&self) -> usize {
        self.calls.lock().expect("lock").len()
    }
}

impl RemoteReporters for FakeRemotes {
    fn report_start(&self, source: &str, _report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("start", source)
    }

    fn report_progress(&self, source: &str, _report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("progress", source)
    }

    fn report_stop(&self, source: &str, _report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("stop", source)
    }

    fn scrobble(&self, source: &str, _report: &RemoteReport) -> Result<(), ProviderFailure> {
        self.answer("scrobble", source)
    }
}

/// The listeners the rig knows, one library track ("Roads", four minutes,
/// with a recording and a matched release group), and the first listener
/// forwarding to both scrobble services.
const SEED: &str = "\
    INSERT INTO auth_users (id, display_name, created_at) VALUES \
     ('user-1', 'Listener One', ''), ('user-hidden', 'Hidden', ''), \
     ('user-offline', 'Offline', ''), ('user-broken', 'Broken', ''); \
    INSERT INTO user_listening_prefs (user_id, scrobble_to_lastfm, scrobble_to_listenbrainz, \
     navidrome_handles_external_scrobbles, updated_at) VALUES ('user-1', 1, 1, 0, ''); \
    INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
     VALUES ('artist-1', 'Portishead', 'portishead', 'group', 0, 0); \
    INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
     album_artist_id, grouping_source, created_at, updated_at) \
     VALUES ('album-1', 'root-1', 'portishead/dummy', 'Dummy', 'dummy', 'artist-1', \
     'manual', 0, 0); \
    INSERT INTO local_album_external_identities \
     (local_album_id, release_group_mbid, decision_source, selected_at) \
     VALUES ('album-1', 'rg-mbid-1', 'manual', 1); \
    INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
     path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
     artist_name, album_title, album_title_folded, embedded_recording_mbid, file_format, \
     duration_seconds, ingest_source, imported_at, membership_source) \
     VALUES ('track-1', 'album-1', 'root-1', '/music/roads.flac', 'roads.flac', 'hash-1', \
     1024, 0, 'rev-1', 'Roads', 'roads', 'Portishead', 'Dummy', 'dummy', \
     'recording-mbid-1', 'flac', 240.0, 'seed', 0, 'manual');";

struct Rig {
    deps: PlaybackDeps,
    clock: ManualClock,
    sinks: Arc<FakeSinks>,
    remotes: Arc<FakeRemotes>,
    crypto: Arc<droppedneedle_kernel::runtime_config::Crypto>,
    db: droppedneedle_kernel::db::Db,
    scratch: ScratchDir,
}

/// The Last.fm switch over settings in `scratch` with Last.fm on and no
/// instance key pair.
fn lastfm_on(scratch: &ScratchDir) -> droppedneedle_auth::users::stores::LastFmSwitch {
    let settings = crate::common::config_store(&scratch.join("settings"));
    settings
        .save_secret_blocking(
            droppedneedle_kernel::runtime_config::secret_sections::LastFmSettings {
                enabled: true,
                ..Default::default()
            },
        )
        .expect("Last.fm settings save");
    droppedneedle_auth::users::stores::LastFmSwitch::new(settings.live())
}

fn rig() -> Rig {
    let scratch = ScratchDir::new("playback-rig");
    let path = scratch.join("app.db");
    crate::common::migrated_db(&path);
    rusqlite::Connection::open(&path)
        .expect("seed connection")
        .execute_batch(SEED)
        .expect("the rig is seeded");
    let clock = ManualClock::at(NOW * 1000);
    let crypto = Arc::new(
        droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
    );
    let db = droppedneedle_kernel::db::Db::open_for_tests(&path).expect("database opens");
    let store = Arc::new(PlaybackStore::new(
        db.clone(),
        Arc::new(droppedneedle_kernel::ids::UuidGenerator),
        crypto.clone(),
        Arc::new(clock.clone()),
        droppedneedle_kernel::jobs::queue::JobQueue::new(db.clone(), Arc::new(clock.clone())),
    ));
    let sinks = Arc::new(FakeSinks::default());
    let remotes = Arc::new(FakeRemotes::default());
    let deps = PlaybackDeps {
        store,
        sinks: sinks.clone(),
        lastfm_switch: lastfm_on(&scratch),
        remotes: remotes.clone(),
        sessions: SessionStore::new(),
        presence: PresenceRegistry::new(droppedneedle_kernel::events::EventHub::new()),
        dedup: ScrobbleDedup::new(),
        mixed: MixedDedup::new(),
        clock: Arc::new(clock.clone()),
        events: Arc::new(playback::ports::NoPlayEvents),
    };
    let rig = Rig {
        deps,
        clock,
        sinks,
        remotes,
        crypto,
        db,
        scratch,
    };
    rig.link_listenbrainz("user-1", "lb-token");
    rig.link_lastfm("user-1");
    rig
}

impl Rig {
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.scratch.join("app.db")).expect("database opens")
    }

    /// Plays waiting to be scrobbled, to any service.
    fn scrobbles(&self) -> i64 {
        self.sql()
            .query_row(
                "SELECT COUNT(DISTINCT idempotency_key) FROM durable_jobs \
                 WHERE kind LIKE 'scrobble.%'",
                [],
                |row| row.get(0),
            )
            .expect("scrobble jobs count")
    }

    /// Plays recorded so far, for every listener.
    fn plays(&self) -> i64 {
        self.sql()
            .query_row("SELECT COUNT(*) FROM library_play_history", [], |row| {
                row.get(0)
            })
            .expect("plays count")
    }

    /// Save the listener's forwarding toggles.
    fn set_scrobble(&self, user_id: &str, lastfm: bool, listenbrainz: bool, navidrome: bool) {
        self.sql()
            .execute(
                "INSERT INTO user_listening_prefs (user_id, scrobble_to_lastfm, \
                 scrobble_to_listenbrainz, navidrome_handles_external_scrobbles, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, '') ON CONFLICT (user_id) DO UPDATE SET \
                 scrobble_to_lastfm = ?2, scrobble_to_listenbrainz = ?3, \
                 navidrome_handles_external_scrobbles = ?4",
                rusqlite::params![user_id, lastfm, listenbrainz, navidrome],
            )
            .expect("prefs saved");
    }

    /// Save the listener's presence visibility.
    fn set_visibility(&self, user_id: &str, visibility: &str) {
        self.sql()
            .execute(
                "INSERT INTO user_listening_prefs (user_id, now_playing_visibility, updated_at) \
                 VALUES (?1, ?2, '') ON CONFLICT (user_id) DO UPDATE SET \
                 now_playing_visibility = ?2",
                [user_id, visibility],
            )
            .expect("visibility saved");
    }

    /// Store a visibility that does not read as text, so the listener's
    /// visibility read fails.
    fn break_visibility(&self, user_id: &str) {
        self.sql()
            .execute(
                "INSERT INTO user_listening_prefs (user_id, now_playing_visibility, updated_at) \
                 VALUES (?1, X'00', '')",
                [user_id],
            )
            .expect("broken visibility stored");
    }

    /// Link ListenBrainz for the listener with `token`, sealed the way the
    /// link store seals it.
    fn link_listenbrainz(&self, user_id: &str, token: &str) {
        let sealed = self
            .crypto
            .encrypt(&json!({"user_token": token, "username": "listener"}).to_string())
            .expect("link seals");
        self.link(user_id, "listenbrainz", &sealed);
    }

    /// Link Last.fm for the listener with their own key pair, each secret
    /// sealed on its own as the Last.fm link flow stores them.
    fn link_lastfm(&self, user_id: &str) {
        let seal = |value: &str| self.crypto.encrypt(value).expect("secret seals");
        let document = json!({
            "configured": true,
            "api_key": seal("lfm-key"),
            "shared_secret": seal("lfm-secret"),
            "username": "listener",
            "session_key": seal("lfm-session"),
        });
        self.link(user_id, "lastfm", &document.to_string());
    }

    /// Link `service` for the listener, with `data` as the stored document.
    fn link(&self, user_id: &str, service: &str, data: &str) {
        self.sql()
            .execute(
                "INSERT INTO user_connections (user_id, service, connection_data, enabled, \
                 created_at, updated_at) VALUES (?1, ?2, ?3, 1, '', '')",
                [user_id, service, data],
            )
            .expect("link stored");
    }
}

fn principal(user_id: &str) -> Principal {
    test_principal(user_id, Role::User, "sess-test", SessionKind::Standard)
}

fn app(rig: &Rig, user: Option<&str>) -> Router {
    let router = playback::routes()
        .with_state(rig.deps.clone())
        .into_router();
    match user {
        Some(id) => with_test_principal(router, principal(id)),
        None => router,
    }
}

async fn call(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, headers)
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("<no-code>")
}

async fn start(rig: &Rig, user: &str, track: &str, source: &str) -> (StatusCode, Value) {
    let (status, body, _) = call(
        app(rig, Some(user)),
        "POST",
        "/api/v3/playback/start",
        Some(json!({"track_id": track, "source": source, "device": "web"})),
    )
    .await;
    (status, body)
}

async fn progress(
    rig: &Rig,
    user: &str,
    track: &str,
    position_ms: Option<i64>,
    is_paused: bool,
) -> (StatusCode, Value) {
    let (status, body, _) = call(
        app(rig, Some(user)),
        "POST",
        "/api/v3/playback/progress",
        Some(json!({
            "track_id": track,
            "source": "local",
            "device": "web",
            "position_ms": position_ms,
            "is_paused": is_paused,
        })),
    )
    .await;
    (status, body)
}

async fn stop(
    rig: &Rig,
    user: &str,
    track: &str,
    source: &str,
    position_ms: Option<i64>,
) -> (StatusCode, Value) {
    let (status, body, _) = call(
        app(rig, Some(user)),
        "POST",
        "/api/v3/playback/stop",
        Some(json!({
            "track_id": track,
            "source": source,
            "device": "web",
            "position_ms": position_ms,
        })),
    )
    .await;
    (status, body)
}

async fn snapshot(rig: &Rig, user: &str) -> Value {
    let (_, body, _) = call(app(rig, Some(user)), "GET", "/api/v3/now-playing", None).await;
    body
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_start_progress_stop_counts_the_play() {
    let rig = rig();
    let (status, body) = start(&rig, "user-1", "track-1", "local").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["session"], "user-1:web:track-1");

    let (status, _) = progress(&rig, "user-1", "track-1", Some(120_000), false).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], true);
    assert_eq!(body["scrobbled"], true);

    assert_eq!(rig.plays(), 1);
    assert_eq!(rig.scrobbles(), 1);
    assert_eq!(rig.sinks.now_playing_calls(), 1);
    assert_eq!(rig.remotes.calls(), 0);
    assert_eq!(
        snapshot(&rig, "user-1").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(rig.deps.sessions.len(NOW), 1);
    rig.clock.advance((SESSION_TTL_SECS + 1) * 1000);
    assert_eq!(rig.deps.sessions.len(rig.clock.now_unix()), 0);
}

/// A session that stops while another write holds the lane for two
/// seconds still records its play, once: the play waits its turn and a
/// repeated stop does not count it again.
#[tokio::test]
async fn a_play_waits_out_a_held_lane_and_lands_once() {
    let rig = rig();
    let (status, _) = start(&rig, "user-1", "track-1", "local").await;
    assert_eq!(status, StatusCode::OK);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder_db = rig.db.clone();
    let holder = std::thread::spawn(move || {
        holder_db.write_blocking(
            droppedneedle_kernel::db::Lane::Background,
            "brief-hold",
            |_tx| {
                held_tx.send(()).expect("the test waits");
                std::thread::sleep(Duration::from_secs(2));
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            },
        )
    });
    held_rx.recv().expect("the lane is held");
    for _ in 0..2 {
        let (status, body) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    holder
        .join()
        .expect("the holder ends")
        .expect("the hold commits");
    assert_eq!(rig.plays(), 1);
    assert_eq!(rig.scrobbles(), 1);
}

#[tokio::test]
async fn stop_below_threshold_skips_but_clears_presence() {
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (status, body) = stop(&rig, "user-1", "track-1", "local", Some(100_000)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scrobbled"], false);

    assert_eq!(rig.plays(), 0);
    assert_eq!(rig.scrobbles(), 0);
    assert_eq!(
        snapshot(&rig, "user-1").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn stop_counts_once_per_session() {
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (_, first) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(first["scrobbled"], true);
    let (_, second) = stop(&rig, "user-1", "track-1", "local", Some(235_000)).await;
    assert_eq!(second["scrobbled"], false);
    assert_eq!(rig.plays(), 1);
}

#[tokio::test]
async fn restart_after_rewind_opens_a_fresh_session() {
    // A stopped session that rewinds restarts fresh (v2 rewind branch), so
    // replaying the track counts again.
    let rig = rig();
    start(&rig, "user-1", "track-1", "local").await;
    let (_, first) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(first["scrobbled"], true);
    rig.clock.advance(300 * 1000);
    progress(&rig, "user-1", "track-1", Some(10_000), false).await;
    let (_, second) = stop(&rig, "user-1", "track-1", "local", Some(230_000)).await;
    assert_eq!(second["scrobbled"], true);
    assert_eq!(rig.plays(), 2);
}

// ---------------------------------------------------------------------------
// Threshold rules
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scrobble_threshold_rules() {
    // Native rule: the v2 Jellyfin stop rule, exact.
    assert!(should_scrobble_native(None, Some(200_000)));
    assert!(should_scrobble_native(None, None));
    assert!(should_scrobble_native(Some(181_000), Some(200_000)));
    assert!(!should_scrobble_native(Some(180_000), Some(200_000)));
    // Exactly 90% still counts inside the last second (the within-1s
    // clause firing where the percent clause does not).
    assert!(should_scrobble_native(Some(9_000), Some(10_000)));
    assert!(!should_scrobble_native(Some(8_999), Some(10_000)));
    assert!(!should_scrobble_native(Some(100_000), Some(200_000)));
    assert!(!should_scrobble_native(Some(100_000), None));
    assert!(!should_scrobble_native(Some(100_000), Some(0)));

    // Subsonic rule: half the track capped at four minutes, four minutes
    // flat for unknown durations.
    assert_eq!(subsonic_scrobble_threshold_ms(600_000), 240_000);
    assert_eq!(subsonic_scrobble_threshold_ms(200_000), 100_000);
    assert_eq!(subsonic_scrobble_threshold_ms(0), 240_000);
    assert!(meets_subsonic_threshold(100_000, 200_000));
    assert!(!meets_subsonic_threshold(99_999, 200_000));
}

// ---------------------------------------------------------------------------
// Native submit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn duplicate_submits_dedup() {
    let rig = rig();
    let body = json!({
        "track_name": "Roads",
        "artist_name": "Portishead",
        "timestamp": NOW,
        "album_name": "Dummy",
        "duration_ms": 240_000,
    });
    for _ in 0..2 {
        let (status, answer, _) = call(
            app(&rig, Some("user-1")),
            "POST",
            "/api/v3/scrobble/submit",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["accepted"], true);
    }
    assert_eq!(rig.plays(), 1);
    assert_eq!(rig.scrobbles(), 1);
}

#[tokio::test]
async fn navidrome_delegation_skips_forwarding_but_records() {
    let rig = rig();
    rig.set_scrobble("user-1", true, true, true);
    let (status, answer, _) = call(
        app(&rig, Some("user-1")),
        "POST",
        "/api/v3/scrobble/submit",
        Some(json!({
            "track_name": "Roads",
            "artist_name": "Portishead",
            "timestamp": NOW,
            "duration_ms": 240_000,
            "source": "Navidrome",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["accepted"], true);
    assert_eq!(rig.plays(), 1);
    assert_eq!(rig.scrobbles(), 0);
}

#[tokio::test]
async fn mixed_report_dedup_counts_live_once() {
    let rig = rig();
    let track = rig
        .deps
        .store
        .get_track("track-1")
        .expect("the track is in the catalog");
    let key = SessionKey {
        user_id: "user-1".to_owned(),
        client: "web".to_owned(),
        track_id: track.track_id.clone(),
    };
    let presence = presence_key("user-1", "web");
    let first = submit_session_scrobble(
        &rig.deps, "user-1", &key, &presence, &track, None, None, NOW,
    );
    assert!(first.accepted);
    let second = submit_session_scrobble(
        &rig.deps, "user-1", &key, &presence, &track, None, None, NOW,
    );
    assert!(second.accepted);
    assert!(second.services.is_empty());
    assert_eq!(rig.plays(), 1);

    // Backdated plays bypass the mixed cache (v2 exact); a distinct play
    // time counts again.
    let third = submit_session_scrobble(
        &rig.deps,
        "user-1",
        &key,
        &presence,
        &track,
        None,
        Some(NOW - 100),
        NOW,
    );
    assert!(third.accepted);
    assert_eq!(rig.plays(), 2);
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn presence_visibility_redacts_and_omits() {
    let rig = rig();
    rig.set_visibility("user-hidden", "track_hidden");
    rig.set_visibility("user-offline", "offline");
    rig.break_visibility("user-broken");
    for user in ["user-hidden", "user-offline", "user-broken"] {
        let (status, _, _) = call(
            app(&rig, Some(user)),
            "POST",
            "/api/v3/now-playing",
            Some(json!({
                "track_name": "Roads",
                "artist_name": "Portishead",
                "progress_ms": 10_000,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let body = snapshot(&rig, "user-1").await;
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    for entry in sessions {
        assert_eq!(entry["track_name"], "");
        assert_eq!(entry["artist_name"], "");
        assert_eq!(entry["redacted"], true);
        assert_eq!(entry["progress_ms"], 10_000);
    }
    assert!(
        sessions
            .iter()
            .all(|entry| entry["id"] != "user-offline:web")
    );
}

// ---------------------------------------------------------------------------
// Remote attribution, errors, auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reporting_requires_session() {
    let rig = rig();
    let (status, body, headers) = call(app(&rig, None), "GET", "/api/v3/now-playing", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
    assert_eq!(
        headers
            .get(axum::http::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    let (status, body, _) = call(
        app(&rig, None),
        "POST",
        "/api/v3/playback/start",
        Some(json!({"track_id": "track-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
}

// ---------------------------------------------------------------------------
// Warmup loops (paused time on the loop supervisor)
// ---------------------------------------------------------------------------

/// Each source waits its startup delay, warms, and warms again every four
/// hours, give or take the jitter.
#[tokio::test(start_paused = true)]
async fn warmup_loops_use_honest_cadences() {
    use droppedneedle_kernel::jobs::{CancellationToken, Supervisor};
    use tokio::time::Instant;

    let scratch = crate::common::ScratchDir::new("warmup");
    let path = scratch.join("app.db");
    crate::common::migrated_db(&path);
    let supervisor = Supervisor::start(
        droppedneedle_kernel::db::Db::open_for_tests(&path).expect("database opens"),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        CancellationToken::new(),
    )
    .await
    .expect("supervisor starts");
    for (source, delay) in [
        (SourceName::Jellyfin, Duration::from_secs(8)),
        (SourceName::Navidrome, Duration::from_secs(12)),
        (SourceName::Plex, Duration::from_secs(15)),
    ] {
        let passes: Arc<Mutex<Vec<Instant>>> = Arc::default();
        let started = Instant::now();
        let work_passes = passes.clone();
        warmup::start(&supervisor, source, move || {
            let passes = work_passes.clone();
            async move {
                passes.lock().expect("lock").push(Instant::now());
                Ok::<_, String>(WarmupStats {
                    source,
                    warmed: 2,
                    pruned: 1,
                })
            }
        })
        .expect("warmup starts");
        // Row writes run on blocking threads while the paused clock jumps,
        // so the bound is wall-clock time.
        let bound = std::time::Instant::now() + Duration::from_secs(30);
        while passes.lock().expect("lock").len() < 2 {
            assert!(std::time::Instant::now() < bound, "{source:?} warms");
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        let at = passes.lock().expect("lock").clone();
        let first = at[0].duration_since(started);
        assert!(
            first >= delay && first < delay + Duration::from_secs(1),
            "{source:?} first pass after {first:?}"
        );
        let gap = at[1].duration_since(at[0]);
        assert!(
            gap >= warmup::WARMUP_INTERVAL
                && gap <= warmup::WARMUP_INTERVAL + warmup::WARMUP_JITTER + Duration::from_secs(1),
            "{source:?} rebuilds after {gap:?}"
        );
    }
    supervisor.shutdown(Duration::from_secs(5)).await;
}

/// `track_hidden` users stay out of the Subsonic now-playing list.
#[tokio::test]
async fn compat_now_playing_serves_full_sessions_only() {
    let rig = rig();
    rig.set_visibility("user-hidden", "track_hidden");
    start(&rig, "user-1", "track-1", "local").await;
    start(&rig, "user-hidden", "track-1", "local").await;

    let served = rig.deps.presence.compat_now_playing();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].1, "track-1");
    assert_eq!(served[0].0.track_name, "Roads");
}

// ---------------------------------------------------------------------------
// Scrobble forwarding
// ---------------------------------------------------------------------------

/// The recording the scrobbled play names.
const RECORDING: &str = "98255a8c-017a-4bc7-8dd6-1fa36124572b";
/// The ListenBrainz scrobble job kind.
const LISTENBRAINZ_JOBS: droppedneedle_kernel::jobs::queue::JobKind =
    droppedneedle_kernel::jobs::queue::JobKind::new("scrobble.listenbrainz");

/// One scripted ListenBrainz answer.
struct LbAnswer {
    status: u16,
    body: &'static str,
}

impl LbAnswer {
    fn new(status: u16, body: &'static str) -> Self {
        Self { status, body }
    }
}

/// ListenBrainz on loopback: records every listen with the token that sent
/// it, and answers from that token's script, then 200.
#[derive(Clone, Default)]
struct ListenBrainz {
    scripts: Arc<Mutex<HashMap<String, std::collections::VecDeque<LbAnswer>>>>,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
}

impl ListenBrainz {
    fn script(&self, token: &str, answers: Vec<LbAnswer>) {
        self.scripts
            .lock()
            .expect("lock")
            .insert(token.to_owned(), answers.into());
    }

    fn seen(&self) -> Vec<(String, Value)> {
        self.seen.lock().expect("lock").clone()
    }

    /// Serve it, and the outside world with ListenBrainz at it.
    async fn outbound(&self) -> droppedneedle::bootstrap::Outbound {
        let router = Router::new()
            .route("/1/submit-listens", axum::routing::post(listen))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        droppedneedle::bootstrap::Outbound {
            listenbrainz: origin,
            ..crate::common::closed_outbound()
        }
    }
}

async fn listen(
    axum::extract::State(lb): axum::extract::State<ListenBrainz>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::response::Response {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Token "))
        .unwrap_or("")
        .to_owned();
    lb.seen.lock().expect("lock").push((token.clone(), body));
    let answer = lb
        .scripts
        .lock()
        .expect("lock")
        .get_mut(&token)
        .and_then(std::collections::VecDeque::pop_front)
        .unwrap_or_else(|| LbAnswer::new(200, r#"{"status": "ok"}"#));
    axum::response::Response::builder()
        .status(answer.status)
        .header("content-type", "application/json")
        .body(Body::from(answer.body))
        .expect("answer builds")
}

/// A user forwarding to ListenBrainz with `token`, linked as the link
/// route stores it.
async fn listener(app: &TestApp, name: &str, token: &str) -> crate::common::Login {
    let login = app.login_user(name).await;
    let (crypto, _) =
        droppedneedle_kernel::runtime_config::Crypto::load_or_generate(&app.config.config_dir())
            .expect("the app's key reads");
    let sealed = crypto
        .encrypt(&json!({"user_token": token, "username": name}).to_string())
        .expect("link seals");
    let db = app.db();
    db.execute(
        "INSERT INTO user_connections (user_id, service, connection_data, enabled, created_at, \
         updated_at) VALUES (?1, 'listenbrainz', ?2, 1, '', '')",
        [&login.user_id, &sealed],
    )
    .expect("link stored");
    db.execute(
        "INSERT INTO user_listening_prefs (user_id, scrobble_to_lastfm, scrobble_to_listenbrainz, \
         navidrome_handles_external_scrobbles, updated_at) VALUES (?1, 0, 1, 0, '') \
         ON CONFLICT (user_id) DO UPDATE SET scrobble_to_listenbrainz = 1",
        [&login.user_id],
    )
    .expect("prefs saved");
    login
}

/// Submit one play of Roads a minute ago; its time and the play's id.
async fn submit_play(app: &TestApp, user: &crate::common::Login) -> (Value, i64, String) {
    let played_at = droppedneedle_kernel::clock::SystemClock.now_unix() - 60;
    let (status, answer) = app
        .call(
            "POST",
            "/api/v3/scrobble/submit",
            &[("authorization", user.authorization.as_str())],
            Some(json!({
                "track_name": "Roads",
                "artist_name": "Portishead",
                "album_name": "Dummy",
                "timestamp": played_at,
                "duration_ms": 240_000,
                "mbid": RECORDING,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let play_id = app
        .db()
        .query_row(
            "SELECT id FROM library_play_history WHERE user_id = ?1",
            [&user.user_id],
            |row| row.get(0),
        )
        .expect("the play is recorded");
    (answer, played_at, play_id)
}

/// The jobs table and its manual clock.
fn job_table(
    app: &TestApp,
) -> (
    droppedneedle_kernel::jobs::queue::JobQueue,
    droppedneedle::bootstrap::jobs::scripted::ScriptedJobs,
) {
    let jobs = &app.state().jobs;
    (
        jobs.queue().expect("the app has a jobs table").clone(),
        jobs.scripted().expect("test hooks are on").clone(),
    )
}

/// Wait for the play's job of `kind` to read as `ok` says.
async fn scrobble_job(
    app: &TestApp,
    queue: &droppedneedle_kernel::jobs::queue::JobQueue,
    kind: droppedneedle_kernel::jobs::queue::JobKind,
    play_id: &str,
    what: &str,
    ok: impl Fn(&droppedneedle_kernel::jobs::queue::JobStatus) -> bool,
) -> droppedneedle_kernel::jobs::queue::JobStatus {
    app.until(what, || async {
        queue
            .latest(kind, play_id)
            .await
            .expect("the jobs table reads")
            .filter(|row| ok(row))
    })
    .await
}

/// A listen ListenBrainz cannot take now (503) waits in the jobs table
/// through a restart, and is sent again once it can: the same request,
/// once, and then the job is done.
#[tokio::test]
async fn an_undelivered_scrobble_survives_restart() {
    use droppedneedle_kernel::jobs::queue::State;

    let lb = ListenBrainz::default();
    lb.script("lb-token", vec![LbAnswer::new(503, r#"{"code": 503}"#)]);
    let app = TestApp::start_hooked_with("scrobble-restart", lb.outbound().await).await;
    let user = listener(&app, "listener", "lb-token").await;

    let (answer, played_at, play_id) = submit_play(&app, &user).await;
    assert_eq!(answer["accepted"], true);
    assert_eq!(answer["services"]["listenbrainz"]["queued"], true);
    let (queue, _) = job_table(&app);
    let waiting = scrobble_job(
        &app,
        &queue,
        LISTENBRAINZ_JOBS,
        &play_id,
        "the refused listen waits",
        |row| row.state == State::Queued && row.attempts == 1,
    )
    .await;
    assert_eq!(waiting.last_error.as_deref(), Some("SERVICE_UNAVAILABLE"));

    let app = app.restart().await;
    let (queue, kit) = job_table(&app);
    kit.clock().set(waiting.not_before_at);
    queue.wake();
    scrobble_job(
        &app,
        &queue,
        LISTENBRAINZ_JOBS,
        &play_id,
        "the listen lands",
        |row| row.state == State::Done,
    )
    .await;

    let seen = lb.seen();
    assert_eq!(seen.len(), 2, "one refused send and one that landed");
    assert_eq!(seen[1], seen[0], "both sends are the same request");
    let (token, body) = &seen[1];
    assert_eq!(token, "lb-token");
    assert_eq!(body["listen_type"], "single");
    let listen = &body["payload"][0];
    assert_eq!(listen["listened_at"], played_at);
    assert_eq!(listen["track_metadata"]["release_name"], "Dummy");
    assert_eq!(
        listen["track_metadata"]["additional_info"]["recording_mbid"],
        RECORDING
    );
    app.stop().await;
}

/// A Last.fm scrobble whose job meets admin settings that cannot be read
/// waits for them, rather than reading them as "Last.fm off" and dropping
/// the play for good.
#[tokio::test]
async fn an_unreadable_last_fm_section_holds_the_scrobble() {
    use droppedneedle_kernel::jobs::queue::{JobKind, State};

    let app = TestApp::start_hooked_with(
        "scrobble-lastfm-unreadable",
        crate::common::closed_outbound(),
    )
    .await;
    let user = app.login_user("listener").await;
    let (crypto, _) =
        droppedneedle_kernel::runtime_config::Crypto::load_or_generate(&app.config.config_dir())
            .expect("the app's key reads");
    let seal = |value: &str| crypto.encrypt(value).expect("secret seals");
    let document = json!({
        "configured": true,
        "api_key": seal("lfm-key"),
        "shared_secret": seal("lfm-secret"),
        "username": "listener",
        "session_key": seal("lfm-session"),
    });
    let db = app.db();
    db.execute(
        "INSERT INTO user_connections (user_id, service, connection_data, enabled, created_at, \
         updated_at) VALUES (?1, 'lastfm', ?2, 1, '', '')",
        [&user.user_id, &document.to_string()],
    )
    .expect("link stored");
    db.execute(
        "INSERT INTO user_listening_prefs (user_id, scrobble_to_lastfm, scrobble_to_listenbrainz, \
         navidrome_handles_external_scrobbles, updated_at) VALUES (?1, 1, 0, 0, '') \
         ON CONFLICT (user_id) DO UPDATE SET scrobble_to_lastfm = 1",
        [&user.user_id],
    )
    .expect("prefs saved");
    drop(db);
    let (app, ()) = app
        .while_stopped(|config| {
            rusqlite::Connection::open(&config.library_db_path)
                .expect("database opens")
                .execute(
                    "INSERT INTO config_sections (key, value_json, revision, updated_at) \
                     VALUES ('lastfm_settings', '{\"enabled\": \"yes\"}', 1, 1) \
                     ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
                    [],
                )
                .expect("section stored");
        })
        .await;

    let (_, _, play_id) = submit_play(&app, &user).await;
    let (queue, _) = job_table(&app);
    let held = scrobble_job(
        &app,
        &queue,
        JobKind::new("scrobble.lastfm"),
        &play_id,
        "the scrobble waits for the settings",
        |row| row.state != State::Running && row.attempts == 1,
    )
    .await;
    assert_eq!(
        (held.state, held.last_error.as_deref()),
        (State::Queued, Some("SETTINGS_UNREADABLE"))
    );
    app.stop().await;
}

// ---------------------------------------------------------------------------
// Play history on the real database
// ---------------------------------------------------------------------------

/// The ids each of the user's plays of `title` is filed under: track,
/// album, artist. An id that was not stored reads as empty.
fn filed_under(app: &TestApp, user_id: &str, title: &str) -> Vec<(String, String, String)> {
    let db = app.db();
    let mut plays = db
        .prepare(
            "SELECT COALESCE(local_track_id, ''), COALESCE(local_album_id, ''), \
             COALESCE(local_artist_id, '') FROM library_play_history \
             WHERE user_id = ?1 AND track_name = ?2",
        )
        .expect("history query");
    plays
        .query_map([user_id, title], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("history reads")
        .collect::<Result<_, _>>()
        .expect("history rows")
}

/// What the catalog holds for the track `title`: its id, its album and its
/// first credited artist.
fn catalog_ids(app: &TestApp, title: &str) -> (String, String, String) {
    app.db()
        .query_row(
            "SELECT t.id, t.local_album_id, x.local_artist_id FROM local_tracks t \
             JOIN local_track_artists x ON x.local_track_id = t.id AND x.position = 0 \
             WHERE t.title = ?1",
            [title],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the track is in the catalog")
}

/// The titles a Jellyfin track list answers with.
async fn jellyfin_titles(rig: &CompatApp, query: &str, auth: &[(&str, &str)]) -> Vec<String> {
    let uri = format!("/jellyfin/Items?IncludeItemTypes=Audio&{query}");
    let (status, body) = rig.app.call("GET", &uri, auth, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["Items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["Name"].as_str().expect("name").to_owned())
        .collect()
}

/// A play from a Subsonic app, a Jellyfin app and the web player is each
/// filed under the track, its album and its artist, for the listener
/// alone. An app's now-playing entry carries the cover the web player's
/// entry carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plays_from_every_player_are_filed_under_the_track() {
    const GROUP: &str = "744c0a9e-5a4e-3b0e-8f0f-6a1f1b2c3d4e";
    let rig = CompatApp::start("playback-history").await;
    let app = &rig.app;
    let subsonic = rig.subsonic_auth("symfonium");
    let jellyfin_value = rig.jellyfin_auth();
    let jellyfin = [("X-Emby-Authorization", jellyfin_value.as_str())];
    let (status, signed_in) = app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": PLAYER,
                "password": format!("{PLAYER}-password-1234"),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{signed_in}");
    let web_value = format!("Bearer {}", signed_in["token"].as_str().expect("token"));
    let web = [("authorization", web_value.as_str())];
    let alice: String = app
        .db()
        .query_row(
            "SELECT id FROM auth_users WHERE username = ?1",
            [PLAYER],
            |row| row.get(0),
        )
        .expect("the player's id");

    let one = catalog_ids(app, "One");
    let airbag = catalog_ids(app, "Airbag");
    let teardrop = catalog_ids(app, "Teardrop");
    // The album of "One" is matched to a release group; the others are not.
    app.db()
        .execute(
            "INSERT INTO local_album_external_identities \
             (local_album_id, release_group_mbid, decision_source, selected_at) \
             VALUES (?1, ?2, 'manual', 1)",
            [one.1.as_str(), GROUP],
        )
        .expect("the album is matched");

    // Now playing: the app's entry and the web player's carry one cover.
    let now_playing = rest(
        &format!("scrobble?id=tr-{}&submission=false", one.0),
        &subsonic,
    );
    let (status, body) = app.call("GET", &now_playing, &[], None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/playback/start",
            &web,
            Some(json!({"track_id": one.0, "source": "local", "device": "web"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, feed) = app.call("GET", "/api/v3/now-playing", &web, None).await;
    assert_eq!(status, StatusCode::OK, "{feed}");
    let cover = |device: &str| {
        let id = format!("{alice}:{device}");
        named(&feed["sessions"], "id", &id)["cover_url"].clone()
    };
    assert_eq!(
        cover("compat"),
        json!(format!("/api/v3/covers/release-group/{GROUP}"))
    );
    assert_eq!(cover("compat"), cover("web"));
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/playback/stop",
            &web,
            Some(json!({
                "track_id": one.0,
                "source": "local",
                "device": "web",
                "ignore_scrobble": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // One play from each kind of player, each of another track.
    let scrobble = rest(
        &format!("scrobble?id=tr-{}&submission=true", one.0),
        &subsonic,
    );
    let (status, body) = app.call("GET", &scrobble, &[], None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, listed) = app
        .call(
            "GET",
            "/jellyfin/Items?IncludeItemTypes=Audio",
            &jellyfin,
            None,
        )
        .await;
    let airbag_item = named(&listed["Items"], "Name", "Airbag")["Id"].clone();
    let (status, _) = app
        .call(
            "POST",
            "/jellyfin/Sessions/Playing/Stopped",
            &jellyfin,
            Some(json!({ "ItemId": airbag_item })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for uri in ["/api/v3/playback/start", "/api/v3/playback/stop"] {
        let body = json!({"track_id": teardrop.0, "source": "local", "device": "web"});
        let (status, answer) = app.call("POST", uri, &web, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {answer}");
    }

    for (title, ids) in [("One", &one), ("Airbag", &airbag), ("Teardrop", &teardrop)] {
        let plays = app
            .until(&format!("the play of {title} is in history"), || async {
                Some(filed_under(app, &alice, title)).filter(|plays| !plays.is_empty())
            })
            .await;
        assert_eq!(plays, vec![ids.clone()], "{title}");
    }

    // Jellyfin narrows a track list by the listener's own plays.
    let mut played = jellyfin_titles(&rig, "Filters=IsPlayed", &jellyfin).await;
    played.sort();
    assert_eq!(played, ["Airbag", "One", "Teardrop"]);
    let unplayed = jellyfin_titles(&rig, "IsPlayed=false", &jellyfin).await;
    assert_eq!(unplayed.len(), 3, "{unplayed:?}");
    assert!(!unplayed.iter().any(|title| played.contains(title)));

    // Another user has none of these plays.
    let bob = app.login_user("bob").await;
    let (status, created) = app
        .call(
            "POST",
            "/api/v3/me/app-passwords",
            &[("authorization", bob.authorization.as_str())],
            Some(json!({"name": "Journey"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let bob_secret = created["secret"].as_str().expect("secret");
    let bob_value = format!("MediaBrowser Client=\"Journey\", Token=\"{bob_secret}\"");
    let bob_jellyfin = [("X-Emby-Authorization", bob_value.as_str())];
    let bobs = jellyfin_titles(&rig, "Filters=IsPlayed", &bob_jellyfin).await;
    assert!(bobs.is_empty(), "{bobs:?}");
    let bob_subsonic = format!("u=bob&p={bob_secret}&v=1.16.1&c=symfonium&f=json");
    let (status, recent) = app
        .call(
            "GET",
            &rest("getAlbumList2?type=recent", &bob_subsonic),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recent}");
    let albums = recent["subsonic-response"]["albumList2"]["album"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(albums, 0, "{recent}");
    let (_, song) = app
        .call(
            "GET",
            &rest(&format!("getSong?id=tr-{}", one.0), &bob_subsonic),
            &[],
            None,
        )
        .await;
    let song = &song["subsonic-response"]["song"];
    assert_eq!(song["title"], "One", "{song}");
    assert!(song.get("playCount").is_none(), "{song}");

    rig.stop().await;
}

/// A web player session on a library track (start, progress, stop) is
/// counted once and player apps see it by the track's library ids: the
/// track's play count and the recently played albums.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_session_counts_in_player_apps() {
    let rig = CompatApp::start("playback-session").await;
    let app = &rig.app;
    let subsonic = rig.subsonic_auth("symfonium");
    let (status, signed_in) = app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": PLAYER,
                "password": format!("{PLAYER}-password-1234"),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{signed_in}");
    let web_value = format!("Bearer {}", signed_in["token"].as_str().expect("token"));
    let web = [("authorization", web_value.as_str())];
    let alice: String = app
        .db()
        .query_row(
            "SELECT id FROM auth_users WHERE username = ?1",
            [PLAYER],
            |row| row.get(0),
        )
        .expect("the player's id");
    let one = catalog_ids(app, "One");

    for (uri, body) in [
        (
            "/api/v3/playback/start",
            json!({"track_id": one.0, "source": "local", "device": "web"}),
        ),
        (
            "/api/v3/playback/progress",
            json!({"track_id": one.0, "source": "local", "device": "web",
                   "position_ms": 1_000, "is_paused": false}),
        ),
        (
            "/api/v3/playback/stop",
            json!({"track_id": one.0, "source": "local", "device": "web",
                   "position_ms": 3_600_000}),
        ),
    ] {
        let (status, answer) = app.call("POST", uri, &web, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {answer}");
    }
    let plays = app
        .until("the session's play is in history", || async {
            Some(filed_under(app, &alice, "One")).filter(|plays| !plays.is_empty())
        })
        .await;
    assert_eq!(plays, vec![one.clone()]);

    let (status, song) = app
        .call(
            "GET",
            &rest(&format!("getSong?id=tr-{}", one.0), &subsonic),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{song}");
    assert_eq!(song["subsonic-response"]["song"]["playCount"], 1, "{song}");
    let (status, recent) = app
        .call(
            "GET",
            &rest("getAlbumList2?type=recent", &subsonic),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recent}");
    let albums: Vec<&str> = recent["subsonic-response"]["albumList2"]["album"]
        .as_array()
        .expect("albums")
        .iter()
        .map(|album| album["id"].as_str().expect("id"))
        .collect();
    assert_eq!(albums, [format!("al-{}", one.1)], "{recent}");

    rig.stop().await;
}

/// The recording and release group stored with each of the user's plays
/// of `title`.
fn stored_ids(app: &TestApp, user_id: &str, title: &str) -> Vec<(Option<String>, Option<String>)> {
    let db = app.db();
    let mut plays = db
        .prepare(
            "SELECT recording_mbid, release_group_mbid FROM library_play_history \
             WHERE user_id = ?1 AND track_name = ?2",
        )
        .expect("history query");
    plays
        .query_map([user_id, title], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("history reads")
        .collect::<Result<_, _>>()
        .expect("history rows")
}

/// The web player names the library track it played. The play is filed
/// under that track, or under the track an old id continued into, with the
/// track's own recording whatever the body sent. An id the library does
/// not know keeps the play by name with no ids. A track id on a play from
/// another server or from YouTube is refused and nothing is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_player_plays_are_filed_under_the_named_track() {
    const RECORDING: &str = "0a8e8d55-4b83-4f8a-9732-fbb5ded9f344";
    const GROUP: &str = "744c0a9e-5a4e-3b0e-8f0f-6a1f1b2c3d4e";
    const SENT: &str = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    let rig = CompatApp::start("playback-web-player").await;
    let app = &rig.app;
    let (status, signed_in) = app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": PLAYER,
                "password": format!("{PLAYER}-password-1234"),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{signed_in}");
    let web_value = format!("Bearer {}", signed_in["token"].as_str().expect("token"));
    let web = [("authorization", web_value.as_str())];
    let alice: String = app
        .db()
        .query_row(
            "SELECT id FROM auth_users WHERE username = ?1",
            [PLAYER],
            |row| row.get(0),
        )
        .expect("the player's id");

    let one = catalog_ids(app, "One");
    let airbag = catalog_ids(app, "Airbag");
    let teardrop = catalog_ids(app, "Teardrop");
    let db = app.db();
    db.execute(
        "UPDATE local_tracks SET embedded_recording_mbid = ?2 WHERE id = ?1",
        [one.0.as_str(), RECORDING],
    )
    .expect("the track has a recording");
    db.execute(
        "INSERT INTO local_album_external_identities \
         (local_album_id, release_group_mbid, decision_source, selected_at) \
         VALUES (?1, ?2, 'manual', 1)",
        [one.1.as_str(), GROUP],
    )
    .expect("the album is matched");
    // Airbag's file went missing and the track continued into Teardrop.
    db.execute(
        "UPDATE local_tracks SET availability = 'missing' WHERE id = ?1",
        [airbag.0.as_str()],
    )
    .expect("the track is missing");
    db.execute(
        "INSERT INTO local_track_aliases (alias, local_track_id, created_at) VALUES (?1, ?2, 1)",
        [airbag.0.as_str(), teardrop.0.as_str()],
    )
    .expect("the old id is an alias");
    drop(db);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    for play in [
        json!({
            "track_name": "One",
            "artist_name": "U2",
            "timestamp": now - 300,
            "track_id": one.0,
            "mbid": SENT,
            "release_group_mbid": SENT,
            "source": "local",
        }),
        json!({
            "track_name": "Ghost",
            "artist_name": "Nobody",
            "timestamp": now - 240,
            "track_id": "no-such-track",
            "mbid": SENT,
            "source": "local",
        }),
        json!({
            "track_name": "Queued Long Ago",
            "artist_name": "Radiohead",
            "timestamp": now - 180,
            "track_id": airbag.0,
        }),
    ] {
        let (status, body) = app
            .call("POST", "/api/v3/scrobble/submit", &web, Some(play))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(filed_under(app, &alice, "One"), vec![one.clone()]);
    assert_eq!(
        stored_ids(app, &alice, "One"),
        vec![(Some(RECORDING.to_owned()), Some(GROUP.to_owned()))]
    );
    let empty = (String::new(), String::new(), String::new());
    assert_eq!(filed_under(app, &alice, "Ghost"), vec![empty]);
    assert_eq!(stored_ids(app, &alice, "Ghost"), vec![(None, None)]);
    assert_eq!(
        filed_under(app, &alice, "Queued Long Ago"),
        vec![teardrop.clone()]
    );

    for source in ["jellyfin", "YouTube"] {
        let play = json!({
            "track_name": "Refused",
            "artist_name": "U2",
            "timestamp": now - 120,
            "track_id": one.0,
            "source": source,
        });
        let (status, body) = app
            .call("POST", "/api/v3/scrobble/submit", &web, Some(play))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{source}: {body}");
        assert_eq!(error_code(&body), "INVALID_INPUT", "{source}: {body}");
    }
    assert!(filed_under(app, &alice, "Refused").is_empty());

    rig.stop().await;
}

// ---------------------------------------------------------------------------
// Older plays: rows that name no track, album or artist
// ---------------------------------------------------------------------------

/// One play row as an older version stored it: names, and no catalog id.
#[derive(Default)]
struct OldPlay<'a> {
    id: &'a str,
    user: &'a str,
    track: &'a str,
    artist: &'a str,
    album: Option<&'a str>,
    recording: Option<&'a str>,
    group: Option<&'a str>,
    length_ms: Option<i64>,
    source: Option<&'a str>,
    at: &'a str,
}

fn store_old_play(db: &rusqlite::Connection, play: &OldPlay<'_>) {
    db.execute(
        "INSERT INTO library_play_history (id, user_id, track_name, artist_name, album_name, \
         recording_mbid, release_group_mbid, duration_ms, source, played_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            play.id,
            play.user,
            play.track,
            play.artist,
            play.album,
            play.recording,
            play.group,
            play.length_ms,
            play.source,
            play.at,
        ],
    )
    .expect("the old play is stored");
}

/// The track, album and artist a play row is filed under, empty when not.
fn filed(db: &rusqlite::Connection, play_id: &str) -> (String, String, String) {
    db.query_row(
        "SELECT COALESCE(local_track_id, ''), COALESCE(local_album_id, ''), \
         COALESCE(local_artist_id, '') FROM library_play_history WHERE id = ?1",
        [play_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("the play row is there")
}

/// Plays stored without a track are filed by the next repair after a scan
/// and then count in player apps: by names when they were plays of this
/// library, by recording id when they came through a connected server. A
/// play of the library with a recording id and names the catalog lacks,
/// and a connected server's play with no recording id, stay as they were.
/// Each
/// row stays with its listener, and the report an administrator reads
/// counts every row once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_plays_are_filed_and_count_in_player_apps() {
    const ONE: &str = "0a8e8d55-4b83-4f8a-9732-fbb5ded9f344";
    const TEARDROP: &str = "9f9cf187-d6f9-437f-9d98-d59cdbd52757";
    let rig = CompatApp::start("playback-older-plays").await;
    let app = &rig.app;
    let subsonic = rig.subsonic_auth("symfonium");
    let jellyfin_value = rig.jellyfin_auth();
    let jellyfin = [("X-Emby-Authorization", jellyfin_value.as_str())];
    let user_id = |name: &str| -> String {
        app.db()
            .query_row(
                "SELECT id FROM auth_users WHERE username = ?1",
                [name],
                |row| row.get(0),
            )
            .expect("the user's id")
    };
    let alice = user_id(PLAYER);
    app.login_user("bob").await;
    let bob = user_id("bob");

    let one = catalog_ids(app, "One");
    let airbag = catalog_ids(app, "Airbag");
    let teardrop = catalog_ids(app, "Teardrop");
    let db = app.db();
    for (track, recording) in [(&one.0, ONE), (&teardrop.0, TEARDROP)] {
        db.execute(
            "INSERT INTO local_track_external_identities \
             (local_track_id, recording_mbid, decision_source, selected_at) \
             VALUES (?1, ?2, 'manual', 1)",
            [track.as_str(), recording],
        )
        .expect("the track has an accepted recording");
    }
    let airbag_ms: i64 = db
        .query_row(
            "SELECT CAST(duration_seconds * 1000 AS INTEGER) FROM local_tracks WHERE id = ?1",
            [airbag.0.as_str()],
            |row| row.get(0),
        )
        .expect("the track has a length");
    let old = [
        // A recording id held by one track, under a title the catalog
        // does not have: the recording files no play of the library.
        OldPlay {
            id: "old-recording",
            user: &alice,
            track: "One (Remastered)",
            artist: "U2",
            recording: Some(ONE),
            source: Some("local"),
            at: "2026-01-01T10:00:00+00:00",
            ..OldPlay::default()
        },
        // Names and a length, no source.
        OldPlay {
            id: "old-names",
            user: &alice,
            track: "AIRBAG",
            artist: "Radiohead",
            album: Some("OK Computer"),
            length_ms: Some(airbag_ms + 900),
            at: "2026-01-02T10:00:00+00:00",
            ..OldPlay::default()
        },
        // The same names from a connected server: not a clear match.
        OldPlay {
            id: "old-remote-names",
            user: &alice,
            track: "Airbag",
            artist: "Radiohead",
            album: Some("OK Computer"),
            length_ms: Some(airbag_ms),
            source: Some("jellyfin"),
            at: "2026-01-03T10:00:00+00:00",
            ..OldPlay::default()
        },
        // A connected server's play of a recording the library holds once.
        OldPlay {
            id: "old-remote-recording",
            user: &alice,
            track: "Teardrop",
            artist: "Massive Attack",
            recording: Some(TEARDROP),
            source: Some("plex"),
            at: "2026-01-04T10:00:00+00:00",
            ..OldPlay::default()
        },
        // Another listener's play of a track the first also played.
        OldPlay {
            id: "old-other-listener",
            user: &bob,
            track: "One",
            artist: "U2",
            album: Some("Achtung Baby"),
            recording: Some(ONE),
            at: "2026-01-05T10:00:00+00:00",
            ..OldPlay::default()
        },
    ];
    for play in &old {
        store_old_play(&db, play);
    }
    drop(db);

    app.scan_library().await;
    let job: String = app
        .until("the older plays are filed", || async {
            app.db()
                .query_row(
                    "SELECT j.id FROM library_operation_jobs j \
                     JOIN library_repair_snapshots s ON s.job_id = j.id \
                     WHERE s.purpose = 'play_history_filing' \
                     AND j.state = 'succeeded'",
                    [],
                    |row| row.get(0),
                )
                .ok()
        })
        .await;

    let db = app.db();
    assert_eq!(filed(&db, "old-names"), airbag);
    assert_eq!(filed(&db, "old-remote-recording"), teardrop);
    assert_eq!(filed(&db, "old-other-listener"), one);
    let empty = (String::new(), String::new(), String::new());
    assert_eq!(filed(&db, "old-remote-names"), empty);
    assert_eq!(filed(&db, "old-recording"), empty);
    let untouched: Vec<(String, String, String)> = db
        .prepare(
            "SELECT id, user_id, track_name || '|' || played_at FROM library_play_history \
             ORDER BY id",
        )
        .expect("history query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("history reads")
        .collect::<Result<_, _>>()
        .expect("history rows");
    let stored: Vec<(String, String, String)> = {
        let mut rows: Vec<_> = old
            .iter()
            .map(|play| {
                (
                    play.id.to_owned(),
                    play.user.to_owned(),
                    format!("{}|{}", play.track, play.at),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    assert_eq!(untouched, stored, "no row is added, renamed or moved");
    drop(db);

    // An album whose only plays are older rows is in the app lists.
    let (status, recent) = app
        .call(
            "GET",
            &rest("getAlbumList2?type=recent", &subsonic),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{recent}");
    let names: Vec<&str> = recent["subsonic-response"]["albumList2"]["album"]
        .as_array()
        .expect("recent albums")
        .iter()
        .map(|album| album["name"].as_str().expect("album name"))
        .collect();
    assert_eq!(names, ["Mezzanine", "OK Computer"]);
    let (status, sorted) = app
        .call(
            "GET",
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=DatePlayed&SortOrder=Descending",
            &jellyfin,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{sorted}");
    let first: Vec<&str> = sorted["Items"]
        .as_array()
        .expect("albums")
        .iter()
        .take(2)
        .map(|album| album["Name"].as_str().expect("album name"))
        .collect();
    assert_eq!(first, ["Mezzanine", "OK Computer"]);
    let (_, song) = app
        .call(
            "GET",
            &rest(&format!("getSong?id=tr-{}", one.0), &subsonic),
            &[],
            None,
        )
        .await;
    assert!(
        song["subsonic-response"]["song"].get("playCount").is_none(),
        "the other listener's play is not counted: {song}"
    );

    // What an administrator reads about the job.
    let admin = app.login_admin().await;
    let (status, report) = app
        .call(
            "GET",
            &format!("/api/v3/library/operations/{job}"),
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let summary = &report["repair_summary"];
    assert_eq!(summary["purpose"], "play_history_filing", "{report}");
    assert_eq!(
        summary["counts_by_finding"],
        json!({"filed": 3, "left": 2}),
        "{report}"
    );
    assert_eq!(
        summary["counts_by_reason"],
        json!({
            "RECORDING": 1,
            "NAMES": 2,
            "REMOTE_WITHOUT_RECORDING": 1,
            "NAMES_WITHOUT_ALBUM_OR_LENGTH": 1,
        }),
        "{report}"
    );

    // A play made now through a connected server or as a preview follows
    // the same rule as it is written: filed by a recording the library
    // holds once, and kept by name otherwise.
    let (status, signed_in) = app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": PLAYER,
                "password": format!("{PLAYER}-password-1234"),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{signed_in}");
    let web_value = format!("Bearer {}", signed_in["token"].as_str().expect("token"));
    let web = [("authorization", web_value.as_str())];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    for play in [
        json!({
            "track_name": "Teardrop (Live)",
            "artist_name": "Massive Attack",
            "timestamp": now - 120,
            "mbid": TEARDROP,
            "source": "Plex",
        }),
        json!({
            "track_name": "Paranoid Android",
            "artist_name": "Radiohead",
            "album_name": "OK Computer",
            "timestamp": now - 60,
            "source": "youtube",
        }),
    ] {
        let (status, body) = app
            .call("POST", "/api/v3/scrobble/submit", &web, Some(play))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(
        filed_under(app, &alice, "Teardrop (Live)"),
        vec![teardrop.clone()]
    );
    assert_eq!(
        filed_under(app, &alice, "Paranoid Android"),
        vec![empty.clone()]
    );

    rig.stop().await;
}

/// A small catalog for the filing rules: one artist with two albums, each
/// matched to a release group, and a guest artist.
const FILING_CATALOG: &str = "
    INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
    VALUES ('a1', 'Artist', 'artist', 'group', 1, 1), ('a2', 'Guest', 'guest', 'group', 1, 1);
    INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_id,
        grouping_source, created_at, updated_at)
    VALUES ('al1', 'r1', 'g1', 'First', 'first', 'a1', 'automatic', 1, 1),
           ('al2', 'r1', 'g2', 'Second', 'second', 'a1', 'automatic', 1, 1);
    INSERT INTO local_album_external_identities
        (local_album_id, release_group_mbid, decision_source, selected_at)
    VALUES ('al1', '00000000-0000-4000-8000-0000000000a1', 'manual', 1),
           ('al2', '00000000-0000-4000-8000-0000000000a2', 'manual', 1);
    INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash,
        file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name,
        artist_name_folded, album_title, album_title_folded, duration_seconds,
        embedded_recording_mbid, availability, file_format, ingest_source, imported_at,
        membership_source)
    VALUES
      ('t1', 'al1', 'r1', '/m/1', '1', 'h1', 1, 1, 's', 'Alpha', 'alpha', 'Artist', 'artist',
       'First', 'first', 200.0, NULL, 'indexed', 'flac', 'scan', 1, 'automatic'),
      ('t2', 'al1', 'r1', '/m/2', '2', 'h2', 1, 1, 's', 'Beta', 'beta', 'Artist', 'artist',
       'First', 'first', 180.0, '00000000-0000-4000-8000-000000000002', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t3', 'al1', 'r1', '/m/3', '3', 'h3', 1, 1, 's', 'Gamma', 'gamma', 'Artist', 'artist',
       'First', 'first', 190.0, '00000000-0000-4000-8000-000000000003', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t4', 'al1', 'r1', '/m/4', '4', 'h4', 1, 1, 's', 'Shared', 'shared', 'Artist', 'artist',
       'First', 'first', 210.0, '00000000-0000-4000-8000-000000000005', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t5', 'al2', 'r1', '/m/5', '5', 'h5', 1, 1, 's', 'Shared', 'shared', 'Artist', 'artist',
       'Second', 'second', 210.0, '00000000-0000-4000-8000-000000000005', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t6', 'al2', 'r1', '/m/6', '6', 'h6', 1, 1, 's', 'Twin', 'twin', 'Artist', 'artist',
       'Second', 'second', 100.0, NULL, 'indexed', 'flac', 'scan', 1, 'automatic'),
      ('t7', 'al2', 'r1', '/m/7', '7', 'h7', 1, 1, 's', 'Twin', 'twin', 'Artist', 'artist',
       'Second', 'second', 100.0, NULL, 'indexed', 'flac', 'scan', 1, 'automatic'),
      ('t8', 'al2', 'r1', '/m/8', '8', 'h8', 1, 1, 's', 'Lost', 'lost', 'Artist', 'artist',
       'Second', 'second', 120.0, '00000000-0000-4000-8000-000000000008', 'missing', 'flac',
       'scan', 1, 'automatic'),
      ('t9', 'al2', 'r1', '/m/9', '9', 'h9', 1, 1, 's', 'Other Take', 'other take', 'Artist',
       'artist', 'Second', 'second', 130.0, '00000000-0000-4000-8000-000000000009', 'indexed',
       'flac', 'scan', 1, 'automatic'),
      ('t10', 'al1', 'r1', '/m/10', '10', 'h10', 1, 1, 's', 'Song', 'song', 'Artist', 'artist',
       'First', 'first', 215.0, NULL, 'indexed', 'flac', 'scan', 1, 'automatic'),
      ('t11', 'al2', 'r1', '/m/11', '11', 'h11', 1, 1, 's', 'Song', 'song', 'Artist', 'artist',
       'Second', 'second', 215.0, '00000000-0000-4000-8000-000000000010', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t12', 'al1', 'r1', '/m/12', '12', 'h12', 1, 1, 's', 'Tune', 'tune', 'Artist', 'artist',
       'First', 'first', 215.0, '00000000-0000-4000-8000-000000000012', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t13', 'al2', 'r1', '/m/13', '13', 'h13', 1, 1, 's', 'Tune', 'tune', 'Artist', 'artist',
       'Second', 'second', 215.0, '00000000-0000-4000-8000-000000000012', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t14', 'al1', 'r1', '/m/14', '14', 'h14', 1, 1, 's', 'Dup', 'dup', 'Artist', 'artist',
       'First', 'first', 90.0, '00000000-0000-4000-8000-000000000014', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t15', 'al1', 'r1', '/m/15', '15', 'h15', 1, 1, 's', 'Dup', 'dup', 'Artist', 'artist',
       'First', 'first', 90.0, '00000000-0000-4000-8000-000000000014', 'indexed', 'flac',
       'scan', 1, 'automatic'),
      ('t16', 'al1', 'r1', '/m/16', '16', 'h16', 1, 1, 's', 'Hidden', 'hidden', 'Artist',
       'artist', 'First', 'first', 95.0, '00000000-0000-4000-8000-000000000016', 'excluded',
       'flac', 'scan', 1, 'automatic');
    INSERT INTO local_track_external_identities
        (local_track_id, recording_mbid, decision_source, selected_at)
    VALUES ('t1', '00000000-0000-4000-8000-000000000001', 'manual', 1),
           ('t3', '00000000-0000-4000-8000-000000000004', 'manual', 1),
           ('t12', '00000000-0000-4000-8000-000000000013', 'manual', 1);
    INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role)
    VALUES ('t1', 0, 'a2', 'primary'), ('t1', 1, 'a1', 'primary');
";

/// The filing rules, one row each, and the job that applies them in
/// batches. A play of the library is filed by its names and never by its
/// recording id, a play through another server by its recording id and
/// never by its names. A row is filed only when it fits exactly one
/// playable track,
/// under that track, its album and its artist together; every other row
/// is left as it was. The job is stopped after every batch and picked up
/// on a new connection: it still looks at each row once, and its report
/// counts every row it changed and every row it left.
#[test]
fn older_plays_are_filed_only_on_a_clear_match() {
    use droppedneedle_library::repairs::plays;

    let recording = |n: u8| format!("00000000-0000-4000-8000-{n:012}");
    let (r1, r2, r3, r5, r8) = (
        recording(1),
        recording(2),
        recording(3),
        recording(5),
        recording(8),
    );
    let (r10, r12, r14, r16) = (recording(10), recording(12), recording(14), recording(16));
    let r1_upper = r1.to_uppercase();
    let other = recording(99);
    let group2 = "00000000-0000-4000-8000-0000000000a2";
    let named = |id, track, album, length_ms| OldPlay {
        id,
        user: "u1",
        track,
        artist: "Artist",
        album,
        length_ms,
        at: "2026-01-01T00:00:00+00:00",
        ..OldPlay::default()
    };
    let recorded = |id, recording| OldPlay {
        recording: Some(recording),
        ..named(id, "Some title", None, None)
    };
    // Each row with the track it must be filed under, and the reason the
    // report gives.
    let cases: Vec<(OldPlay<'_>, &str, &str)> = vec![
        // A recording id alone files no play of the library: not one a
        // single track holds as its accepted recording,
        (recorded("p01", &r1), "", "NAMES_WITHOUT_ALBUM_OR_LENGTH"),
        // nor one from a tag,
        (recorded("p02", &r2), "", "NAMES_WITHOUT_ALBUM_OR_LENGTH"),
        (recorded("p29", &r3), "", "NAMES_WITHOUT_ALBUM_OR_LENGTH"),
        // nor with an album name beside a title the catalog lacks,
        (
            OldPlay {
                album: Some("First"),
                ..recorded("p03", &r3)
            },
            "",
            "NO_MATCH",
        ),
        (
            OldPlay {
                album: Some("First"),
                ..recorded("p05", &r5)
            },
            "",
            "NO_MATCH",
        ),
        (
            OldPlay {
                album: Some("First"),
                ..recorded("p25", &r14)
            },
            "",
            "NO_MATCH",
        ),
        // nor with a release group.
        (
            OldPlay {
                group: Some(group2),
                ..recorded("p04", &r5)
            },
            "",
            "NAMES_WITHOUT_ALBUM_OR_LENGTH",
        ),
        (
            OldPlay {
                group: Some(group2),
                album: Some("First"),
                ..recorded("p24", &r5)
            },
            "",
            "NO_MATCH",
        ),
        (recorded("p06", &r5), "", "NAMES_WITHOUT_ALBUM_OR_LENGTH"),
        // The album is spelled as no album in the catalog is, and another
        // album holds the recording.
        (
            OldPlay {
                recording: Some(&r10),
                ..named("p30", "Song", Some("Elsewhere"), None)
            },
            "",
            "NO_MATCH",
        ),
        // The row names a deluxe edition by name and release group; the
        // plain edition, matched to that group, holds the recording.
        (
            OldPlay {
                recording: Some(&r10),
                group: Some(group2),
                ..named("p31", "Song", Some("Second (Deluxe Edition)"), None)
            },
            "",
            "NO_MATCH",
        ),
        // The row names a remaster of the song on the album whose track
        // holds the recording.
        (
            OldPlay {
                recording: Some(&r10),
                ..named("p32", "Song (2011 Remaster)", Some("Second"), None)
            },
            "",
            "NO_MATCH",
        ),
        // Names that fit two tracks, with or without a recording id that
        // a third track holds.
        (
            named("p08", "Twin", Some("Second"), None),
            "",
            "SEVERAL_MATCHES",
        ),
        (
            OldPlay {
                recording: Some(&r10),
                ..named("p28", "Twin", Some("Second"), None)
            },
            "",
            "SEVERAL_MATCHES",
        ),
        // Names that fit one track, with a length within two seconds,
        (
            named("p09", "Beta", Some("First"), Some(181_500)),
            "t2",
            "NAMES",
        ),
        // and with a length further off.
        (
            named("p10", "Beta", Some("First"), Some(190_000)),
            "",
            "LENGTH_DISAGREES",
        ),
        // No album name: the length has to agree,
        (named("p11", "Alpha", None, Some(200_000)), "t1", "NAMES"),
        // and a row with neither is left.
        (
            named("p12", "Alpha", None, None),
            "",
            "NAMES_WITHOUT_ALBUM_OR_LENGTH",
        ),
        // The same song on two albums: the names say which, whichever
        // track is known to hold the row's recording.
        (
            OldPlay {
                recording: Some(&r10),
                ..named("p20", "Song", Some("First"), Some(215_000))
            },
            "t10",
            "NAMES",
        ),
        (
            OldPlay {
                recording: Some(&r10),
                ..named("p21", "Song", Some("Second"), None)
            },
            "t11",
            "NAMES",
        ),
        // The row's recording is the track's tag, beside a different
        // accepted one: the track is not known only as other recordings.
        (
            OldPlay {
                recording: Some(&r12),
                ..named("p22", "Tune", Some("First"), Some(215_000))
            },
            "t12",
            "NAMES",
        ),
        // The names fit a track known only as another recording.
        (
            OldPlay {
                recording: Some(&other),
                ..named("p16", "Other Take", Some("Second"), None)
            },
            "",
            "RECORDING_DISAGREES",
        ),
        // A track that is gone from disk is never a match,
        (
            OldPlay {
                recording: Some(&r8),
                ..named("p15", "Lost", Some("Second"), None)
            },
            "",
            "NO_MATCH",
        ),
        // nor one kept out of the library.
        (
            OldPlay {
                recording: Some(&r16),
                ..named("p26", "Hidden", Some("First"), None)
            },
            "",
            "NO_MATCH",
        ),
        // Another listener's play stays that listener's.
        (
            OldPlay {
                user: "u2",
                ..named("p17", "Alpha", Some("First"), None)
            },
            "t1",
            "NAMES",
        ),
        // Names are compared the way the catalog folds them.
        (
            named("p18", "  \u{c1}LPHA ", Some("first"), None),
            "t1",
            "NAMES",
        ),
        // A length an older version stored with a fraction.
        (
            named("p27", "Beta", Some("First"), Some(181_500)),
            "t2",
            "NAMES",
        ),
        // A connected server's play is not filed by names,
        (
            OldPlay {
                source: Some("plex"),
                ..named("p13", "Alpha", Some("First"), Some(200_000))
            },
            "",
            "REMOTE_WITHOUT_RECORDING",
        ),
        // but is by a recording the library holds once,
        (
            OldPlay {
                source: Some("youtube"),
                ..recorded("p14", &r1_upper)
            },
            "t1",
            "RECORDING",
        ),
        // unless that track's accepted recording says otherwise,
        (
            OldPlay {
                source: Some("plex"),
                ..recorded("p19", &r3)
            },
            "",
            "NO_MATCH",
        ),
        // or two tracks hold it, whatever album the row names.
        (
            OldPlay {
                album: Some("First"),
                source: Some("jellyfin"),
                ..recorded("p07", &r5)
            },
            "",
            "SEVERAL_MATCHES",
        ),
        (
            OldPlay {
                source: Some("jellyfin"),
                ..recorded("p23", &r12)
            },
            "",
            "SEVERAL_MATCHES",
        ),
    ];

    let dir = crate::common::ScratchDir::new("older-plays");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let open = || {
        let conn = rusqlite::Connection::open(&path).expect("database opens");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("foreign keys on");
        conn
    };
    let mut conn = open();
    conn.execute_batch(FILING_CATALOG).expect("catalog seeds");
    for (play, _, _) in &cases {
        store_old_play(&conn, play);
    }
    // A row that already names an album is not the repair's to touch.
    conn.execute(
        "INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
         artist_name, album_name, played_at) \
         VALUES ('p00', 'u1', 'al2', 'Alpha', 'Artist', 'First', '2026-01-01T00:00:00+00:00')",
        [],
    )
    .expect("the filed row is stored");
    conn.execute(
        "UPDATE library_play_history SET duration_ms = 181500.5 WHERE id = 'p27'",
        [],
    )
    .expect("the length is stored with a fraction");

    let tx = conn.transaction().expect("transaction");
    assert!(plays::queue(&tx, "job-1", None, 1.0).expect("queues"));
    assert!(
        !plays::queue(&tx, "job-2", None, 2.0).expect("queues"),
        "a second start with nothing new queues nothing"
    );
    tx.commit().expect("commit");

    let mut batches = 0;
    loop {
        // A stop between batches: nothing is kept but the database.
        drop(conn);
        conn = open();
        let tx = conn.transaction().expect("transaction");
        let done = plays::step(&tx, "job-1", 4, 3.0).expect("batch runs");
        tx.commit().expect("commit");
        batches += 1;
        let looked_at: i64 = conn
            .query_row(
                "SELECT completed_count FROM library_operation_jobs WHERE id = 'job-1'",
                [],
                |row| row.get(0),
            )
            .expect("job reads");
        assert_eq!(looked_at, (batches * 4).min(cases.len() as i64));
        if done {
            break;
        }
        // A restart while the job is half way queues no second job.
        let tx = conn.transaction().expect("transaction");
        assert!(
            !plays::queue(&tx, "job-3", None, 3.0).expect("queues"),
            "a job is already at work"
        );
        tx.commit().expect("commit");
    }
    assert_eq!(batches, cases.len() as i64 / 4 + 1);

    let mut wrong = Vec::new();
    for (play, track, _) in &cases {
        let want = match *track {
            "" => (String::new(), String::new(), String::new()),
            "t1" => ("t1".to_owned(), "al1".to_owned(), "a2".to_owned()),
            "t2" | "t10" | "t12" => ((*track).to_owned(), "al1".to_owned(), "a1".to_owned()),
            _ => ((*track).to_owned(), "al2".to_owned(), "a1".to_owned()),
        };
        let got = filed(&conn, play.id);
        if got != want {
            wrong.push(format!("{}: {got:?}, not {want:?}", play.id));
        }
        let listener: String = conn
            .query_row(
                "SELECT user_id FROM library_play_history WHERE id = ?1",
                [play.id],
                |row| row.get(0),
            )
            .expect("listener reads");
        assert_eq!(listener, play.user, "{}", play.id);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!(
        filed(&conn, "p00"),
        (String::new(), "al2".to_owned(), String::new())
    );

    let mut reasons = std::collections::BTreeMap::new();
    for (_, _, reason) in &cases {
        *reasons.entry((*reason).to_owned()).or_insert(0_i64) += 1;
    }
    let filed_rows = cases.iter().filter(|case| !case.1.is_empty()).count() as i64;
    let left_rows = cases.len() as i64 - filed_rows;
    let report = |conn: &rusqlite::Connection| -> (Value, (i64, i64, i64)) {
        let summary: String = conn
            .query_row(
                "SELECT result_json FROM library_repair_snapshots WHERE job_id = 'job-1'",
                [],
                |row| row.get(0),
            )
            .expect("summary reads");
        let counts = conn
            .query_row(
                "SELECT completed_count, succeeded_count, skipped_count \
                 FROM library_operation_jobs WHERE id = 'job-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("job reads");
        (
            serde_json::from_str(&summary).expect("summary parses"),
            counts,
        )
    };
    let (summary, counts) = report(&conn);
    assert_eq!(summary["counts_by_reason"], json!(reasons));
    assert_eq!(
        summary["counts_by_finding"],
        json!({"filed": filed_rows, "left": left_rows})
    );
    assert_eq!(summary["total_identities"], cases.len() as i64);
    assert_eq!(counts, (cases.len() as i64, filed_rows, left_rows));
    let changed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM library_play_history WHERE local_track_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("count reads");
    assert_eq!(changed, filed_rows, "the report counts the rows changed");

    // One more turn after the end looks at nothing.
    let tx = conn.transaction().expect("transaction");
    assert!(plays::step(&tx, "job-1", 4, 4.0).expect("batch runs"));
    tx.commit().expect("commit");
    assert_eq!(report(&conn), (summary, counts));
}
