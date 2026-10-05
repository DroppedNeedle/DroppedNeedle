//! Background loops under virtual time: checkpoint cadence, presence sweeps
//! that isolate a failing source and never hot-loop, the personal mix boot
//! wait and daily run, and precache completion, watchdog and single run.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use droppedneedle::jobs::checkpoint::{self, FakeCheckpointRunner};
use droppedneedle::jobs::personal_mix::{self, PersonalMixer};
use droppedneedle::jobs::precache::{
    self, PrecacheLimits, PrecacheOutcome, PrecacheWork, Progress,
};
use droppedneedle::jobs::presence::{
    self, PresenceSession, PresenceSources, PresenceStore, SourceStatus,
};
use droppedneedle::jobs::registry::{
    BoxFuture, JobRegistry, JobState, MemoryRegistryStore, RegistryStore,
};
use droppedneedle::jobs::schedule::Schedule;

type TestRegistry = JobRegistry<MemoryRegistryStore>;

fn registry() -> TestRegistry {
    JobRegistry::new(MemoryRegistryStore::new())
}

/// Let freshly spawned loops run until they arm their first sleep. `advance`
/// jumps the clock before yielding, so a loop that first runs after an
/// advance would arm its sleep late and every timing assert would drift.
async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

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

// ---------------------------------------------------------------------------
// Checkpoint
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn checkpoint_passes_at_once_then_every_interval() {
    let registry = registry();
    let runner = FakeCheckpointRunner::new();
    checkpoint::spawn_on(
        &registry,
        runner.clone(),
        Schedule::new(Duration::from_secs(30)),
    )
    .await
    .expect("spawn wins");

    // First pass runs without any wait.
    settle().await;
    assert_eq!(runner.passes(), 1);

    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert_eq!(runner.passes(), 2);

    // Half an interval is not a pass.
    tokio::time::advance(Duration::from_secs(15)).await;
    settle().await;
    assert_eq!(runner.passes(), 2);

    registry
        .cancel(checkpoint::JOB_NAME, Duration::from_secs(5))
        .await;
    let row = registry
        .store()
        .get_job(checkpoint::JOB_NAME)
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Stopped);
    assert!(row.last_heartbeat_at.is_some());
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

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

