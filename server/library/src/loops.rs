//! Library background loops and the ticks they drive.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::clock::{now_ms, now_unix, today_day};
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

/// Artist ids asked about after each identification attempt, and when
/// the queue is empty.
const ATTEMPT_ARTIST_LOOKUPS: usize = 3;
const IDLE_ARTIST_LOOKUPS: usize = 20;

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
        let (_live, quiet) = watch::channel(false);
        self.watcher_tick_with_shutdown(&quiet).await
    }

    /// One watcher tick that stops its tree snapshot once `shutdown` is
    /// signalled; an interrupted root keeps its previous baseline.
    pub async fn watcher_tick_with_shutdown(
        &self,
        shutdown: &watch::Receiver<bool>,
    ) -> WatcherAction {
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
            shutdown,
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
            // Artist links follow every change of a match, whoever made it,
            // so they are swept before each claim, paused or not.
            self.identify_store.sweep_artist_links();
            // An administrator's pause stops new claims; a job already
            // running finishes.
            if self.identification_paused() {
                break;
            }
            let claimed = self
                .identify_store
                .claim(now_ms(), super::identify::queue::LEASE_SECONDS * 1000);
            let Some(job) = claimed else {
                self.resolve_merged_artist_ids(IDLE_ARTIST_LOOKUPS, &mut shutdown)
                    .await;
                break;
            };
            let attempt = self.identify.run_claimed_job(&job.id, now_ms());
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                report = attempt => match report {
                    Some(report) => {
                        attempted += 1;
                        // What the match proves about the album's artists,
                        // before automatic management plans the album.
                        self.identify_store
                            .link_and_merge_album_artists(&report.job.local_album_id);
                        // An artist id the pass found disagreeing may be one
                        // MusicBrainz merged; asking now lets the answer land
                        // before a managed run writes the current id.
                        self.resolve_merged_artist_ids(ATTEMPT_ARTIST_LOOKUPS, &mut shutdown)
                            .await;
                        // A settled match is what automatic management
                        // waits for after a scan finds an album.
                        if matches!(
                            report.outcome,
                            super::identify::models::IdentificationOutcome::Identified
                                | super::identify::models::IdentificationOutcome::EditionUncertain
                                | super::identify::models::IdentificationOutcome::KeptTagged
                        ) {
                            let setup = self.clone();
                            let album = report.job.local_album_id.clone();
                            let queued = tokio::task::spawn_blocking(move || {
                                setup.queue_automatic_management(&album);
                            })
                            .await;
                            if let Err(error) = queued {
                                tracing::warn!(%error, "automatic management queueing did not run");
                            }
                        }
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

    /// Ask MusicBrainz about up to `limit` artist ids the passes found
    /// disagreeing, unless identification is paused. Shutdown stops the
    /// lookups; an id not answered stays pending.
    async fn resolve_merged_artist_ids(&self, limit: usize, shutdown: &mut watch::Receiver<bool>) {
        if *shutdown.borrow() || self.identification_paused() {
            return;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => {}
            _ = self.identify.resolve_merged_artist_ids(limit) => {}
        }
    }

    /// One publish maintenance tick: purge expired operation
    /// snapshots plus expired preview seals. Returns snapshots purged.
    pub fn publish_tick(&self) -> Result<usize, PublishError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        // Reconcile-on-reopen (and the retry of held bundles) can resume
        // renames; a no-op tick takes no guards and bumps no revisions.
        let reopen = cell.needs_refresh(&registry);
        let retry = !reopen && cell.has_held();
        let _guards = if reopen || retry {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        if retry {
            cell.retry_held()?;
        }
        let purged = match cell.cell.as_mut() {
            Some(open) => {
                let conn = open.publisher.connection();
                let purged = SnapshotStore::new(conn).purge_expired(today_day())?;
                // Generated files a preview planned (covers, lyrics) wait in
                // the blob store until their job is done with them.
                conn.execute(
                    "DELETE FROM library_publish_blob_refs WHERE owner_kind = 'preview' \
                     AND owner_id NOT IN (SELECT id FROM library_operation_jobs \
                     WHERE state NOT IN ('succeeded', 'cancelled'))",
                    [],
                )?;
                let freed = super::publish::snapshots::BlobStore::new(conn).sweep_unreferenced()?;
                if freed > 0 {
                    tracing::info!(freed, "freed before-states no undo needs any more");
                }
                purged
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
    // Album art and the lyrics marks are refreshed once the scans go idle,
    // and once at boot.
    let mut sweeps_due = true;
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
            sweeps_due = true;
            continue;
        }
        if sweeps_due {
            // The sweeps give way to shutdown and to any scan request (a
            // wakeup, or a queued run), and resume on a later pass.
            let (sweep_setup, shutdown) = (setup.clone(), shutdown.clone());
            let swept = drive_blocking(move || async move {
                let wakeups = sweep_setup.wakeups.clone();
                let coordinator = sweep_setup.coordinator.clone();
                let polls = std::cell::Cell::new(0u32);
                let stop = || {
                    polls.set(polls.get().wrapping_add(1));
                    *shutdown.borrow()
                        || wakeups.revision("scan") != revision
                        || (polls.get() % 32 == 0 && !coordinator.current().is_empty())
                };
                // A catalog that cannot be read is logged and leaves the
                // sweep due, so a later pass runs it again.
                let artwork = sweep_setup.scan_store.refresh_album_artwork(&stop);
                let lyrics = sweep_setup.scan_store.refresh_track_lyrics(&stop);
                match (artwork, lyrics) {
                    (Ok(artwork), Ok(lyrics)) => artwork.complete && lyrics.complete,
                    (artwork, lyrics) => {
                        for error in [artwork.err(), lyrics.err()].into_iter().flatten() {
                            tracing::warn!(%error, "library sweep could not read the catalog");
                        }
                        false
                    }
                }
            })
            .await;
            // A panicked sweep (already logged) waits for the next scan.
            sweeps_due = swept.is_some_and(|complete| !complete);
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
            let (setup, shutdown) = (setup.clone(), shutdown.clone());
            drive_blocking(move || async move { setup.watcher_tick_with_shutdown(&shutdown).await })
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
        let revision = setup.wakeups.revision("identification");
        {
            let (setup, shutdown) = (setup.clone(), shutdown.clone());
            drive_blocking(
                move || async move { setup.identify_tick_with_shutdown(&shutdown).await },
            )
            .await;
        }
        // Resume wakes the loop at once; otherwise it polls.
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = setup.wakeups.wait("identification", revision, IDENTIFY_POLL) => {}
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
    let driven = tokio::task::spawn_blocking(move || {
        droppedneedle_kernel::db::block_on(&handle, "library_loop_tick", tick())
    });
    match driven.await {
        Ok(Ok(output)) => Some(output),
        Ok(Err(error)) => {
            tracing::error!(%error, "library loop tick refused");
            None
        }
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
