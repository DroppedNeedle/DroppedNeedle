//! The loop supervisor's promises, on a real database: a stop mid-sleep
//! returns within the grace, a failing tick backs off and recovers, a panic
//! fails the row and the name starts clean again, and a second one-shot
//! under a live name is refused. A loop that follows the commit counter
//! does not wake itself with its own rows.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use droppedneedle_kernel::db::JobState;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, TickError};

use crate::common::{loop_row, scratch_supervisor};

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
    loop_row(runtime.db(), "sleeper", |row| {
        row.last_heartbeat_at.is_some()
    })
    .await;

    let started = std::time::Instant::now();
    supervisor.shutdown(GRACE).await;
    assert!(started.elapsed() < GRACE, "{:?}", started.elapsed());
    assert!(!supervisor.is_running("sleeper"));
    let row = loop_row(runtime.db(), "sleeper", |row| {
        row.state != JobState::Running
    })
    .await;
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
    let row = loop_row(runtime.db(), "flaky", |row| row.last_error.is_none()).await;
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
    let row = loop_row(runtime.db(), "panicker", |row| {
        row.state == JobState::Failed
    })
    .await;
    assert_eq!(row.last_error.as_deref(), Some("PANICKED"));

    supervisor
        .periodic(
            "panicker",
            schedule,
            FnTick(|_| async { Ok(Next::Continue) }),
        )
        .unwrap();
    let row = loop_row(runtime.db(), "panicker", |row| {
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

/// A loop woken by the database's commits never wakes itself: a heartbeat
/// alone is written at most once a minute, so on an idle database the
/// commit counter stays put across its turns.
#[tokio::test]
async fn a_commit_follower_does_not_wake_itself() {
    let (supervisor, runtime, _dir) = scratch_supervisor("loop-quiet").await;
    let (turns, mut ticked) = tokio::sync::mpsc::unbounded_channel();
    let mut commits = runtime.db().commits();
    commits.mark_changed();
    supervisor
        .on_wakeup(
            "follower",
            commits,
            Schedule::new(Duration::from_millis(100)),
            FnTick(move |_| {
                let turns = turns.clone();
                async move {
                    let _ = turns.send(());
                    Ok(Next::Continue)
                }
            }),
        )
        .unwrap();
    // The start and the first heartbeat are written, then the loop settles.
    loop_row(runtime.db(), "follower", |row| {
        row.last_heartbeat_at.is_some()
    })
    .await;
    for _ in 0..3 {
        ticked.recv().await.unwrap();
    }
    let counter = runtime.db().commits();
    let before = *counter.borrow();
    for _ in 0..10 {
        ticked.recv().await.unwrap();
    }
    assert_eq!(
        *counter.borrow(),
        before,
        "the loop's own rows moved the commit counter"
    );
    supervisor.shutdown(GRACE).await;
    runtime.shutdown().await;
}
