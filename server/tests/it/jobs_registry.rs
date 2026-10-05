//! The job registry: one live run per name, cancel-all on shutdown, a
//! panicking task lands as a failed row, and every always-on loop is
//! registered at boot exactly once.

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
