//! Library background loops and the ticks they drive.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::clock::{now_ms, now_unix, today_day};
use super::identify::stores::QueueStore;
use super::publish::PublishError;
use super::publish::snapshots::SnapshotStore;
use super::scan::coordinator::ResolverSource as _;
use super::scan::supervisor::SupervisorInputs;
use super::scan::supervisor::{startup_recovery, supervise_once, supervise_once_with_shutdown};
use super::scan::watcher::{
    WatcherAction, WatcherSettings, WatcherState, clear_pending, watcher_request,
};
use super::wiring::LibrarySetup;

/// Scan supervisor idle ceiling. The supervisor's 47s recovery ceiling
/// cannot wait on shutdown, so the wired loop re-checks the watch
/// every 5s; operator-visible behavior is identical.
const SUPERVISOR_IDLE_CEILING: Duration = Duration::from_secs(5);

/// Identify queue poll cadence.
const IDENTIFY_POLL: Duration = Duration::from_secs(2);

/// Publish maintenance cadence (snapshot purge plus preview sweep).
const PUBLISH_MAINTENANCE: Duration = Duration::from_secs(3600);

impl LibrarySetup {
    /// Supervisor inputs, rebuilt per call so settings-swap rebuilds
    /// never strand the loop on stale getters.
    pub(crate) fn supervisor_inputs(&self) -> SupervisorInputs {
        SupervisorInputs {
            root_paths: self.root_dirs.clone(),
            schedule: {
                let config = self.config.clone();
                Arc::new(move || super::settings::schedule(&config))
            },
            inclusion_rules: {
                let registry = self.registry.clone();
                Arc::new(move || super::settings::inclusion_rules(registry.resolver().registry()))
            },
            dirty: self.dirty.clone(),
            wakeups: self.wakeups.clone(),
            now_unix: Arc::new(now_unix),
        }
    }

    /// Watcher settings, re-read from the config store every tick.
    pub(crate) fn watcher_settings(&self) -> WatcherSettings {
        super::settings::watcher(&self.config)
    }

    /// One-shot scan startup reconciliation (Hook A). The loop runs
    /// this in its preamble; tests drive it directly.
    pub async fn scan_startup_recovery(&self) {
        self.refresh_registry();
        startup_recovery(&self.coordinator, &self.supervisor_inputs()).await;
    }

    /// One supervisor iteration. Returns true when a run was driven.
    pub async fn supervisor_tick(&self) -> bool {
        self.refresh_registry();
        supervise_once(&self.coordinator, &self.supervisor_inputs()).await
    }

    /// One shutdown-aware supervisor iteration: same Hook B, schedule,
    /// and worker semantics as [`supervisor_tick`](Self::supervisor_tick),
    /// but a signalled shutdown stops the in-flight scan instead of
    /// waiting it out. The stop goes through the regular control latch,
    /// so the walk and index checkpoints settle the run to cancelled
    /// on their next check and the next start resumes cleanly. A
    /// pre-signalled shutdown claims no new work.
    pub async fn supervisor_tick_with_shutdown(&self, shutdown: &watch::Receiver<bool>) -> bool {
        self.refresh_registry();
        supervise_once_with_shutdown(&self.coordinator, &self.supervisor_inputs(), shutdown).await
    }

    /// One watcher tick over the persistent watcher state.
    pub async fn watcher_tick(&self) -> WatcherAction {
        // The state swaps out and back so no mutex guard crosses the
        // snapshot await.
        let mut state = {
            let mut guard = self
                .watcher_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::replace(&mut *guard, WatcherState::new())
        };
        let settings = self.watcher_settings();
        let registry = self.live_registry();
        let action = super::scan::watcher::poll_once(
            &mut state,
            &settings,
            &registry,
            &(self.root_dirs)(),
            &self.pool,
            now_unix(),
        )
        .await;
        if matches!(action, WatcherAction::Due) {
            let rules = super::settings::inclusion_rules(&registry);
            match watcher_request(&registry, &rules, state.changed_roots()) {
                None => tracing::debug!("filesystem watcher dropping pending scan: no scopes"),
                Some(request) => match self.coordinator.request_run(&request) {
                    Ok(result) => {
                        tracing::info!(
                            disposition = ?result.disposition,
                            "filesystem watcher requested incremental scan"
                        );
                        self.wakeups.notify("scan");
                    }
                    Err(error) => tracing::warn!(%error, "watcher scan request failed"),
                },
            }
            clear_pending(&mut state);
        }
        {
            let mut guard = self
                .watcher_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = state;
        }
        action
    }

    /// One identify tick: claim every due job, fill missing facts
    /// from the scan catalog plus disk tag reads, and run each
    /// attempt. Returns jobs attempted.
    pub async fn identify_tick(&self) -> usize {
        // No signal: the sender stays alive so the watch never fires
        // and the drain runs exactly as before.
        let (_live, quiet) = watch::channel(false);
        self.identify_tick_with_shutdown(&quiet).await
    }

