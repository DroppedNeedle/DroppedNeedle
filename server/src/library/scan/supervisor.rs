//! Stable scan supervisor: recovery, dirty scopes, schedule, worker.
//!
//! Port of `backend/services/native/library_scan_supervisor.py`. One loop
//! owns every trigger: S-01 Hook A (one-shot startup reconciliation),
//! Hook B (dirty scope marks from settings saves), the rolling schedule
//! tick, and the single scan worker. Iterations never fail the loop: a
//! failed iteration logs and retries after 1s; an idle loop sleeps on the
//! work-wakeup revision with a 47s recovery ceiling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::coordinator::LibraryScanCoordinator;
use super::models::{Disposition, ScanKind, ScanRequest, ScanScope, ScanTrigger};
use super::roots::RootRegistry;
use super::scheduler::{
    InclusionRule, ScheduleSettings, scheduled_scopes, seconds_until_due, tick,
};
use super::seams::{IdentifyQueue, TagReader};
use super::store::ScanStore;
use super::watcher::{DirtyScopes, WorkWakeups};

/// Idle recovery ceiling (v2 `EMPTY_RECOVERY_INTERVAL_SECONDS`).
pub const EMPTY_RECOVERY_INTERVAL_SECS: f64 = 47.0;
/// Failed-iteration retry (v2 `ERROR_RETRY_INTERVAL_SECONDS`).
pub const ERROR_RETRY_INTERVAL_SECS: f64 = 1.0;

/// Supervisor loop inputs. Getters are re-read every iteration.
pub struct SupervisorInputs {
    pub root_paths: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>,
    pub schedule: Arc<dyn Fn() -> ScheduleSettings + Send + Sync>,
    pub inclusion_rules: Arc<dyn Fn() -> Vec<InclusionRule> + Send + Sync>,
    pub dirty: DirtyScopes,
    pub wakeups: WorkWakeups,
    pub now_unix: Arc<dyn Fn() -> f64 + Send + Sync>,
}

/// Resolve Hook B dirty scope ids against current settings (v2
/// `_scopes_for_dirty_ids`). Ids that no longer resolve are dropped:
/// dirty marks are hints only, and removals converge through
/// policy reconciliation instead.
pub fn scopes_for_dirty_ids(
    registry: &RootRegistry,
    rules: &[InclusionRule],
    scope_ids: &[String],
) -> Vec<ScanScope> {
    let wanted: std::collections::HashSet<&str> = scope_ids.iter().map(String::as_str).collect();
    let mut scopes = Vec::new();
    for root in registry.roots() {
        if wanted.contains(root.id.as_str()) {
            let mut scope = ScanScope::root(
                &root.id,
                &root.path.to_string_lossy(),
                registry.policy_revision(),
            );
            scope.effective_policy = root.policy;
            scopes.push(scope);
        }
    }
    for rule in rules {
        if !wanted.contains(rule.rule_id.as_str()) {
            continue;
        }
        let Some(root) = registry.resolve(&rule.root_id) else {
            continue;
        };
        scopes.push(ScanScope {
            root_id: rule.root_id.clone(),
            scope_id: Some(rule.rule_id.clone()),
            relative_path: rule.relative_path.clone(),
            root_path: Some(root.path.to_string_lossy().into_owned()),
            effective_policy: root.policy,
            policy_revision: registry.policy_revision().to_owned(),
            estimated_count: None,
        });
    }
    scopes
}

