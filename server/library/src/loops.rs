//! Library background loops and the ticks they drive.

use std::sync::{Arc, PoisonError};
use std::time::Duration;

use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::jobs::schedule::SplitMix64;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, CancellationToken, Next, Schedule, Supervisor, Tick, TickCtx, TickError,
    TickResult,
};
use tokio::runtime::Handle;

use super::clock::LibraryClock as _;
use super::publish::PublishError;
use super::publish::expiry::{self, ExpiryPass, ExpiryReport};
use super::publish::paths::{Root, Sandbox};
use super::publish::snapshots;
use super::scan::ScanStoreError;
use super::scan::coordinator::{ResolverSource as _, ScanRequestError};
use super::scan::models::EffectivePolicy;
use super::scan::supervisor::SupervisorInputs;
use super::scan::supervisor::{
    request_dirty_scopes, startup_recovery, supervise_once, supervise_once_with_shutdown,
};
use super::scan::watcher::{
    WatcherAction, WatcherSettings, WatcherState, clear_pending, watcher_request,
};
use super::wiring::LibrarySetup;

/// The library's always-on loops on the supervisor.
pub const SCAN_JOB: &str = "library-scan";
/// The filesystem watcher.
pub const WATCHER_JOB: &str = "library-watcher";
/// The identification queue.
pub const IDENTIFY_JOB: &str = "library-identify";
/// Publish maintenance.
pub const PUBLISH_JOB: &str = "library-publish";
/// The one look for leftover hidden files after startup recovery.
pub const LEFTOVERS_JOB: &str = "library-leftovers";
/// Recycle-bin expiry.
pub const RECYCLE_EXPIRY_JOB: &str = "library-recycle-expiry";

/// Every always-on library loop, by name.
pub const LOOPS: &[&str] = &[
    SCAN_JOB,
    WATCHER_JOB,
    IDENTIFY_JOB,
    super::contrib::worker::UPKEEP_JOB,
    super::operations::JOB_NAME,
    super::management::JOB_NAME,
    super::repairs::JOB_NAME,
    PUBLISH_JOB,
    RECYCLE_EXPIRY_JOB,
];

/// Scan idle ceiling: how often the scan loop looks for scheduled work
/// when nothing wakes it.
const SUPERVISOR_IDLE_CEILING: Duration = Duration::from_secs(5);

/// Longest backoff after failed scan turns, short so a failing run never
/// keeps a requested scan waiting for long.
const SCAN_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Identify queue poll cadence.
const IDENTIFY_POLL: Duration = Duration::from_secs(2);

/// After a drain that hit a store error the loop backs off from 2 s,
/// doubling up to this, plus up to [`IDENTIFY_JITTER`].
const IDENTIFY_BACKOFF_CAP: Duration = Duration::from_secs(60);
const IDENTIFY_JITTER: Duration = Duration::from_secs(2);

/// Row code of a drain that hit a store error.
const IDENTIFY_STORE_FAILED: &str = "IDENTIFY_STORE_FAILED";
/// Row code of a publish maintenance tick that failed.
const PUBLISH_FAILED: &str = "PUBLISH_MAINTENANCE_FAILED";
/// Row code of a watcher tick that did not run.
const WATCHER_FAILED: &str = "WATCHER_TICK_FAILED";
/// Row code of a turn the database runtime refused to wait in.
const TICK_REFUSED: &str = "LIBRARY_TICK_REFUSED";
/// Row code of a turn that panicked.
const TICK_PANICKED: &str = "LIBRARY_TICK_PANICKED";

/// A job the database was too busy to run waits this long, plus up to as
/// much again, keeping its attempts.
const STORE_BUSY_RETRY: Duration = Duration::from_secs(5);

/// What one identify drain did.
#[derive(Default)]
struct Drain {
    attempted: usize,
    /// A store call failed: the loop backs off before the next drain.
    store_failed: bool,
}

/// Artist ids asked about after each identification attempt, and when
/// the queue is empty.
const ATTEMPT_ARTIST_LOOKUPS: usize = 3;
const IDLE_ARTIST_LOOKUPS: usize = 20;
/// Release documents fetched when the queue is empty.
const IDLE_DOCUMENT_FETCHES: usize = 20;

