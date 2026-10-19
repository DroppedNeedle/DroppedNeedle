//! Operator tooling: the offline CLI helpers and the v2 fixture behind the
//! pipeline tests.
//!
//! The export engine lives in [`crate::export`] and the validate/import
//! engines in [`crate::import`]; this module wires them
//! into operator surface without duplicating them:
//!
//! - [`upgrade`] turns a v2 data directory into a v3 one in place and
//!   keeps the v2 files in backup folders. It also holds the offline import
//!   the tool's `import` and `dry-run` commands run.
//! - [`recovery_code`] issues a password recovery code from the host, for
//!   an admin who cannot sign in.
//! - [`host_settings`] changes a setting from the host, for an admin whose
//!   saves the web UI refuses.
//! - [`reset_key`] gives an instance whose data key is lost for good a new
//!   key, clearing every secret the old one sealed.
//! - [`restore`] wraps the backup mechanism as an offline restore command.
//!   There is no HTTP route for restore, on purpose.
//! - [`fixture`] builds a scratch v2 instance dir carrying every entity
//!   plus conflict and deleted-ID cases for the pipeline tests.
//!
//! Everything here is offline: file and SQLite reads/writes only.

#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
pub mod host_settings;
pub mod recovery_code;
pub mod reset_key;
pub mod restore;
pub mod upgrade;
