//! Stable scan supervisor: recovery, dirty scopes, schedule, worker.
//!
//! Port of v2's library scan supervisor. One loop owns every trigger:
//! Hook A (one-shot startup reconciliation),
//! Hook B (dirty scope marks from settings saves), the rolling schedule
//! tick, and the single scan worker. Iterations never fail the loop: a
//! failed iteration logs and retries after 1s; an idle loop sleeps on the
//! work-wakeup revision with a 47s recovery ceiling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_kernel::runtime_config::sections::ScanFrequency;

use super::coordinator::LibraryScanCoordinator;
use super::models::{Disposition, ScanKind, ScanRequest, ScanScope, ScanTrigger};
use super::roots::RootRegistry;
use super::scheduler::{InclusionRule, ScheduleSettings, scheduled_scopes, tick};
use super::seams::TagReader;
use super::watcher::{DirtyScopes, WorkWakeups};

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

/// Hook B consumer: ask for a scan of the scopes marked dirty (new or
/// changed roots and rules), whatever the schedule, while the library is
/// on. Marks clear only on a non-conflict request; ids that no longer
/// resolve clear at once as hints. Callers take turns, so two never ask
/// for the same marks. Blocking.
pub fn request_dirty_scopes<T>(coordinator: &LibraryScanCoordinator<T>, inputs: &SupervisorInputs)
where
    T: TagReader + 'static,
{
    let _turn = coordinator.dirty_turn();
    let registry = coordinator.registry();
    if !registry.enabled() {
        return;
    }
    let dirty_ids = inputs.dirty.list();
    if dirty_ids.is_empty() {
        return;
    }
    let rules = (inputs.inclusion_rules)();
    let dirty_scopes = scopes_for_dirty_ids(&registry, &rules, &dirty_ids);
    if dirty_scopes.is_empty() {
        inputs.dirty.clear(&dirty_ids);
        return;
    }
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

/// Startup recovery plus the Hook A one-shot reconciliation scan (v2
/// supervisor preamble, every boot). Guards mirror the loop gates:
/// enabled, not manual, nothing resumable or current, non-empty scopes.
/// Every request disposition is acceptable; failures log and the loop
/// below still runs.
pub async fn startup_recovery<T>(coordinator: &LibraryScanCoordinator<T>, inputs: &SupervisorInputs)
where
    T: TagReader + 'static,
{
    let registry = coordinator.registry();
    let enabled = registry.enabled();
    let recovered = if enabled {
        coordinator.recover()
    } else {
        coordinator.recover_stopping()
    };
    // Unread state requests nothing: the loop below drives what is there.
    let idle = match (recovered, coordinator.current_runs()) {
        (Ok(recovered), Ok(current)) => recovered.is_empty() && current.is_empty(),
        (recovered, current) => {
            for error in [recovered.err(), current.err()].into_iter().flatten() {
                tracing::error!(%error, "scan startup recovery could not read the runs");
            }
            false
        }
    };
    let schedule = (inputs.schedule)();
    if enabled && schedule.frequency != ScanFrequency::Manual && idle {
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
pub async fn supervise_once<T>(
    coordinator: &LibraryScanCoordinator<T>,
    inputs: &SupervisorInputs,
) -> bool
where
    T: TagReader + 'static,
{
    supervise_once_inner(coordinator, inputs, None).await
}

/// One shutdown-aware supervisor iteration: Hook B, schedule, and worker
/// exactly like [`supervise_once`], but a cancelled stop ends the
/// in-flight run instead of waiting it out. A stop already cancelled
/// claims no new work. Returns true when a run was driven.
pub async fn supervise_once_with_shutdown<T>(
    coordinator: &LibraryScanCoordinator<T>,
    inputs: &SupervisorInputs,
    stop: &CancellationToken,
) -> bool
where
    T: TagReader + 'static,
{
    supervise_once_inner(coordinator, inputs, Some(stop)).await
}

async fn supervise_once_inner<T>(
    coordinator: &LibraryScanCoordinator<T>,
    inputs: &SupervisorInputs,
    stop: Option<&CancellationToken>,
) -> bool
where
    T: TagReader + 'static,
{
    let registry = coordinator.registry();
    let enabled = registry.enabled();
    request_dirty_scopes(coordinator, inputs);
    // A last scan that cannot be read schedules nothing this pass.
    let latest = if enabled {
        coordinator.latest_filesystem_terminal().map_err(|error| {
            tracing::error!(%error, "scan schedule could not read the last scan");
        })
    } else {
        Err(())
    };
    if let Ok(latest) = latest {
        let schedule = (inputs.schedule)();
        let terminal_at = latest.and_then(|run| run.terminal_at);
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
        return match stop {
            Some(stop) => coordinator
                .run_once_with_shutdown(&(inputs.root_paths)(), stop)
                .await
                .is_some(),
            None => coordinator.run_once(&(inputs.root_paths)()).await.is_some(),
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::coordinator::{LibraryScanCoordinator, StaticResolver};
    use super::super::pool::BlockingPool;
    use super::super::roots::LibraryRoot;
    use super::super::seams::NullTagReader;
    use super::super::sqlite_store::scratch_store;
    use super::*;
    use std::sync::atomic::Ordering;

    fn coordinator() -> (
        droppedneedle_kernel::db::scratch::ScratchDir,
        LibraryScanCoordinator<NullTagReader>,
    ) {
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "r1",
                PathBuf::from("/music"),
                super::super::models::EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        let (dir, store) = scratch_store();
        let coordinator = LibraryScanCoordinator::new(
            Arc::new(store),
            BlockingPool::new(2),
            Arc::new(NullTagReader::new()),
            Arc::new(StaticResolver::new(registry)),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        );
        (dir, coordinator)
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
        let (_dir, coordinator) = coordinator();
        let coordinator = coordinator.with_wakeups(WorkWakeups::new());
        let dirty = DirtyScopes::new();
        dirty.mark("r1");
        let inputs = inputs(dirty.clone());
        // Manual schedule: the tick stays quiet, but Hook B still fires.
        let processed = supervise_once(&coordinator, &inputs).await;
        assert!(processed, "dirty mark should request and drive a run");
        assert!(dirty.is_empty(), "marks clear on non-conflict request");
        let history = coordinator.history(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].trigger, ScanTrigger::PolicyApply);
    }

    #[tokio::test]
    async fn pre_signalled_shutdown_claims_no_new_work() {
        let (_dir, coordinator) = coordinator();
        let coordinator = coordinator.with_wakeups(WorkWakeups::new());
        let dirty = DirtyScopes::new();
        dirty.mark("r1");
        let inputs = inputs(dirty.clone());
        let stop = CancellationToken::new();
        stop.cancel();
        let processed = supervise_once_with_shutdown(&coordinator, &inputs, &stop).await;
        assert!(!processed, "pre-signalled shutdown drives no run");
        let current = coordinator.current_runs().unwrap();
        assert!(
            current
                .iter()
                .all(|run| run.state == super::super::models::RunState::Queued),
            "no run leaves queued under shutdown"
        );
    }

    #[tokio::test]
    async fn unresolvable_dirty_marks_clear_as_hints() {
        let (_dir, coordinator) = coordinator();
        let dirty = DirtyScopes::new();
        dirty.mark("removed-root");
        let inputs = inputs(dirty.clone());
        supervise_once(&coordinator, &inputs).await;
        assert!(dirty.is_empty(), "unresolvable marks clear without a scan");
        assert!(coordinator.current_runs().unwrap().is_empty());
    }

    #[tokio::test]
    async fn startup_recovery_requests_a_resume_scan() {
        let (_dir, coordinator) = coordinator();
        let inputs = SupervisorInputs {
            schedule: Arc::new(|| {
                ScheduleSettings::new(ScanFrequency::Hr1, "03:00", jiff::tz::TimeZone::UTC)
            }),
            ..inputs(DirtyScopes::new())
        };
        startup_recovery(&coordinator, &inputs).await;
        let current = coordinator.current_runs().unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].trigger, ScanTrigger::StartupResume);
        let _ = Ordering::Relaxed;
    }
}