/// Startup recovery plus the Hook A one-shot reconciliation scan (v2
/// supervisor preamble, D5 every boot). Guards mirror the loop gates:
/// enabled, not manual, nothing resumable or current, non-empty scopes.
/// Every request disposition is acceptable; failures log and the loop
/// below still runs.
pub async fn startup_recovery<S, T, Q>(
    coordinator: &LibraryScanCoordinator<S, T, Q>,
    inputs: &SupervisorInputs,
) where
    S: ScanStore,
    T: TagReader + 'static,
    Q: IdentifyQueue,
{
    let registry = coordinator.registry();
    let enabled = registry.enabled();
    let recovered = if enabled {
        coordinator.recover()
    } else {
        coordinator.recover_stopping()
    };
    let schedule = (inputs.schedule)();
    if enabled
        && schedule.frequency != "manual"
        && recovered.is_empty()
        && coordinator.current().is_empty()
    {
        let scopes = scheduled_scopes(&registry, &(inputs.inclusion_rules)());
        if !scopes.is_empty() {
            let result = coordinator.request_run(&ScanRequest {
                kind: ScanKind::Incremental,
                trigger: ScanTrigger::StartupResume,
                scopes,
                requested_by_user_id: None,
                policy_revision: registry.policy_revision().to_owned(),
            });
            if let Err(error) = result {
                tracing::warn!(error = %error, "target scan startup reconciliation request failed");
            }
        }
    }
}

/// One supervisor iteration. Returns true when a run was driven (the
/// caller loops immediately instead of sleeping).
pub async fn supervise_once<S, T, Q>(
    coordinator: &LibraryScanCoordinator<S, T, Q>,
    inputs: &SupervisorInputs,
) -> bool
where
    S: ScanStore,
    T: TagReader + 'static,
    Q: IdentifyQueue,
{
    let registry = coordinator.registry();
    let enabled = registry.enabled();
    // Hook B consumer: dirty scope marks fire regardless of frequency
    // (after the enabled check only). Marks clear only on a non-conflict
    // request; unresolvable ids clear immediately as hints.
    if enabled {
        let dirty_ids = inputs.dirty.list();
        if !dirty_ids.is_empty() {
            let rules = (inputs.inclusion_rules)();
            let dirty_scopes = scopes_for_dirty_ids(&registry, &rules, &dirty_ids);
            if dirty_scopes.is_empty() {
                inputs.dirty.clear(&dirty_ids);
            } else {
                match coordinator.request_run(&ScanRequest {
                    kind: ScanKind::Incremental,
                    trigger: ScanTrigger::PolicyApply,
                    requested_by_user_id: None,
                    policy_revision: registry.policy_revision().to_owned(),
                    scopes: dirty_scopes,
                }) {
                    Ok(result) if result.disposition != Disposition::Conflict => {
                        inputs.dirty.clear(&dirty_ids);
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "dirty-scope scan request failed");
                    }
                    _ => {}
                }
            }
        }
    }
    if enabled {
        let schedule = (inputs.schedule)();
        let terminal_at = coordinator
            .latest_filesystem_terminal()
            .and_then(|run| run.terminal_at);
        let now = (inputs.now_unix)();
        let rules = (inputs.inclusion_rules)();
        let tick_scheduled = tick(
            |request| {
                coordinator
                    .request_run(&request)
                    .map(|result| result.disposition)
                    .map_err(|error| error.to_string())
            },
            &registry,
            &rules,
            &schedule,
            terminal_at,
            now,
        );
        if !tick_scheduled {
            tracing::debug!("target scan scheduler tick did not start a run");
        }
    }
    if enabled {
        return coordinator.run_once(&(inputs.root_paths)()).await.is_some();
    }
    false
}

/// Run the supervisor loop until `shutdown` is set.
///
/// Standalone loop form over [`supervise_once`] plus [`startup_recovery`].
/// The wired bundle drives those two directly from its own
/// shutdown-watch loop (`LibrarySetup::scan_startup_recovery` /
/// `supervisor_tick`), so no production caller reaches this; the
/// scan briefs pin its Hook-A-then-exit behavior for embedders.
pub async fn supervise_target_scans<S, T, Q>(
    coordinator: &LibraryScanCoordinator<S, T, Q>,
    inputs: &SupervisorInputs,
    shutdown: &AtomicBool,
) where
    S: ScanStore,
    T: TagReader + 'static,
    Q: IdentifyQueue,
{
    startup_recovery(coordinator, inputs).await;
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let revision = inputs.wakeups.revision("scan");
        // Hook A inputs are captured during recovery above; the loop below
        // re-reads every getter each iteration.
        let processed = supervise_once(coordinator, inputs).await;
        if processed {
            continue;
        }
        // Idle: sleep on the wakeup revision with the recovery ceiling.
        // A failed wait must not kill the supervisor either.
        let _ = inputs
            .wakeups
            .wait(
                "scan",
                revision,
                Duration::from_secs_f64(EMPTY_RECOVERY_INTERVAL_SECS),
            )
            .await;
    }
}

