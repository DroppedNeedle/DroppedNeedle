//! Plugins host plus the ListenBrainz and scrobble settings backend.
//!
//! The host manages plugin packages the way v2 does: a folder on disk is
//! inert until its `plugin.toml` validates and an admin enables it. What it
//! does not do yet is run plugin code. v2 imports Python in-process; v3 has
//! no execution engine for that, so [`ModuleLoader`](runtime::ModuleLoader)
//! is the seam where one plugs in: tests and future engines provide the
//! modules, the host owns discovery, validation, settings, dispatch, and
//! scheduling around them.
//!
//! Tick durability lives in jobs: per-plugin tick state is on the jobs
//! tick store, loops run under the jobs registry as `plugin-tick:<name>`,
//! and this module adapts the host to those seams
//! ([`ticks::HostTickAdapter`], [`ticks::PluginTickLoops`]). Loop mechanics
//! and store durability are tested with jobs; the journey here proves the
//! host side: install, enable, tick, and the
//! persisted state reading back from the jobs store.
//!
//! The scrobble half is the settings backend only: per-user ListenBrainz
//! links (verify-then-store, token sealed) and scrobble preference reads
//! and writes. The SQLite stores target the existing baseline tables
//! (`user_connections` with `service = 'listenbrainz'`, and
//! `user_listening_prefs`), with memory stores behind the same traits for
//! tests.
//!
//! The `plugins_*` integration tests drive these modules through the
//! `droppedneedle` crate name, alongside the wired routers.

pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod handlers;
pub mod host;
pub mod manifest;
pub mod models;
pub mod runtime;
pub mod scrobble;
pub mod ticks;
pub mod wiring;