/// Publish maintenance cadence (snapshot purge plus preview sweep).
const PUBLISH_MAINTENANCE: Duration = Duration::from_secs(3600);

/// Recycle-bin expiry cadence, its first pass after start, and the
/// spread added to each.
const RECYCLE_EXPIRY: Duration = Duration::from_secs(3600);
const RECYCLE_EXPIRY_FIRST: Duration = Duration::from_secs(600);
const RECYCLE_EXPIRY_JITTER: Duration = Duration::from_secs(300);
/// Rows an expiry pass reads at a time.
const RECYCLE_EXPIRY_PAGE: usize = 200;
/// Row code of an expiry pass that failed.
const RECYCLE_EXPIRY_FAILED: &str = "RECYCLE_EXPIRY_FAILED";

impl LibrarySetup {
    /// Supervisor inputs, rebuilt per call so settings-swap rebuilds
    /// never strand the loop on stale getters.
    pub(crate) fn supervisor_inputs(&self) -> SupervisorInputs {
        SupervisorInputs {
            root_paths: self.root_dirs.clone(),
            schedule: {
                let schedule = self.settings.schedule.clone();
                Arc::new(move || super::settings::schedule(&schedule))
            },
            inclusion_rules: {
                let registry = self.registry.clone();
                Arc::new(move || super::settings::inclusion_rules(registry.resolver().registry()))
            },
            dirty: self.dirty.clone(),
            wakeups: self.wakeups.clone(),
            now_unix: {
                let clock = Arc::clone(&self.clock);
                Arc::new(move || clock.now_secs())
            },
        }
    }

    /// Watcher settings, re-read from the config store every tick.
    pub(crate) fn watcher_settings(&self) -> WatcherSettings {
        super::settings::watcher(&self.settings.watcher)
    }

    /// One-shot scan startup reconciliation (Hook A). The loop runs
    /// this in its preamble; tests drive it directly.
    pub async fn scan_startup_recovery(&self) {
        self.refresh_registry();
        startup_recovery(&self.coordinator, &self.supervisor_inputs()).await;
    }

    /// The saved library roots changed: swap them in and ask now for the
    /// scans that new or changed roots need, so a folder added anywhere is
    /// scanned without waiting for the scan loop's next pass. Blocking.
    pub fn roots_saved(&self) {
        self.refresh_registry();
        request_dirty_scopes(&self.coordinator, &self.supervisor_inputs());
    }

    /// One supervisor iteration. Returns true when a run was driven.
    pub async fn supervisor_tick(&self) -> bool {
        self.refresh_registry();
        supervise_once(&self.coordinator, &self.supervisor_inputs()).await
    }

    /// One shutdown-aware supervisor iteration: same Hook B, schedule,
    /// and worker semantics as [`supervisor_tick`](Self::supervisor_tick),
    /// but a cancelled `stop` ends the in-flight scan instead of waiting
    /// it out. The run keeps its state and the next start resumes it. A
    /// stop already cancelled claims no new work.
    pub async fn supervisor_tick_with_shutdown(&self, stop: &CancellationToken) -> bool {
        self.refresh_registry();
        supervise_once_with_shutdown(&self.coordinator, &self.supervisor_inputs(), stop).await
    }

    /// One watcher tick over the persistent watcher state.
    pub async fn watcher_tick(&self) -> WatcherAction {
        self.watcher_tick_with_shutdown(&CancellationToken::new())
            .await
    }