/// Seconds until the next automatic scan, for operator visibility.
/// Thin wrapper over the schedule math (None when manual or unresolvable).
pub fn seconds_until_next_scan(
    settings: &ScheduleSettings,
    terminal_at: Option<f64>,
    now: f64,
) -> Option<f64> {
    seconds_until_due(
        &settings.frequency,
        &settings.daily_time,
        terminal_at,
        now,
        &settings.timezone_name,
    )
}

#[cfg(test)]
mod tests {
    use super::super::coordinator::{LibraryScanCoordinator, StaticResolver};
    use super::super::pool::BlockingPool;
    use super::super::roots::LibraryRoot;
    use super::super::seams::{NullIdentifyQueue, NullTagReader};
    use super::super::store::MemoryScanStore;
    use super::*;
    use std::sync::atomic::Ordering;

    fn coordinator() -> LibraryScanCoordinator<MemoryScanStore, NullTagReader, NullIdentifyQueue> {
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "r1",
                PathBuf::from("/music"),
                super::super::models::EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        LibraryScanCoordinator::new(
            Arc::new(MemoryScanStore::new()),
            BlockingPool::new(2),
            Arc::new(NullTagReader::new()),
            Arc::new(NullIdentifyQueue::new()),
            Arc::new(StaticResolver::new(registry)),
        )
    }

    fn inputs(dirty: DirtyScopes) -> SupervisorInputs {
        SupervisorInputs {
            root_paths: Arc::new(HashMap::new),
            schedule: Arc::new(ScheduleSettings::manual),
            inclusion_rules: Arc::new(Vec::new),
            dirty,
            wakeups: WorkWakeups::new(),
            now_unix: Arc::new(|| 1_000.0),
        }
    }

    #[tokio::test]
    async fn dirty_marks_request_a_policy_apply_scan() {
        let coordinator = coordinator().with_wakeups(WorkWakeups::new());
        let dirty = DirtyScopes::new();
        dirty.mark("r1");
        let inputs = inputs(dirty.clone());
        // Manual schedule: the tick stays quiet, but Hook B still fires.
        let processed = supervise_once(&coordinator, &inputs).await;
        assert!(processed, "dirty mark should request and drive a run");
        assert!(dirty.is_empty(), "marks clear on non-conflict request");
        let history = coordinator.history(10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].trigger, ScanTrigger::PolicyApply);
    }

    #[tokio::test]
    async fn unresolvable_dirty_marks_clear_as_hints() {
        let coordinator = coordinator();
        let dirty = DirtyScopes::new();
        dirty.mark("removed-root");
        let inputs = inputs(dirty.clone());
        supervise_once(&coordinator, &inputs).await;
        assert!(dirty.is_empty(), "unresolvable marks clear without a scan");
        assert!(coordinator.current().is_empty());
    }

    #[tokio::test]
    async fn startup_recovery_requests_a_resume_scan() {
        let coordinator = coordinator();
        let inputs = SupervisorInputs {
            schedule: Arc::new(|| ScheduleSettings::new("1hr", "03:00", "UTC")),
            ..inputs(DirtyScopes::new())
        };
        startup_recovery(&coordinator, &inputs).await;
        let current = coordinator.current();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].trigger, ScanTrigger::StartupResume);
        let _ = Ordering::Relaxed;
    }
}
