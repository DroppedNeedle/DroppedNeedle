//! Live progress of the library image precache.
//!
//! One [`CacheSyncStatus`] per server holds where the current run stands and
//! publishes every change as `cache.sync` on the event hub, the way v2's
//! `CacheStatusService` fed its SSE stream. Item updates are throttled to
//! one frame per [`BROADCAST_EVERY`]; phase changes, the last item of a
//! phase, and the end of a run always go out.
//!
//! Each run gets a generation number. Cancelling bumps it, so a phase that
//! is still unwinding after the cancel cannot overwrite the reset status.
//! The status lives in memory only: a rerun skips everything already
//! cached, so there is nothing a durable resume record would save.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::events::{CacheSyncProgress, Event, EventSink};

/// Least time between two item-progress frames (v2 broadcast throttle).
pub const BROADCAST_EVERY: Duration = Duration::from_millis(300);

/// Shared precache status. Cheap to clone; clones share everything.
#[derive(Clone, Default)]
pub struct CacheSyncStatus {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    sink: EventSink,
}

#[derive(Default)]
struct State {
    progress: CacheSyncProgress,
    generation: u64,
    last_frame: Option<Instant>,
}

impl std::fmt::Debug for CacheSyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheSyncStatus")
            .field("syncing", &self.snapshot().is_syncing)
            .finish()
    }
}

impl CacheSyncStatus {
    /// The sink frames go out on; [`crate::AppState::with_events`] attaches
    /// it to the hub.
    pub fn sink(&self) -> &EventSink {
        &self.inner.sink
    }

    /// The current status.
    pub fn snapshot(&self) -> CacheSyncProgress {
        self.lock().progress.clone()
    }

    /// Begin a run in its first phase. Returns the run's generation, which
    /// every later update for this run carries.
    pub fn start(&self, phase: &str, total_artists: u64, total_albums: u64) -> u64 {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64());
        let frame = {
            let mut state = self.lock();
            state.generation += 1;
            state.progress = CacheSyncProgress {
                is_syncing: true,
                phase: Some(phase.to_owned()),
                started_at: Some(started_at),
                total_artists,
                total_albums,
                ..CacheSyncProgress::default()
            };
            state.last_frame = Some(Instant::now());
            (state.generation, state.progress.clone())
        };
        self.publish(frame.1);
        frame.0
    }

    /// Move to a phase with `total` items. A zero total marks the phase as
    /// skipped, which the progress pill shows by jumping past it.
    pub fn phase(&self, generation: u64, phase: &str, total: u64) {
        self.update(generation, true, |progress| {
            progress.phase = Some(phase.to_owned());
            progress.total_items = total;
            progress.processed_items = 0;
            progress.current_item = None;
        });
    }

    /// Record items done in the current phase. `artists`/`albums` move the
    /// run-wide counters when given.
    pub fn progress(
        &self,
        generation: u64,
        processed: u64,
        current: &str,
        artists: Option<u64>,
        albums: Option<u64>,
    ) {
        self.update(generation, false, |progress| {
            if processed >= progress.processed_items {
                progress.processed_items = processed;
                progress.current_item = Some(current.to_owned());
            }
            if let Some(artists) = artists {
                progress.processed_artists = progress.processed_artists.max(artists);
            }
            if let Some(albums) = albums {
                progress.processed_albums = progress.processed_albums.max(albums);
            }
        });
    }

    /// End the run: idle again, carrying the error when it failed.
    pub fn finish(&self, generation: u64, error: Option<String>) {
        let frame = {
            let mut state = self.lock();
            if state.generation != generation || !state.progress.is_syncing {
                return;
            }
            state.progress = CacheSyncProgress {
                error_message: error,
                ..CacheSyncProgress::default()
            };
            state.progress.clone()
        };
        self.publish(frame);
    }

    /// Stop following the current run: back to idle with no error, and a
    /// new generation so the run's last updates are ignored.
    pub fn cancel(&self) {
        let frame = {
            let mut state = self.lock();
            state.generation += 1;
            if !state.progress.is_syncing {
                return;
            }
            tracing::warn!(
                phase = ?state.progress.phase,
                processed = state.progress.processed_items,
                total = state.progress.total_items,
                "library precache cancelled"
            );
            state.progress = CacheSyncProgress::default();
            state.progress.clone()
        };
        self.publish(frame);
    }

    fn update(&self, generation: u64, always: bool, change: impl FnOnce(&mut CacheSyncProgress)) {
        let frame = {
            let mut state = self.lock();
            if state.generation != generation || !state.progress.is_syncing {
                return;
            }
            change(&mut state.progress);
            let progress = &mut state.progress;
            progress.progress_percent = percent(progress.processed_items, progress.total_items);
            let last_item = progress.processed_items >= progress.total_items;
            let due = state
                .last_frame
                .is_none_or(|last| last.elapsed() >= BROADCAST_EVERY);
            if !(always || last_item || due) {
                return;
            }
            state.last_frame = Some(Instant::now());
            state.progress.clone()
        };
        self.publish(frame);
    }

    fn publish(&self, progress: CacheSyncProgress) {
        self.inner.sink.publish(Event::CacheSync(progress));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Whole percent done, capped at 100; zero for an empty phase.
fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    u8::try_from((done.saturating_mul(100) / total).min(100)).unwrap_or(100)
}