    /// Shutdown-aware drain: same claim order and per-attempt
    /// semantics as [`identify_tick`](Self::identify_tick), but a
    /// signalled shutdown abandons the drain instead of pacing out
    /// the whole queue at one MusicBrainz gate slot per attempt. The
    /// signal is checked before each claim, before each gated
    /// attempt, and across the attempt itself, so SIGTERM mid-drain
    /// yields promptly. A claimed-but-unfinished job keeps its lease;
    /// boot recovery (or the lease running out) puts it back in the
    /// queue.
    pub async fn identify_tick_with_shutdown(&self, shutdown: &watch::Receiver<bool>) -> usize {
        let mut attempted = 0;
        let mut shutdown = shutdown.clone();
        loop {
            if *shutdown.borrow() {
                break;
            }
            let claimed = self
                .identify_store
                .claim(now_ms(), super::identify::queue::LEASE_SECONDS * 1000);
            let Some(job) = claimed else { break };
            let attempt = self.identify.run_claimed_job(&job.id, now_ms());
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                report = attempt => match report {
                    Some(report) => {
                        attempted += 1;
                        tracing::info!(
                            job_id = report.job.id,
                            outcome = ?report.outcome,
                            reason = report.reason_code,
                            "identify attempt finished"
                        );
                    }
                    None => {
                        tracing::warn!(job_id = job.id, "identify job vanished mid-claim");
                    }
                },
            }
        }
        attempted
    }

    /// One publish maintenance tick: purge expired operation
    /// snapshots plus expired preview seals. Returns snapshots purged.
    pub fn publish_tick(&self) -> Result<usize, PublishError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        // Reconcile-on-reopen can resume renames; a no-op refresh
        // takes no guards and bumps no revisions.
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        let purged = match cell.cell.as_mut() {
            Some(open) => {
                SnapshotStore::new(open.publisher.connection()).purge_expired(today_day())?
            }
            None => 0,
        };
        drop(cell);
        self.sweep_previews();
        Ok(purged)
    }
}

// ---------------------------------------------------------------------------
// Background loops. Thin shutdown-aware shells over the tick methods.
// ---------------------------------------------------------------------------

/// Scan supervisor loop: Hook A preamble, then drive-until-idle with
/// a shutdown-checked ceiling. The tick itself is shutdown-aware, so a
/// SIGTERM landing mid-scan stops the run instead of waiting it out.
pub(crate) async fn scan_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    {
        let setup = setup.clone();
        drive_blocking(move || async move { setup.scan_startup_recovery().await }).await;
    }
    loop {
        if *shutdown.borrow() {
            break;
        }
        let revision = setup.wakeups.revision("scan");
        let tick = {
            let (setup, shutdown) = (setup.clone(), shutdown.clone());
            drive_blocking(
                move || async move { setup.supervisor_tick_with_shutdown(&shutdown).await },
            )
            .await
        };
        if tick == Some(true) {
            continue;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = setup.wakeups.wait("scan", revision, SUPERVISOR_IDLE_CEILING) => {}
        }
    }
}

/// Filesystem watcher loop: snapshot, batch, request on due.
pub(crate) async fn watcher_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        // A snapshot of a large tree takes a while; shutdown does not wait
        // for it.
        let tick = {
            let setup = setup.clone();
            drive_blocking(move || async move { setup.watcher_tick().await })
        };
        let action = tokio::select! {
            _ = shutdown.changed() => break,
            action = tick => action.unwrap_or(WatcherAction::Idle {
                sleep_secs: setup.watcher_settings().poll_interval_seconds,
            }),
        };
        let sleep_secs = match action {
            WatcherAction::Idle { sleep_secs } | WatcherAction::Batching { sleep_secs } => {
                sleep_secs.max(0.0)
            }
            WatcherAction::Due => setup.watcher_settings().poll_interval_seconds.max(1.0),
        };
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(Duration::from_secs_f64(sleep_secs)) => {}
        }
    }
}

/// Identify queue loop: drain every due job each tick.
pub(crate) async fn identify_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        {
            let (setup, shutdown) = (setup.clone(), shutdown.clone());
            drive_blocking(
                move || async move { setup.identify_tick_with_shutdown(&shutdown).await },
            )
            .await;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(IDENTIFY_POLL) => {}
        }
    }
}

/// Drive one loop tick to completion on a blocking thread. The scan and
/// identify stores are synchronous SQLite (their calls can wait out the
/// busy timeout), so their ticks run here and never stall the async
/// workers that serve requests. `None` when the tick panicked.
async fn drive_blocking<F, Fut>(tick: F) -> Option<Fut::Output>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future,
    Fut::Output: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    match tokio::task::spawn_blocking(move || handle.block_on(tick())).await {
        Ok(output) => Some(output),
        Err(error) => {
            tracing::error!(%error, "library loop tick panicked");
            None
        }
    }
}

/// Publish maintenance loop: snapshot purge plus preview sweep.
pub(crate) async fn publish_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(PUBLISH_MAINTENANCE) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        // The tick is sync blocking work (mutexes, spin-guards, sqlite):
        // keep it off the async runtime.
        let tick_setup = setup.clone();
        match tokio::task::spawn_blocking(move || tick_setup.publish_tick()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "publish maintenance tick failed"),
            Err(error) => tracing::warn!(%error, "publish maintenance tick panicked"),
        }
    }
}