#[tokio::test(start_paused = true)]
async fn presence_sweeps_and_reconciles_every_source() {
    let registry = registry();
    let feed = FakeFeed::default();
    let sources = FakeSources::all_on(vec![session("jellyfin:1")]);
    presence::spawn_on(
        &registry,
        feed.clone(),
        sources,
        Schedule::new(Duration::from_secs(4)),
    )
    .await
    .expect("spawn wins");

    settle().await;
    assert_eq!(feed.sweeps.load(Ordering::SeqCst), 1);
    {
        let slices = feed.slices.lock().unwrap();
        assert_eq!(slices.len(), 3);
        assert_eq!(slices["jellyfin"].len(), 1);
    }

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn presence_isolates_a_failing_source() {
    let registry = registry();
    let feed = FakeFeed::default();
    let sources = FakeSources::all_on(vec![session("navidrome:2")]);
    *sources.jellyfin.lock().unwrap() = vec![Err("jellyfin down".to_owned()); 10];
    presence::spawn_on(
        &registry,
        feed.clone(),
        sources,
        Schedule::new(Duration::from_secs(4)),
    )
    .await
    .expect("spawn wins");

    // Two cycles with Jellyfin down: the loop survives, the others reconcile.
    settle().await;
    tokio::time::advance(Duration::from_secs(20)).await;
    settle().await;
    assert!(feed.sweeps.load(Ordering::SeqCst) >= 2);
    {
        let slices = feed.slices.lock().unwrap();
        assert_eq!(slices["navidrome"].len(), 1);
        assert_eq!(slices["plex"].len(), 1);
        // The failed source keeps no fresh slice.
        assert!(slices.get("jellyfin").is_none());
    }

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn presence_never_hot_loops_on_total_failure() {
    let registry = registry();
    let feed = FakeFeed::default();
    let sources = FakeSources::all_on(Vec::new());
    for script in [&sources.jellyfin, &sources.navidrome, &sources.plex] {
        *script.lock().unwrap() = vec![Err("all down".to_owned()); 1000];
    }
    // A zero interval is the hostile case: the floor still paces the loop.
    presence::spawn_on(
        &registry,
        feed.clone(),
        sources,
        Schedule::new(Duration::ZERO),
    )
    .await
    .expect("spawn wins");
    settle().await;

    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    // The 50 ms floor caps a second at ~20 cycles, never thousands.
    assert!(feed.sweeps.load(Ordering::SeqCst) <= 25);

    registry.cancel_all(Duration::from_secs(5)).await;
}

// ---------------------------------------------------------------------------
// Personal mix
// ---------------------------------------------------------------------------

/// Scripted mixer: counts builds, fails while told to.
#[derive(Clone, Default)]
struct FakeMixer {
    builds: Arc<AtomicU64>,
    fail_until: Arc<AtomicU64>,
}

impl PersonalMixer for FakeMixer {
    fn run_for_all_users(&self) -> BoxFuture<'_, Result<(), String>> {
        let builds = Arc::clone(&self.builds);
        let fail_until = Arc::clone(&self.fail_until);
        Box::pin(async move {
            let build = builds.fetch_add(1, Ordering::SeqCst) + 1;
            if build <= fail_until.load(Ordering::SeqCst) {
                Err("provider down".to_owned())
            } else {
                Ok(())
            }
        })
    }
}

#[tokio::test(start_paused = true)]
async fn personal_mix_waits_for_boot_then_runs_daily() {
    let registry = registry();
    let mixer = FakeMixer::default();
    personal_mix::spawn_on(
        &registry,
        mixer.clone(),
        Schedule::new(personal_mix::REFRESH_INTERVAL)
            .with_initial_delay(personal_mix::INITIAL_DELAY),
    )
    .await
    .expect("spawn wins");
    settle().await;

    tokio::time::advance(Duration::from_secs(299)).await;
    settle().await;
    assert_eq!(mixer.builds.load(Ordering::SeqCst), 0);

    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(mixer.builds.load(Ordering::SeqCst), 1);

    // A day later, the second build.
    tokio::time::advance(personal_mix::REFRESH_INTERVAL).await;
    settle().await;
    assert_eq!(mixer.builds.load(Ordering::SeqCst), 2);

    registry.cancel_all(Duration::from_secs(5)).await;
}

// ---------------------------------------------------------------------------
// Precache
// ---------------------------------------------------------------------------

/// Scripted phases: beats on demand, then answers ok/err/hang.
#[derive(Clone)]
enum FakeBehavior {
    BeatThenOk { beats: u64 },
    Hang,
}

#[derive(Clone)]
struct FakeWork {
    behavior: FakeBehavior,
    started: Arc<AtomicBool>,
}

impl PrecacheWork for FakeWork {
    fn run(&self, progress: Progress) -> BoxFuture<'_, Result<(), String>> {
        let behavior = self.behavior.clone();
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            started.store(true, Ordering::SeqCst);
            match behavior {
                FakeBehavior::BeatThenOk { beats } => {
                    for beat in 0..beats {
                        progress.beat(Some(if beat == 0 { "artists" } else { "albums" }));
                        tokio::task::yield_now().await;
                    }
                    Ok(())
                }
                FakeBehavior::Hang => std::future::pending().await,
            }
        })
    }
}

fn test_limits() -> PrecacheLimits {
    PrecacheLimits::new(Duration::from_secs(60), Duration::from_secs(600))
        .with_watchdog_tick(Duration::from_secs(5))
}

#[tokio::test(start_paused = true)]
async fn precache_run_completes_and_unregisters() {
    let registry = registry();
    let work = FakeWork {
        behavior: FakeBehavior::BeatThenOk { beats: 3 },
        started: Arc::new(AtomicBool::new(false)),
    };
    let handle = precache::spawn_run(&registry, work, test_limits())
        .await
        .expect("run starts");
    assert!(registry.is_running(precache::JOB_NAME));

    // The work finishes without any clock movement; the watchdog never fires.
    settle().await;
    assert_eq!(handle.wait().await, PrecacheOutcome::Done);
    assert!(!registry.is_running(precache::JOB_NAME));
    let row = registry
        .store()
        .get_job(precache::JOB_NAME)
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Stopped);
}

#[tokio::test(start_paused = true)]
async fn precache_stall_trips_the_watchdog() {
    let registry = registry();
    let work = FakeWork {
        behavior: FakeBehavior::Hang,
        started: Arc::new(AtomicBool::new(false)),
    };
    let handle = precache::spawn_run(&registry, work, test_limits())
        .await
        .expect("run starts");
    settle().await;

    tokio::time::advance(Duration::from_secs(70)).await;
    let outcome = handle.wait().await;
    assert!(
        matches!(outcome, PrecacheOutcome::Watchdog(_)),
        "{outcome:?}"
    );
    assert!(!registry.is_running(precache::JOB_NAME));
}

#[tokio::test(start_paused = true)]
async fn precache_second_run_rejected_while_live() {
    let registry = registry();
    let make = || FakeWork {
        behavior: FakeBehavior::Hang,
        started: Arc::new(AtomicBool::new(false)),
    };
    precache::spawn_run(&registry, make(), test_limits())
        .await
        .expect("first run starts");
    let duplicate = precache::spawn_run(&registry, make(), test_limits()).await;
    assert!(duplicate.is_err());
    registry.cancel_all(Duration::from_secs(5)).await;
}
