//! Operator tooling: the offline CLI helpers, the dev-only covers-debug
//! route, and the v2 fixture behind the pipeline tests.
//!
//! The export engine lives in [`crate::export`] and the validate/import
//! engines in [`crate::import`]; this module wires them
//! into operator surface without duplicating them:
//!
//! - [`datalock`] is the file lock the server holds and the offline
//!   import takes exclusively, so the two never run on one database.
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
pub mod restore;
