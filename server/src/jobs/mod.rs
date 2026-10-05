//! Background jobs: the registry mechanism plus the loops it owns.
//!
//! Every loop in this module registers under its own job name, answers stop
//! promptly, paces itself with interval plus jitter plus backoff, and
//! recovers from cycle failures without hot-looping. One-shots
//! (the events kick, precache runs) register only while live. Plugin ticks
//! register per plugin and persist their state on the store, never in files.
//!
//! Ownership is narrow on purpose. `acquire` owns acquisition, downloads,
//! indexers, wanted, Lidarr, Spotify, the download watchdog, auto-retry,
//! request-status sync, and the new-release poll; `library` owns scan,
//! identify, publish, the library watcher, the revision poller, and worker
//! supervision; `playback` owns the Jellyfin/Navidrome/Plex MBID warmups;
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
//!
//! On-demand entries, registered only while running:
//!
//! - [`events_kick::JOB_NAME`]: the kicked sweep after a settings save.
//! - [`precache::JOB_NAME`]: supervised precache runs.
//! - `plugin-tick:{name}`: one loop per scheduler plugin.
//!
//! Wiring: [`wiring::JobsSetup`] is the one `AppState` field this module
//! adds. It owns the shared registry, nests the playlist-sync route for the
//! settings tree ([`wiring::JobsSetup::settings_router`]), and spawns the
//! boot loops ([`wiring::JobsSetup::spawn_loops`]); the plugins bundle
//! shares the registry for its tick loops rather than running its own.

pub mod checkpoint;
pub mod events_kick;
pub mod events_watcher;
pub mod media;
pub mod personal_mix;
pub mod playlist_sync;
pub mod plugin_ticks;
pub mod precache;
pub mod presence;
pub mod registry;
pub mod schedule;
pub mod wiring;

pub use registry::{
    AlreadyRunning, JobCtx, JobExit, JobKind, JobRegistry, JobState, RegistryStore,
};
pub use schedule::{Schedule, SplitMix64};

/// Always-on loops the boot path spawns, in spawn order.
pub const BOOT_JOBS: &[&str] = &[
    checkpoint::JOB_NAME,
    presence::JOB_NAME,
    personal_mix::JOB_NAME,
    playlist_sync::JOB_NAME,
    events_watcher::JOB_NAME,
];
