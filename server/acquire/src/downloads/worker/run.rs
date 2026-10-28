//! One pass of every step, and the loop that runs it on the supervisor.

use std::sync::Arc;

use super::{DOWNLOAD_LANDINGS_JOB, DOWNLOAD_WORKER_JOB, DownloadWorker, Pass, WORKER_INTERVAL};
use crate::downloads::watchdog::Watchdog;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickResult,
};

/// Orphan sweep runs every Nth pass (hourly at the 30s cadence).
const ORPHAN_EVERY_NTH_PASS: u64 = 120;

/// Run the worker on the supervisor: a pass at once, then one on every
/// wake and at least every [`WORKER_INTERVAL`]. At shutdown the landings it
/// started finish within the grace, under [`DOWNLOAD_LANDINGS_JOB`].
pub fn spawn_download_worker(
    worker: Arc<DownloadWorker>,
    supervisor: &Supervisor,
) -> Result<(), AlreadyRunning> {
    let mut wake = worker.wake.subscribe();
    wake.mark_changed();
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    supervisor.on_wakeup(
        DOWNLOAD_WORKER_JOB,
        wake,
        Schedule::new(WORKER_INTERVAL),
        WorkerTick {
            worker: Arc::clone(&worker),
            gate: Arc::clone(&gate),
            pass: 0,
        },
    )?;
    supervisor.one_shot(DOWNLOAD_LANDINGS_JOB, move |ctx| async move {
        ctx.token().cancelled().await;
        // A pass still running may start a landing: wait for it first.
        let _pass = gate.lock().await;
        worker.wait_for_landings().await;
        Ok(())
    })
}

/// One worker pass. The gate keeps a pass and the shutdown wait for
/// landings apart, so no landing starts after that wait began.
struct WorkerTick {
    worker: Arc<DownloadWorker>,
    gate: Arc<tokio::sync::Mutex<()>>,
    pass: u64,
}

impl Tick for WorkerTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let _pass = self.gate.lock().await;
        if ctx.token().is_cancelled() {
            return Ok(Next::Finish);
        }
        self.worker.run_once(self.pass).await;
        self.pass = self.pass.wrapping_add(1);
        Ok(Next::Continue)
    }
}

impl DownloadWorker {
    /// Run every steady-state pass once, in order. The orphan sweep only
    /// runs when `pass` hits its hourly slot. Landings the poll pass
    /// starts run on their own tasks; [`Self::wait_for_landings`] awaits
    /// them.
    pub async fn run_once(self: &Arc<Self>, pass: u64) {
        // Settings that do not read stop the whole pass: nothing starts,
        // lands or fails over until they are saved again.
        let Some(config) = self.inputs.config() else {
            return;
        };
        let library = self.check_library().await;
        self.note_library_wait(library.as_ref());
        let pass_ctx = Pass {
            library,
            watchdog: Watchdog::new(config.watchdog.clone()),
            sources: self.inputs.sources(),
            config,
            now: self.now_unix_f64(),
        };
        self.enqueue_pass(&pass_ctx).await;
        self.poll_pass(&pass_ctx).await;
        self.retry_pass(&pass_ctx).await;
        self.cleanup_pass(&pass_ctx).await;
        if pass.is_multiple_of(ORPHAN_EVERY_NTH_PASS) {
            self.release_kept(&pass_ctx).await;
            self.orphan_pass(&pass_ctx).await;
        }
    }
}