    /// One watcher tick that stops its tree snapshot once `stop` is
    /// cancelled; an interrupted root keeps its previous baseline.
    pub async fn watcher_tick_with_shutdown(&self, stop: &CancellationToken) -> WatcherAction {
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
            self.clock.now_secs(),
            stop,
        )
        .await;
        if matches!(action, WatcherAction::Due) {
            let rules = super::settings::inclusion_rules(&registry);
            let asked = match watcher_request(&registry, &rules, state.changed_roots()) {
                None => {
                    tracing::debug!("filesystem watcher dropping pending scan: no scopes");
                    true
                }
                Some(request) => match self.coordinator.request_run(&request) {
                    Ok(result) => {
                        tracing::info!(
                            disposition = ?result.disposition,
                            "filesystem watcher requested incremental scan"
                        );
                        self.wakeups.notify("scan");
                        true
                    }
                    // A busy database: the changes stay pending and the
                    // next poll asks again.
                    Err(
                        error @ ScanRequestError::Store(
                            ScanStoreError::Busy { .. } | ScanStoreError::LaneClosed,
                        ),
                    ) => {
                        tracing::warn!(%error, "watcher scan request waits: the database is busy");
                        false
                    }
                    Err(error) => {
                        tracing::warn!(%error, "watcher scan request failed");
                        true
                    }
                },
            };
            if asked {
                clear_pending(&mut state);
            }
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
        self.identify_tick_with_shutdown(&CancellationToken::new())
            .await
    }

    /// Shutdown-aware drain: same claim order and per-attempt
    /// semantics as [`identify_tick`](Self::identify_tick), but a
    /// cancelled `stop` abandons the drain instead of pacing out the
    /// whole queue at one MusicBrainz gate slot per attempt. The stop is
    /// checked before each claim, before each gated attempt, and across
    /// the attempt itself, so a shutdown mid-drain yields promptly. A claimed-but-unfinished job keeps its lease;
    /// boot recovery (or the lease running out) puts it back in the
    /// queue.
    pub async fn identify_tick_with_shutdown(&self, stop: &CancellationToken) -> usize {
        self.identify_drain(stop).await.attempted
    }

    /// [`identify_tick_with_shutdown`](Self::identify_tick_with_shutdown),
    /// also saying whether a store call failed, so the loop backs off.
    async fn identify_drain(&self, stop: &CancellationToken) -> Drain {
        let mut drain = Drain::default();
        loop {
            if stop.is_cancelled() {
                break;
            }
            // Artist links follow every change of a match, whoever made it,
            // so they are swept before each claim, paused or not.
            if let Err(error) = self.identify_store.sweep_artist_links() {
                tracing::warn!(%error, "artist link sweep stopped; the next one goes on");
                drain.store_failed = true;
            }
            // An administrator's pause stops new claims; a job already
            // running finishes.
            if self.identification_paused() || !self.settings.identification_readable() {
                break;
            }
            let claimed = self.identify_store.claim(
                self.clock.now_ms_u64(),
                super::identify::queue::LEASE_SECONDS * 1000,
            );
            let job = match claimed {
                Ok(Some(job)) => job,
                Ok(None) => {
                    self.resolve_merged_artist_ids(IDLE_ARTIST_LOOKUPS, stop)
                        .await;
                    self.fetch_waiting_documents(stop).await;
                    break;
                }
                Err(error) => {
                    tracing::warn!(%error, "no identify job claimed: the queue could not be read");
                    drain.store_failed = true;
                    break;
                }
            };
            let attempt = self
                .identify
                .run_claimed(job.clone(), self.clock.now_ms_u64());
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                report = attempt => match report {
                    Ok(Some(report)) => {
                        drain.attempted += 1;
                        // What the match proves about the album's artists,
                        // before automatic management plans the album.
                        if let Err(error) = self
                            .identify_store
                            .link_and_merge_album_artists(&report.job.local_album_id)
                        {
                            tracing::warn!(
                                %error,
                                album_id = report.job.local_album_id,
                                "the album's artist links could not be worked out; a later sweep tries again"
                            );
                        }
                        // An artist id the pass found disagreeing may be one
                        // MusicBrainz merged; asking now lets the answer land
                        // before a managed run writes the current id.
                        self.resolve_merged_artist_ids(ATTEMPT_ARTIST_LOOKUPS, stop)
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
                    Ok(None) => {
                        tracing::warn!(job_id = job.id, "identify job vanished or went back to the queue mid-claim");
                    }
                    Err(error) => {
                        // The loop backs off before it claims again.
                        drain.store_failed = true;
                        self.after_store_error(job, error);
                        break;
                    }
                },
            }
        }
        drain
    }

