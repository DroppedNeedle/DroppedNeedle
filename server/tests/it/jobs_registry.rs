//! Job-registry briefs: names, liveness rows, and cancellation.
//!
//! The registry is the mechanism every stage-10 loop stands on: duplicate
//! names rejected while live, durable rows tracking idle/running/stopped/
//! failed, heartbeats per cycle, cooperative cancel with a grace period, and
//! panics landing as failed rows instead of stuck entries. These briefs pin
//! that mechanism with trivial loops; the per-loop briefs live next to their
//! loops.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use droppedneedle::jobs::registry::{
    AlreadyRunning, JobCtx, JobExit, JobKind, JobRegistry, JobState, MemoryRegistryStore,
    RegistryStore,
};

type TestRegistry = JobRegistry<MemoryRegistryStore>;

fn registry() -> TestRegistry {
    JobRegistry::new(MemoryRegistryStore::new())
}

/// Wait until the name clears, or panic with the name after the budget.
async fn wait_until_clear(registry: &TestRegistry, name: &str) {
    for _ in 0..500 {
        if !registry.is_running(name) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("job {name} never cleared");
}

#[tokio::test]
async fn spawn_marks_running_then_stopped() {
    let registry = registry();
    registry
        .spawn("one-shot", JobKind::Ephemeral, None, |_ctx| async {
            JobExit::Stopped
        })
        .await
        .expect("first spawn wins");
    assert!(registry.is_running("one-shot"));
    let row = registry
        .store()
        .get_job("one-shot")
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Running);

    wait_until_clear(&registry, "one-shot").await;
    let row = registry
        .store()
        .get_job("one-shot")
        .await
        .expect("row survives");
    assert_eq!(row.state, JobState::Stopped);
    assert!(registry.running_names().is_empty());
}

#[tokio::test]
async fn duplicate_spawn_rejected_while_running() {
    let registry = registry();
    registry
        .spawn(
            "sleeper",
            JobKind::Ephemeral,
            None,
            |ctx: JobCtx<_>| async move {
                ctx.stop().notified().await;
                JobExit::Stopped
            },
        )
        .await
        .expect("first spawn wins");
    let duplicate = registry
        .spawn("sleeper", JobKind::Ephemeral, None, |_ctx| async {
            JobExit::Stopped
        })
        .await;
    assert!(matches!(duplicate, Err(AlreadyRunning)));

    registry.cancel("sleeper", Duration::from_secs(5)).await;
    assert!(!registry.is_running("sleeper"));

    // A cleared name spawns clean.
    registry
        .spawn("sleeper", JobKind::Ephemeral, None, |_ctx| async {
            JobExit::Stopped
        })
        .await
        .expect("respawn after clear wins");
    wait_until_clear(&registry, "sleeper").await;
}

#[tokio::test]
async fn cancel_fires_stop_and_marks_stopped() {
    let registry = registry();
    let saw_stop = Arc::new(AtomicBool::new(false));
    let probe = Arc::clone(&saw_stop);
    registry
        .spawn(
            "stoppable",
            JobKind::Ephemeral,
            None,
            |ctx: JobCtx<_>| async move {
                ctx.stop().notified().await;
                probe.store(true, Ordering::SeqCst);
                JobExit::Stopped
            },
        )
        .await
        .expect("spawn wins");

    registry.cancel("stoppable", Duration::from_secs(5)).await;
    assert!(saw_stop.load(Ordering::SeqCst));
    let row = registry
        .store()
        .get_job("stoppable")
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Stopped);
}

#[tokio::test]
async fn cancel_all_stops_everything() {
    let registry = registry();
    for name in ["first", "second", "third"] {
        registry
            .spawn(
                name,
                JobKind::Ephemeral,
                None,
                |ctx: JobCtx<_>| async move {
                    ctx.stop().notified().await;
                    JobExit::Stopped
                },
            )
            .await
            .expect("spawn wins");
    }
    assert_eq!(registry.running_names().len(), 3);
    registry.cancel_all(Duration::from_secs(5)).await;
    assert!(registry.running_names().is_empty());
    for name in ["first", "second", "third"] {
        let row = registry.store().get_job(name).await.expect("row exists");
        assert_eq!(row.state, JobState::Stopped, "{name}");
    }
}

#[tokio::test]
async fn failed_exit_marks_failed_row() {
    let registry = registry();
    registry
        .spawn("doomed", JobKind::Ephemeral, None, |_ctx| async {
            JobExit::Failed("boom".to_owned())
        })
        .await
        .expect("spawn wins");
    wait_until_clear(&registry, "doomed").await;
    let row = registry
        .store()
        .get_job("doomed")
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Failed);
}

#[tokio::test]
async fn panicking_task_marks_failed_row() {
    let registry = registry();
    registry
        .spawn("panicker", JobKind::Ephemeral, None, |_ctx| async {
            panic!("test panic, caught by the registry");
        })
        .await
        .expect("spawn wins");
    wait_until_clear(&registry, "panicker").await;
    let row = registry
        .store()
        .get_job("panicker")
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Failed);
}

#[tokio::test]
async fn unknown_cancel_is_a_silent_noop() {
    let registry = registry();
    registry
        .cancel("never-registered", Duration::from_secs(1))
        .await;
    registry.cancel_all(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn heartbeat_lands_on_the_row() {
    let registry = registry();
    registry
        .spawn(
            "beater",
            JobKind::Ephemeral,
            None,
            |ctx: JobCtx<_>| async move {
                ctx.heartbeat().await;
                ctx.stop().notified().await;
                JobExit::Stopped
            },
        )
        .await
        .expect("spawn wins");
    // Let the heartbeat land before cancelling.
    for _ in 0..500 {
        let row = registry
            .store()
            .get_job("beater")
            .await
            .expect("row exists");
        if row.last_heartbeat_at.is_some() {
            registry.cancel("beater", Duration::from_secs(5)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("heartbeat never landed");
}

#[test]
fn boot_jobs_lists_every_always_on_loop_once() {
    use droppedneedle::jobs::{
        BOOT_JOBS, checkpoint, events_watcher, personal_mix, playlist_sync, presence,
    };
    assert_eq!(
        BOOT_JOBS,
        &[
            checkpoint::JOB_NAME,
            presence::JOB_NAME,
            personal_mix::JOB_NAME,
            playlist_sync::JOB_NAME,
            events_watcher::JOB_NAME,
        ]
    );
    let mut sorted = BOOT_JOBS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), BOOT_JOBS.len());
}

#[tokio::test]
async fn list_jobs_returns_name_order() {
    let registry = registry();
    for name in ["charlie", "alpha", "bravo"] {
        registry
            .spawn(name, JobKind::Ephemeral, None, |_ctx| async {
                JobExit::Stopped
            })
            .await
            .expect("spawn wins");
    }
    registry.cancel_all(Duration::from_secs(5)).await;
    let names: Vec<String> = registry
        .list_jobs()
        .await
        .into_iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
}
