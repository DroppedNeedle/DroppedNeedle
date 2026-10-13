//! Operator tooling: the offline CLI helpers, the dev-only covers-debug
//! route, and the v2 fixture behind the pipeline tests.
//!
//! The export engine lives in [`crate::export`] and the validate/import
//! engines in [`crate::import`]; this module wires them
//! into operator surface without duplicating them:
//!
//! - [`datalock`] is the file lock the server holds and the offline
//!   import takes exclusively, so the two never run on one database.
//! - [`upgrade`] turns a v2 data directory into a v3 one in place, keeps
//!   the v2 files in backup folders, and tells the server which data
//!   directories it must not boot on. It also holds the offline import
//!   the tool's `import` and `dry-run` commands run.
//! - [`recovery_code`] issues a password recovery code from the host, for
//!   an admin who cannot sign in.
//! - [`host_settings`] changes a setting from the host, for an admin whose
//!   saves the web UI refuses.
//! - [`restore`] wraps the backup mechanism as an offline restore command.
//!   There is no HTTP route for restore, on purpose.
//! - [`covers_debug`] serves the covers-debug shape on a dev-only
//!   tooling route that release builds cannot mount.
//! - [`fixture`] builds a scratch v2 instance dir carrying every entity
//!   plus conflict and deleted-ID cases for the pipeline tests.
//!
//! Everything here is offline: file and SQLite reads/writes only.

pub mod covers_debug;
pub mod datalock;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
pub mod host_settings;
pub mod recovery_code;
pub mod restore;
#[cfg(any(test, feature = "test-support"))]
pub mod scratch;
pub mod upgrade;