    /// Settle a claimed job whose attempt hit a store error. Busy: the
    /// job waits a few seconds and keeps its attempts. Shutting down:
    /// nothing is written; boot recovery requeues it. Anything else: the
    /// job takes a step on its backoff ladder as `STORE_FAILED`, so one
    /// that never reads ends in Attention instead of retrying for ever. A
    /// write that fails here leaves the job to its lease.
    fn after_store_error(
        &self,
        mut job: super::identify::models::IdentifyJob,
        error: super::identify::stores::StoreError,
    ) {
        use super::identify::models::JobState;
        use super::identify::stores::{AttemptLanding, StoreError, land_job};
        tracing::warn!(
            %error,
            job_id = job.id,
            album_id = job.local_album_id,
            "identify attempt stopped on a store error"
        );
        let now = self.clock.now_ms_u64();
        match &error {
            StoreError::Closed => return,
            StoreError::Busy(_) => {
                let jitter = SplitMix64::new(SplitMix64::seed_from_time())
                    .below_duration(STORE_BUSY_RETRY)
                    .as_millis() as u64;
                job.state = JobState::Deferred;
                job.not_before_ms = now + STORE_BUSY_RETRY.as_millis() as u64 + jitter;
            }
            StoreError::Failed(_) => {
                land_job(
                    &mut job,
                    AttemptLanding::Deferred,
                    now,
                    Some("STORE_FAILED"),
                );
            }
        }
        match self.identify_store.land(&job) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    job_id = job.id,
                    "identify job taken over by another claim; left to it"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    job_id = job.id,
                    "identify job not put back; its lease brings it back"
                );
            }
        }
    }

    /// Ask MusicBrainz about up to `limit` artist ids the passes found
    /// disagreeing, unless identification is paused. Shutdown stops the
    /// lookups; an id not answered stays pending.
    async fn resolve_merged_artist_ids(&self, limit: usize, stop: &CancellationToken) {
        if stop.is_cancelled() || self.identification_paused() {
            return;
        }
        tokio::select! {
            biased;
            () = stop.cancelled() => {}
            resolved = self.identify.resolve_merged_artist_ids(limit) => {
                if let Err(error) = resolved {
                    tracing::warn!(%error, "artist id lookups stopped on a store error");
                }
            }
        }
    }

    /// Fetch release documents that albums with an accepted match wait for,
    /// so the artist link sweep can prove their artists.
    async fn fetch_waiting_documents(&self, stop: &CancellationToken) {
        if stop.is_cancelled() || self.identification_paused() {
            return;
        }
        tokio::select! {
            biased;
            () = stop.cancelled() => {}
            pass = self.identify.fetch_waiting_documents(IDLE_DOCUMENT_FETCHES) => match pass {
                Ok(pass) if pass.fetched + pass.not_found + pass.refused > 0 => tracing::info!(
                    fetched = pass.fetched,
                    not_found = pass.not_found,
                    refused = pass.refused,
                    "release documents fetched for matched albums"
                ),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "release document fetches stopped on a store error");
                }
            }
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
        // renames.
        let session = cell.session(&registry);
        let reopen = cell.needs_refresh(&session);
        let retry = !reopen && cell.has_held()?;
        cell.refresh_to(session, &self.root_dirs)?;
        if retry {
            cell.retry_held()?;
        }
        // Under the cell, so no publish is between storing a blob and
        // referring to it. Each step is a short background write, never
        // held back while the WAL is full, so the cell is not held for
        // long.
        let purged = match cell.cell.as_ref() {
            Some(_) => {
                let purged = snapshots::purge_expired(&self.db, self.clock.today_day())?;
                // Generated files a preview planned (covers, lyrics) wait in
                // the blob store until their job is done with them.
                self.db
                    .write_blocking(Lane::Background, "publish.drop_preview_refs", |tx| {
                        tx.prepare_cached(super::publish::sql::DROP_PREVIEW_REFS.text())?
                            .execute([])?;
                        Ok::<_, PublishError>(())
                    })?;
                let freed = snapshots::sweep_unreferenced_blobs(&self.db)?;
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

    /// One recycle-bin expiry pass, on the retention the download settings
    /// give now. Skipped while those or the library settings cannot be
    /// read. The deletes wait for the publish lock, so no put-back or
    /// removal is reading a copy while it goes. `go_on` is asked between
    /// rows. Blocking.
    pub fn recycle_expiry_tick(
        &self,
        go_on: &dyn Fn() -> bool,
    ) -> Result<ExpiryReport, PublishError> {
        let Some(policy) = self.settings.download_policy.read() else {
            return Ok(ExpiryReport::default());
        };
        let pass = ExpiryPass {
            retention_days: policy.recycle_retention_days,
            now_ms: self.clock.now_ms(),
            page: RECYCLE_EXPIRY_PAGE,
            due_before_ms: None,
        };
        self.expiry_pass(&pass, go_on)
    }

    /// Empty the recycle bin now: the expiry pass with no retention, over
    /// the copies that reached the bin before `before_ms` (unix ms, never
    /// later than now) and the old entries stamped before it. Every rule
    /// of the expiry holds; `finished` is false when it stopped early or
    /// the bins could not be told. Blocking.
    pub fn empty_recycle_bin(
        &self,
        before_ms: i64,
        go_on: &dyn Fn() -> bool,
    ) -> Result<ExpiryReport, PublishError> {
        let now_ms = self.clock.now_ms();
        let pass = ExpiryPass {
            retention_days: 0,
            now_ms,
            page: RECYCLE_EXPIRY_PAGE,
            due_before_ms: Some(before_ms.min(now_ms)),
        };
        self.expiry_pass(&pass, go_on)
    }

    fn expiry_pass(
        &self,
        pass: &ExpiryPass,
        go_on: &dyn Fn() -> bool,
    ) -> Result<ExpiryReport, PublishError> {
        let report = expiry::expire(
            &self.db,
            pass,
            &self.expiry,
            &|| self.expiry_sandbox(),
            || self.publish.lock().unwrap_or_else(PoisonError::into_inner),
            go_on,
        )?;
        if report.did_anything() {
            tracing::info!(
                deleted = report.deleted,
                gone = report.gone,
                changed = report.changed,
                kept = report.kept,
                waited = report.waited,
                legacy_deleted = report.legacy_deleted,
                finished = report.finished,
                "recycle bin expiry pass"
            );
        }
        Ok(report)
    }

    /// The library folders and the bins expiry may delete from, as the
    /// settings give them now. `None` while the library settings, or the
    /// settings the bins come from, cannot be read.
    pub fn expiry_sandbox(&self) -> Option<Sandbox> {
        let library = self.settings.library.read()?;
        let registry = super::settings::registry_from(&library);
        let (bins, dropped) = match self.bins.for_expiry(&registry) {
            Ok(found) => found,
            Err(cause) => {
                self.expiry.note(
                    "bins",
                    Some(format!("recycle bin expiry waits while {cause}")),
                );
                return None;
            }
        };
        self.expiry
            .note("bins", (!dropped.is_empty()).then(|| dropped.join("; ")));
        let mut roots: Vec<Root> = registry
            .roots()
            .iter()
            .map(|root| match root.policy {
                EffectivePolicy::Excluded => Root::remove_only(root.id.clone(), root.path.clone()),
                _ => Root::library(root.id.clone(), root.path.clone()),
            })
            .collect();
        roots.extend(bins);
        Sandbox::new(roots).ok()
    }
}

// ---------------------------------------------------------------------------
// Background loops on the supervisor, over the tick methods above.
// ---------------------------------------------------------------------------

/// Start the library's loops on `supervisor`: the always-on [`LOOPS`] and
/// the one [`LEFTOVERS_JOB`] look. Called from inside the runtime, after
/// startup recovery.
pub(crate) fn start_loops(
    setup: &LibrarySetup,
    supervisor: &Supervisor,
) -> Result<(), AlreadyRunning> {
    let handle = Handle::current();
    let mut scans = setup.wakeups.subscribe("scan");
    // The first turn runs at once: startup recovery, then any work queued.
    scans.mark_changed();
    supervisor.on_wakeup_blocking(
        SCAN_JOB,
        scans,
        Schedule::new(SUPERVISOR_IDLE_CEILING)
            .with_backoff(Duration::from_secs(1), SCAN_BACKOFF_CAP),
        scan_tick(setup.clone(), handle.clone()),
    )?;
    supervisor.periodic(
        WATCHER_JOB,
        Schedule::new(Duration::ZERO).with_backoff(Duration::from_secs(1), Duration::from_secs(60)),
        WatcherTick {
            setup: setup.clone(),
            handle: handle.clone(),
        },
    )?;
    let mut identification = setup.wakeups.subscribe("identification");
    identification.mark_changed();
    supervisor.on_wakeup_blocking(
        IDENTIFY_JOB,
        identification,
        Schedule::new(IDENTIFY_POLL)
            .with_jitter(IDENTIFY_JITTER)
            .with_backoff(Duration::from_secs(1), IDENTIFY_BACKOFF_CAP),
        identify_tick(setup.clone(), handle),
    )?;
    super::contrib::worker::start_upkeep(supervisor, setup.contrib_worker.clone())?;
    super::operations::start(setup, supervisor)?;
    super::management::start(setup, supervisor)?;
    super::repairs::start(setup, supervisor)?;
    let leftovers = setup.clone();
    supervisor.one_shot(LEFTOVERS_JOB, move |ctx| leftover_pass(leftovers, ctx))?;
    let publish = setup.clone();
    supervisor.periodic_blocking(
        PUBLISH_JOB,
        Schedule::new(PUBLISH_MAINTENANCE).with_initial_delay(PUBLISH_MAINTENANCE),
        move |_ctx| match caught("publish maintenance", || publish.publish_tick())? {
            Ok(_) => Ok(Next::Continue),
            Err(error) => Err(TickError::new(PUBLISH_FAILED, error)),
        },
    )?;
    let expiry = setup.clone();
    supervisor.periodic_blocking(
        RECYCLE_EXPIRY_JOB,
        Schedule::new(RECYCLE_EXPIRY)
            .with_initial_delay(RECYCLE_EXPIRY_FIRST)
            .with_jitter(RECYCLE_EXPIRY_JITTER)
            .with_backoff(Duration::from_secs(60), RECYCLE_EXPIRY),
        move |ctx| {
            let go_on = || {
                ctx.heartbeat();
                !ctx.token().is_cancelled()
            };
            match caught("recycle bin expiry", || expiry.recycle_expiry_tick(&go_on))? {
                Ok(_) => Ok(Next::Continue),
                Err(error) => Err(TickError::new(RECYCLE_EXPIRY_FAILED, error)),
            }
        },
    )
}

/// Run `work`, a panic turned into a failed turn: the loop backs off and
/// carries on, as a panicked turn always has.
fn caught<T>(what: &'static str, work: impl FnOnce() -> T) -> Result<T, TickError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .map_err(|_| TickError::new(TICK_PANICKED, format!("{what} panicked")))
}

/// Drive one async turn to completion on this blocking thread. The scan
/// and identify stores are synchronous SQLite, so their turns run on a
/// blocking thread and never stall the async workers that serve requests.
fn drive<F: std::future::Future>(
    handle: &Handle,
    what: &'static str,
    turn: F,
) -> Result<F::Output, TickError> {
    caught(what, || {
        droppedneedle_kernel::db::block_on(handle, what, turn)
    })?
    .map_err(|error| TickError::new(TICK_REFUSED, format!("{what}: {error}")))
}

/// The scan loop's turn: startup recovery once, then every run that is
/// due until none is left, then the album art and lyrics sweeps when a run
/// changed something. A cancelled stop ends the run in flight; the next
/// start resumes it.
fn scan_tick(
    setup: LibrarySetup,
    handle: Handle,
) -> impl FnMut(&TickCtx) -> TickResult + Send + 'static {
    let mut recovered = false;
    // Album art and the lyrics marks are refreshed once the scans go idle,
    // and once at boot.
    let mut sweeps_due = true;
    move |ctx| {
        let stop = ctx.token();
        if !recovered {
            recovered = true;
            if let Err(error) = drive(
                &handle,
                "library_scan_recovery",
                setup.scan_startup_recovery(),
            ) {
                tracing::error!(%error, "scan startup recovery did not run");
            }
        }
        let mut revision = setup.wakeups.revision("scan");
        while !stop.is_cancelled() {
            revision = setup.wakeups.revision("scan");
            if !drive(
                &handle,
                "library_scan_tick",
                setup.supervisor_tick_with_shutdown(stop),
            )? {
                break;
            }
            sweeps_due = true;
            ctx.heartbeat();
        }
        if sweeps_due && !stop.is_cancelled() {
            // A sweep of its own: a panic there waits for the next scan and
            // never holds up the runs.
            sweeps_due = match caught("library sweep", || setup.sweep(revision, stop)) {
                Ok(complete) => !complete,
                Err(error) => {
                    tracing::error!(%error, "library sweep panicked; it waits for the next scan");
                    false
                }
            };
        }
        Ok(Next::Continue)
    }
}

