//! Plugins.
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
//! - [`ext`]: the gateway to a plugin's own HTTP routes.
//! - [`panel`]: a plugin's settings panel page.
//! - [`handlers`], [`models`], [`error`]: the HTTP routes.
//! - [`wiring`]: the `AppState` bundle.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod capabilities;
pub mod error;
pub mod ext;
pub mod handlers;
pub mod host;
pub mod install;
pub mod manifest;
pub mod models;
pub mod panel;
pub mod process;
pub mod protocol;
pub mod runtime;
pub mod ticks;
pub mod wiring;
