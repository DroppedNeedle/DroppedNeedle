//! Plugins, plus the ListenBrainz and scrobble settings backend.
//!
//! Plugins work like community plugins in the *arr apps: an admin installs
//! one from a GitHub repository, pinned to an exact commit, and enables it.
//! Each enabled plugin runs as its own process and talks to the server in
//! JSON-RPC over stdin and stdout, so a crash or a hang stays inside that
//! process. Plugins are trusted code, not sandboxed; PLUGINS.md says what
//! the process limits do and do not cover.
//!
//! One module per concern:
//! - [`protocol`]: the wire format and method names.
//! - [`process`]: one plugin as a supervised subprocess.
//! - [`runtime`]: the transport-neutral seam and the payload types.
//! - [`manifest`]: `plugin.toml` validation.
//! - [`host`]: discovery, the running set, plugin-to-host requests.
//! - [`install`]: install and update from GitHub.
//! - [`capabilities`]: typed calls per capability, with their budgets and
//!   fallbacks.
//! - [`ticks`]: `scheduler` loops and plugin state on the jobs store.
//! - [`handlers`], [`models`], [`error`]: the HTTP routes.
//! - [`wiring`]: the `AppState` bundle.
//!
//! The scrobble half ([`scrobble`]) is the settings backend only: per-user
//! ListenBrainz links (verify-then-store, token sealed) and scrobble
//! preference reads and writes, over the baseline tables.

pub mod capabilities;
pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod handlers;
pub mod host;
pub mod install;
pub mod manifest;
pub mod models;
pub mod process;
pub mod protocol;
pub mod runtime;
pub mod scrobble;
pub mod ticks;
pub mod wiring;