impl LibrarySetup {
    /// Refresh album art and the lyrics marks, giving way to a stop and to
    /// any scan request (a wakeup past `revision`, or a queued run). True
    /// when both finished; a catalog that cannot be read is logged and
    /// leaves the sweep due.
    fn sweep(&self, revision: u64, stop: &CancellationToken) -> bool {
        let polls = std::cell::Cell::new(0u32);
        let give_way = || {
            polls.set(polls.get().wrapping_add(1));
            stop.is_cancelled()
                || self.wakeups.revision("scan") != revision
                || (polls.get() % 32 == 0 && !self.coordinator.current().is_empty())
        };
        let artwork = self.scan_store.refresh_album_artwork(&give_way);
        let lyrics = self.scan_store.refresh_track_lyrics(&give_way);
        match (artwork, lyrics) {
            (Ok(artwork), Ok(lyrics)) => artwork.complete && lyrics.complete,
            (artwork, lyrics) => {
                for error in [artwork.err(), lyrics.err()].into_iter().flatten() {
                    tracing::warn!(%error, "library sweep could not read the catalog");
                }
                false
            }
        }
    }
}

/// The identify loop's turn: drain every due job. A store error backs the
/// loop off; a resume wakes it at once.
fn identify_tick(
    setup: LibrarySetup,
    handle: Handle,
) -> impl FnMut(&TickCtx) -> TickResult + Send + 'static {
    move |ctx| {
        let drain = drive(
            &handle,
            "library_identify_tick",
            setup.identify_drain(ctx.token()),
        )?;
        if drain.store_failed {
            return Err(TickError::new(
                IDENTIFY_STORE_FAILED,
                "a store call failed during the drain",
            ));
        }
        Ok(Next::Continue)
    }
}

