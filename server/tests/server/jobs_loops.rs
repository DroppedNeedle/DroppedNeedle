//! Kernel loops on the supervisor: presence isolates a failing source, and
//! the precache watchdog fails a stalled run.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use droppedneedle_kernel::db::JobState;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::precache::{
    self, PrecacheLimits, PrecacheOutcome, PrecacheWork, Progress,
};
use droppedneedle_kernel::jobs::presence::{
    self, PresenceSession, PresenceSources, PresenceStore, SourceStatus,
};
use droppedneedle_kernel::jobs::schedule::Schedule;

use crate::common::{loop_row, scratch_supervisor};

const GRACE: Duration = Duration::from_secs(5);

fn session(key: &str) -> PresenceSession {
    PresenceSession {
        key: key.to_owned(),
        user_name: "listener".to_owned(),
        device_name: "speaker".to_owned(),
        track_name: "track".to_owned(),
        artist_name: "artist".to_owned(),
        album_name: None,
        cover_url: String::new(),
        is_paused: false,
        progress_ms: 0,
        duration_ms: 1000,
    }
}

/// Scripted feed: counts sweeps and remembers each source's last slice.
#[derive(Clone, Default)]
struct FakeFeed {
    sweeps: Arc<AtomicU64>,
    slices: Arc<Mutex<HashMap<String, Vec<PresenceSession>>>>,
}

impl PresenceStore for FakeFeed {
    fn sweep(&self) -> BoxFuture<'_, ()> {
        let sweeps = Arc::clone(&self.sweeps);
        Box::pin(async move {
            sweeps.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn reconcile(&self, source: &str, sessions: Vec<PresenceSession>) -> BoxFuture<'_, ()> {
        let slices = Arc::clone(&self.slices);
        let source = source.to_owned();
        Box::pin(async move {
            slices.lock().unwrap().insert(source, sessions);
        })
    }
}

/// One source's scripted answers: ok slices or failure causes in order.
type PollScript = Arc<Mutex<Vec<Result<Vec<PresenceSession>, String>>>>;

/// Scripted upstreams with per-source scripts of ok/err answers.
#[derive(Clone)]
struct FakeSources {
    status: SourceStatus,
    jellyfin: PollScript,
    navidrome: PollScript,
    plex: PollScript,
}

impl FakeSources {
    fn all_on(sessions: Vec<PresenceSession>) -> Self {
        Self {
            status: SourceStatus {
                jellyfin: true,
                navidrome: true,
                plex: true,
            },
            jellyfin: Arc::new(Mutex::new(vec![Ok(sessions.clone())])),
            navidrome: Arc::new(Mutex::new(vec![Ok(sessions.clone())])),
            plex: Arc::new(Mutex::new(vec![Ok(sessions)])),
        }
    }

    fn pop(
        script: &Mutex<Vec<Result<Vec<PresenceSession>, String>>>,
    ) -> Result<Vec<PresenceSession>, String> {
        let mut guard = script.lock().unwrap();
        if guard.len() > 1 {
            guard.remove(0)
        } else {
            guard.first().cloned().unwrap_or(Ok(Vec::new()))
        }
    }
}

impl PresenceSources for FakeSources {
    fn status(&self) -> BoxFuture<'_, SourceStatus> {
        let status = self.status;
        Box::pin(async move { status })
    }

    fn poll_jellyfin(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>> {
        let script = Arc::clone(&self.jellyfin);
        Box::pin(async move { Self::pop(&script) })
    }

    fn poll_navidrome(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>> {
        let script = Arc::clone(&self.navidrome);
        Box::pin(async move { Self::pop(&script) })
    }

    fn poll_plex(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>> {
        let script = Arc::clone(&self.plex);
        Box::pin(async move { Self::pop(&script) })
    }
}

#[tokio::test]
async fn presence_isolates_a_failing_source() {
    let (supervisor, runtime, _dir) = scratch_supervisor("presence-isolates").await;
    let feed = FakeFeed::default();
    let sources = FakeSources::all_on(vec![session("navidrome:2")]);
    *sources.jellyfin.lock().unwrap() = vec![Err("jellyfin down".to_owned()); 10];
    tokio::time::pause();
    presence::spawn_on(
        &supervisor,
        feed.clone(),
        sources,
        Schedule::new(Duration::from_secs(4)),
    )
    .expect("spawn wins");

    // Two cycles with Jellyfin down: the loop survives, the others reconcile.
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert!(feed.sweeps.load(Ordering::SeqCst) >= 2);
    {
        let slices = feed.slices.lock().unwrap();
        assert_eq!(slices["navidrome"].len(), 1);
        assert_eq!(slices["plex"].len(), 1);
        // The failed source keeps no fresh slice.
        assert!(slices.get("jellyfin").is_none());
    }
    let row = loop_row(runtime.db(), presence::JOB_NAME, |row| {
        row.last_error.is_some()
    })
    .await;
    assert_eq!(row.state, JobState::Running);
    assert_eq!(row.last_error.as_deref(), Some(presence::FAILED));

    tokio::time::resume();
    supervisor.shutdown(GRACE).await;
    runtime.shutdown().await;
}

/// Phases that never progress.
#[derive(Clone)]
struct HangingWork;

impl PrecacheWork for HangingWork {
    fn run(&self, _progress: Progress) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn precache_stall_trips_the_watchdog() {
    let (supervisor, runtime, _dir) = scratch_supervisor("precache-stall").await;
    tokio::time::pause();
    let limits = PrecacheLimits::new(Duration::from_secs(60), Duration::from_secs(600))
        .with_watchdog_tick(Duration::from_secs(5));
    let handle = precache::spawn_run(&supervisor, HangingWork, limits, |_| {}).expect("run starts");

    let outcome = handle.wait().await;
    assert!(
        matches!(outcome, PrecacheOutcome::Watchdog(_)),
        "{outcome:?}"
    );
    let row = loop_row(runtime.db(), precache::JOB_NAME, |row| {
        row.state == JobState::Failed
    })
    .await;
    assert_eq!(row.last_error.as_deref(), Some(precache::WATCHDOG));
    assert!(!supervisor.is_running(precache::JOB_NAME));

    tokio::time::resume();
    supervisor.shutdown(GRACE).await;
    runtime.shutdown().await;
}
