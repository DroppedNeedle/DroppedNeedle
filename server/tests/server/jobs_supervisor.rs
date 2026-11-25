//! The loop supervisor's promises: a stop mid-sleep returns within the
//! grace, a failing tick backs off and recovers, a panic fails the entry
//! and the name starts clean again, and a second one-shot under a live name
//! is refused. Loops with nothing to do make no commit: their liveness
//! lives in the supervisor's memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, CancellationToken, FnTick, JobState, LoopRecord, Next, Schedule, Supervisor,
    TickError,
};
use droppedneedle_kernel::test_support::ManualClock;

use crate::common::{ScratchDir, loop_row, scratch_runtime, scratch_supervisor};

const GRACE: Duration = Duration::from_secs(5);

/// Give spawned tasks their turns until `name` is no longer live.
async fn until_gone(supervisor: &droppedneedle_kernel::jobs::Supervisor, name: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while supervisor.is_running(name) {
        assert!(std::time::Instant::now() < deadline, "{name} never ended");
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[tokio::test]
async fn a_stop_mid_sleep_returns_within_the_grace() {
    let (supervisor, runtime, _dir) = scratch_supervisor("loop-stop").await;
    supervisor
        .periodic(
            "sleeper",
            Schedule::new(Duration::from_secs(3600)),
            FnTick(|_| async { Ok(Next::Continue) }),
        )
        .unwrap();
    // The first tick has run; the loop now sleeps for an hour.
    loop_row(&supervisor, "sleeper", |row| {
        row.last_heartbeat_at.is_some()
    })
    .await;

    let started = std::time::Instant::now();
    supervisor.shutdown(GRACE).await;
    assert!(started.elapsed() < GRACE, "{:?}", started.elapsed());
    assert!(!supervisor.is_running("sleeper"));
    let row = loop_row(&supervisor, "sleeper", |row| row.state != JobState::Running).await;
    assert_eq!((row.state, row.last_error), (JobState::Stopped, None));
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_failing_tick_backs_off_then_recovers() {
    let (supervisor, runtime, _dir) = scratch_supervisor("loop-backoff").await;
    tokio::time::pause();
    let (turns, mut ticked) = tokio::sync::mpsc::unbounded_channel();
    let calls = AtomicU32::new(0);
    // Three failures, then success, on the blocking path.
    supervisor
        .periodic_blocking(
            "flaky",
            Schedule::new(Duration::from_secs(10))
                .with_backoff(Duration::from_secs(1), Duration::from_secs(60)),
            move |_| {
                let _ = turns.send(tokio::time::Instant::now());
                if calls.fetch_add(1, Ordering::SeqCst) < 3 {
                    Err(TickError::new("FLAKY", "not yet"))
                } else {
                    Ok(Next::Continue)
                }
            },
        )
        .unwrap();
    let mut at = Vec::new();
    for _ in 0..5 {
        at.push(ticked.recv().await.unwrap());
    }
    let gaps: Vec<u64> = at
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_secs())
        .collect();
    // Interval plus 2, 4 and 8 s of backoff, then the plain interval.
    assert_eq!(gaps, [12, 14, 18, 10]);
    let row = loop_row(&supervisor, "flaky", |row| row.last_error.is_none()).await;
    assert_eq!(row.state, JobState::Running);

    // A loop that only wakes on a signal backs off the same way, without
    // the interval: its idle ceiling is no part of a retry.
    let (wake, woken) = tokio::sync::watch::channel(0_u64);
    let mut woken = woken;
    woken.mark_changed();
    let (turns, mut ticked) = tokio::sync::mpsc::unbounded_channel();
    supervisor
        .on_wakeup(
            "flaky-wake",
            woken,
            Schedule::new(Duration::MAX)
                .with_backoff(Duration::from_secs(1), Duration::from_secs(60)),
            FnTick(move |_| {
                let _ = turns.send(tokio::time::Instant::now());
                async { Err(TickError::new("FLAKY", "down")) }
            }),
        )
        .unwrap();
    let mut at = Vec::new();
    for _ in 0..4 {
        at.push(ticked.recv().await.unwrap());
    }
    let gaps: Vec<u64> = at
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_secs())
        .collect();
    assert_eq!(gaps, [2, 4, 8]);
    drop(wake);
    tokio::time::resume();
    supervisor.shutdown(GRACE).await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_panicking_tick_fails_the_row_and_the_name_starts_clean() {
    let (supervisor, runtime, _dir) = scratch_supervisor("loop-panic").await;
    let schedule = Schedule::new(Duration::from_secs(3600));
    supervisor
        .periodic(
            "panicker",
            schedule,
            FnTick(|_| async { panic!("tick blew up") }),
        )
        .unwrap();
    until_gone(&supervisor, "panicker").await;
    let row = loop_row(&supervisor, "panicker", |row| row.state == JobState::Failed).await;
    assert_eq!(row.last_error.as_deref(), Some("PANICKED"));

    supervisor
        .periodic(
            "panicker",
            schedule,
            FnTick(|_| async { Ok(Next::Continue) }),
        )
        .unwrap();
    let row = loop_row(&supervisor, "panicker", |row| {
        row.state == JobState::Running && row.last_heartbeat_at.is_some()
    })
    .await;
    assert_eq!(row.last_error, None);
    supervisor.shutdown(GRACE).await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_second_one_shot_under_a_live_name_is_refused() {
    let (supervisor, runtime, _dir) = scratch_supervisor("loop-once").await;
    let hold = |ctx: droppedneedle_kernel::jobs::TickCtx| async move {
        ctx.token().cancelled().await;
        Ok(())
    };
    supervisor.one_shot("once", hold).unwrap();
    assert_eq!(supervisor.one_shot("once", hold), Err(AlreadyRunning));

    supervisor.shutdown(GRACE).await;
    assert!(!supervisor.is_running("once"));
    runtime.shutdown().await;
}

/// An idle server writes nothing on a timer. For an hour of virtual time a
/// 1 s periodic loop, a loop woken by the database's commits, a loop whose
/// tick fails, one that fails then recovers, a one-shot and a one-shot
/// replaced under its name run, then stop, and the commit counter never
/// moves. The supervisor's entries say how each loop stands: the periodic
/// one beat within the last second, the failing one has failed since its
/// first tick, the recovered one is clean again, and the replaced run never
/// wrote over its successor.
#[tokio::test]
async fn an_hour_of_loops_makes_no_commit() {
    let dir = ScratchDir::new("loop-quiet");
    let runtime = scratch_runtime(&dir.join("app.db")).await;
    const START: i64 = 1_700_000_000_000;
    let clock = ManualClock::at(START);
    let supervisor = Supervisor::new(Arc::new(clock.clone()), CancellationToken::new());
    tokio::time::pause();
    let counter = runtime.db().commits();
    let before = *counter.borrow();

    supervisor
        .periodic(
            "periodic",
            Schedule::new(Duration::from_secs(1)),
            FnTick(|_| async { Ok(Next::Continue) }),
        )
        .unwrap();
    let follower_turns = Arc::new(AtomicU32::new(0));
    let turns = Arc::clone(&follower_turns);
    let mut commits = runtime.db().commits();
    commits.mark_changed();
    supervisor
        .on_wakeup(
            "follower",
            commits,
            Schedule::new(Duration::from_secs(60)),
            FnTick(move |_| {
                turns.fetch_add(1, Ordering::SeqCst);
                async { Ok(Next::Continue) }
            }),
        )
        .unwrap();
    supervisor
        .periodic(
            "failing",
            Schedule::new(Duration::from_secs(10)),
            FnTick(|_| async { Err(TickError::new("QUIET_FAILED", "down")) }),
        )
        .unwrap();
    let recovering_turns = AtomicU32::new(0);
    supervisor
        .periodic(
            "recovering",
            Schedule::new(Duration::from_secs(10)),
            FnTick(move |_| {
                let turn = recovering_turns.fetch_add(1, Ordering::SeqCst);
                async move {
                    if turn < 2 {
                        Err(TickError::new("QUIET_FAILED", "not yet"))
                    } else {
                        Ok(Next::Continue)
                    }
                }
            }),
        )
        .unwrap();
    supervisor.one_shot("once", |_| async { Ok(()) }).unwrap();
    supervisor
        .one_shot("relaunched", |ctx| async move {
            ctx.token().cancelled().await;
            Err(TickError::new("RETIRED_RUN", "replaced"))
        })
        .unwrap();
    supervisor
        .replace_one_shot("relaunched", |ctx| async move {
            ctx.token().cancelled().await;
            Ok(())
        })
        .unwrap();

    for _ in 0..3_600 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        clock.advance(1_000);
    }
    tokio::time::sleep(Duration::from_millis(10)).await;

    let now = clock.now_ms();
    let entries = supervisor.loops();
    let names: Vec<&str> = entries.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "failing",
            "follower",
            "once",
            "periodic",
            "recovering",
            "relaunched"
        ]
    );
    let entry =
        |name: &str| -> &LoopRecord { entries.iter().find(|row| row.name == name).unwrap() };
    let periodic = entry("periodic");
    assert_eq!(
        (periodic.state, periodic.last_error.as_deref()),
        (JobState::Running, None)
    );
    let beat = periodic.last_heartbeat_at.expect("the periodic loop beat");
    assert!(
        (0..=1_000).contains(&(now - beat)),
        "beat {} ms ago",
        now - beat
    );
    let follower = entry("follower");
    assert_eq!(follower.state, JobState::Running);
    assert!(follower.last_heartbeat_at.is_some());
    assert!(
        follower_turns.load(Ordering::SeqCst) <= 61,
        "only its idle ceiling woke the follower"
    );
    let failing = entry("failing");
    assert_eq!(
        (failing.state, failing.last_error.as_deref()),
        (JobState::Running, Some("QUIET_FAILED"))
    );
    assert_eq!(
        failing.failing_since,
        Some(START),
        "failing since its first failure"
    );
    let recovering = entry("recovering");
    assert_eq!(
        (recovering.last_error.as_deref(), recovering.failing_since),
        (None, None),
        "a clean tick clears the failure"
    );
    let once = entry("once");
    assert_eq!(
        (once.state, once.last_error.as_deref()),
        (JobState::Stopped, None)
    );
    let relaunched = entry("relaunched");
    assert_eq!(
        (relaunched.state, relaunched.last_error.as_deref()),
        (JobState::Running, None),
        "the replaced run left its successor's entry alone"
    );

    tokio::time::resume();
    supervisor.shutdown(GRACE).await;
    assert_eq!(*counter.borrow(), before, "the loops made a commit");
    runtime.shutdown().await;
}
