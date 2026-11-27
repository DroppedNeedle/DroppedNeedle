//! The sound analysis loop on the supervisor.
//!
//! It is started by the server's builder, which has both the library and
//! media's transcode count in hand, and hands that count in as `busy`.

use std::time::Duration;

use droppedneedle_kernel::jobs::schedule::SplitMix64;
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, TickCtx, TickError, TickResult,
};
use tokio::runtime::Handle;

use super::{PACE, SoundAnalysis, SoundError, Sweep};
use crate::scan::watcher::WorkWakeups;

/// The loop's name on the supervisor, as the admin jobs view shows it.
pub const SOUND_JOB: &str = "library-sound-analysis";

/// How often the loop looks again when nothing wakes it: soon after the
/// switch is turned on or a reason to wait goes. The loudness loop's
/// wakeup (a scan that changed the catalog, a publish that rewrote files)
/// wakes it too. Its first look waits a minute after boot.
const EVERY: Duration = Duration::from_secs(30);
const FIRST: Duration = Duration::from_secs(60);
const JITTER: Duration = Duration::from_secs(5);
/// A pass that found nothing to do is run again at most this often while
/// nothing wakes the loop, for files that waited out a failed read.
const DONE_RECHECK: Duration = Duration::from_secs(600);
const BUSY_RETRY: Duration = Duration::from_secs(5);
const BUSY_CAP: Duration = Duration::from_secs(300);
/// Row code of a pass that failed.
const SOUND_FAILED: &str = "SOUND_ANALYSIS_FAILED";
/// Row code of a pass that panicked.
const SOUND_PANICKED: &str = "SOUND_ANALYSIS_PANICKED";

/// Start the loop. `busy` answers true while someone plays through a
/// transcode; the loop waits then.
pub fn spawn_on(
    supervisor: &Supervisor,
    analysis: SoundAnalysis,
    wakeups: &WorkWakeups,
    busy: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<(), AlreadyRunning> {
    let mut asked = wakeups.subscribe(crate::loudness::WAKEUP);
    asked.mark_changed();
    supervisor.on_wakeup_blocking(
        SOUND_JOB,
        asked,
        Schedule::new(EVERY)
            .with_initial_delay(FIRST)
            .with_jitter(JITTER)
            .with_backoff(BUSY_RETRY, BUSY_CAP),
        tick(analysis, wakeups.clone(), Handle::current(), busy),
    )
}

/// One turn: a pass, unless the last one found nothing to do and nothing
/// has changed since. A busy database is no failure: the turn waits a few
/// seconds, keeps the results it holds and asks for the next turn.
fn tick(
    analysis: SoundAnalysis,
    wakeups: WorkWakeups,
    handle: Handle,
    busy: impl Fn() -> bool + Send + Sync + 'static,
) -> impl FnMut(&TickCtx) -> TickResult + Send + 'static {
    let mut sweep = Sweep::default();
    let mut settings = analysis.settings.subscribe();
    let mut finished: Option<(u64, std::time::Instant)> = None;
    move |ctx| {
        let woken = wakeups.revision(crate::loudness::WAKEUP);
        let changed = settings.has_changed().unwrap_or(true);
        settings.mark_unchanged();
        if !changed
            && finished.is_some_and(|(at, when)| at == woken && when.elapsed() < DONE_RECHECK)
        {
            return Ok(Next::Continue);
        }
        let stop = ctx.token();
        let rest = |wait: Duration| {
            ctx.heartbeat();
            matches!(
                droppedneedle_kernel::db::block_on(&handle, "library_sound_rest", ctx.sleep(wait)),
                Ok(true)
            )
        };
        let pass = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            analysis.pass(&mut sweep, &PACE, &|| stop.is_cancelled(), &busy, &rest)
        }))
        .map_err(|_| TickError::new(SOUND_PANICKED, "sound analysis pass panicked"))?;
        finished = None;
        match pass {
            Ok(report) => {
                if report.finished && report.held_back.is_none() {
                    finished = Some((woken, std::time::Instant::now()));
                }
                Ok(Next::Continue)
            }
            Err(SoundError::Busy(cause)) => {
                tracing::info!(%cause, "sound analysis waits: the database is busy");
                let jitter =
                    SplitMix64::new(SplitMix64::seed_from_time()).below_duration(BUSY_RETRY);
                rest(BUSY_RETRY + jitter);
                Ok(Next::Continue)
            }
            Err(error) => Err(TickError::new(SOUND_FAILED, error)),
        }
    }
}