/// The filesystem watcher's turn: snapshot, batch, request on due, then
/// wait as long as the saved settings say.
struct WatcherTick {
    setup: LibrarySetup,
    handle: Handle,
}

impl Tick for WatcherTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let (setup, handle, stop) = (self.setup.clone(), self.handle.clone(), ctx.token().clone());
        // A snapshot of a large tree takes a while; the stop does not wait
        // for it.
        let action = tokio::task::spawn_blocking(move || {
            drive(
                &handle,
                "library_watcher_tick",
                setup.watcher_tick_with_shutdown(&stop),
            )
        })
        .await
        .map_err(|error| TickError::new(WATCHER_FAILED, error))??;
        let sleep_secs = match action {
            WatcherAction::Idle { sleep_secs } | WatcherAction::Batching { sleep_secs } => {
                sleep_secs.max(0.0)
            }
            WatcherAction::Due => self.setup.watcher_settings().poll_interval_seconds.max(1.0),
        };
        ctx.sleep(Duration::from_secs_f64(sleep_secs)).await;
        Ok(Next::Continue)
    }
}

/// Look once for hidden files no journal row names. The walk runs off the
/// async runtime and gives up between two folders once the stop comes.
async fn leftover_pass(setup: LibrarySetup, ctx: TickCtx) -> Result<(), TickError> {
    let stop = ctx.token().clone();
    let walk = tokio::task::spawn_blocking(move || {
        let registry = setup.live_registry();
        setup
            .leftover_files
            .refresh(&registry, &|| stop.is_cancelled())
    });
    match walk.await {
        Ok(Ok(Some(report))) if report.count > 0 => tracing::warn!(
            count = report.count,
            "hidden files no publish journal row names were found; nothing was removed"
        ),
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "leftover pass failed"),
        Err(error) => tracing::warn!(%error, "leftover pass panicked"),
    }
    Ok(())
}
