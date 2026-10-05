//! Stage-11 tooling slice: the offline CLI helpers, the dev-only
//! covers-debug route, and the v2 fixture behind the pipeline tests.
//!
//! The export engine lives in [`crate::export`] and the validate/import
//! engines in [`crate::import`] (sibling slices); this module wires them
//! into operator surface without duplicating them:
//!
//! - [`restore`] wraps the stage-10 backup mechanism as an offline
//!   restore command. There is deliberately no HTTP route for restore.
//! - [`covers_debug`] serves the R11 covers-debug shape on a dev-only
//!   tooling route that release builds cannot mount.
//! - [`fixture`] builds a scratch v2 instance dir carrying every entity
//!   plus conflict and deleted-ID cases for the pipeline tests.
//!
//! Everything here is offline: file and SQLite reads/writes only.

pub mod covers_debug;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
pub mod restore;
