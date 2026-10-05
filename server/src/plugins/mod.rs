//! Stage-10 plugins host plus the ListenBrainz and scrobble settings backend.
//!
//! The host manages plugin packages the way v2 does: a folder on disk is
//! inert until its `plugin.toml` validates AND an admin enables it. What it
//! does not do yet is run plugin code. v2 imports Python in-process; v3 has
//! no execution engine for that, so [`ModuleLoader`](runtime::ModuleLoader)
//! is the seam where one plugs in: tests and future engines provide the
//! modules, the host owns discovery, validation, settings, dispatch, and
//! scheduling around them.
//!
//! Tick durability follows the D11 redesign on the sibling jobs slice:
//! per-plugin tick state lives on the jobs tick store, loops run under the
//! jobs registry as `plugin-tick:<name>`, and this slice adapts the host
//! to those seams ([`ticks::HostTickAdapter`], [`ticks::PluginTickLoops`]).
//! Loop mechanics and store durability are the jobs slice's briefs; the
//! journey here proves the host side: install, enable, tick, and the
//! persisted state reading back from the jobs store.
//!
//! The scrobble half is the R9 settings backend only: per-user ListenBrainz
//! links (verify-then-store, token sealed) and scrobble preference reads and
//! writes. No UI lives here; that arrives in stage 12. The SQLite stores
//! target the existing baseline tables (`user_connections` with
//! `service = 'listenbrainz'`, and `user_listening_prefs`); this slice ships
//! the traits plus memory stores behind the same shapes.
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
