//! Background jobs: the loop supervisor plus the kernel's own loops.
//!
//! Every loop runs on the [`Supervisor`] under its own name: it answers
//! stop promptly, paces itself with interval plus jitter plus backoff, and
//! recovers from failed ticks without hot-looping. One-shots (the events
//! kick, precache runs) are live only while they run. Plugin ticks run per
//! plugin and keep their state on the store, never in files.
//!
//! Ownership is narrow on purpose. `acquire` owns acquisition, downloads,
//! indexers, wanted, Lidarr, Spotify, the download watchdog, auto-retry,
//! request-status sync, and the new-release poll; `library` owns scan,
//! identify, publish, the library watcher, the revision poller, and worker
//! supervision; `remotes` owns the Jellyfin/Navidrome/Plex MBID warmups;
//! `reads::discover` owns the discover/home cache loops. This module must
//! not grow twins of those.
//!
//! Always-on loops spawned at boot:
//!
//! - [`checkpoint::JOB_NAME`]: WAL checkpoint passes.
//! - [`presence::JOB_NAME`]: now-playing presence polling.
//! - [`personal_mix::JOB_NAME`]: daily personal-mix rebuilds.
//! - [`playlist_sync::JOB_NAME`]: Navidrome playlist file sync.
//! - [`events_watcher::JOB_NAME`]: daily live-events sweep.
//! - [`discovery_demand::JOB_NAME`]: the discover warm cycle.
//!
//! Started on demand:
//!
//! - [`events_kick::JOB_NAME`]: the kicked sweep after a settings save.
//! - [`precache::JOB_NAME`]: supervised precache runs.
//! - [`precache::AFTER_SCAN_JOB`]: the post-scan wait before a precache run.
//! - `plugin-tick:{name}`: one loop per scheduler plugin.
//!
//! Wiring: `bootstrap::jobs::JobsSetup` is the one `AppState` field these
//! loops add. It holds the supervisor, nests the playlist-sync route for the
//! settings tree, and starts the boot loops; the plugins bundle runs its tick
//! loops on the same supervisor.

pub mod cache_sync;
pub mod checkpoint;
pub mod discovery_demand;
pub mod events_kick;
pub mod events_watcher;
pub mod personal_mix;
pub mod playlist_export;
pub mod playlist_sync;
pub mod plugin_ticks;
pub mod precache;
pub mod precache_phases;
pub mod presence;
pub mod queue;
pub mod schedule;
pub mod supervisor;

pub use futures_util::future::BoxFuture;
pub use schedule::{Schedule, SplitMix64};
pub use supervisor::{
    AlreadyRunning, FnTick, Next, Supervisor, Tick, TickCtx, TickError, TickResult,
};
pub use tokio_util::sync::CancellationToken;

/// Always-on loops the boot path spawns, in spawn order.
pub const BOOT_JOBS: &[&str] = &[
    checkpoint::JOB_NAME,
    presence::JOB_NAME,
    personal_mix::JOB_NAME,
    playlist_sync::JOB_NAME,
    events_watcher::JOB_NAME,
    discovery_demand::JOB_NAME,
];
