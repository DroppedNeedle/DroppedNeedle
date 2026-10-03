//! v2 → v3 export validation and import (stage 11, validate+import slice).
//!
//! Rule sources: `build-stage-plan.md` stage 11 plus
//! `.dev-notes/Plans/RustPort/stage0-export.md`. The pipeline is
//! [`pipeline::run_import`]; the standalone checker is
//! [`validate::validate_export`]; every run yields one
//! [`report::ImportReport`].
//!
//! Crypto and envelope shapes are owned by the export slice
//! (`crate::export::seal`, `crate::export::envelope`) and reused here, so
//! the two sides cannot drift: one KDF, one sealed-blob layout. Import
//! decisions on top: `lastfm_settings` keeps only `enabled` (R7
//! decrypt-then-drop); v3-only config keys outside the export section
//! set are preserved, never defaulted. CLI verbs
//! (validate/import/dry-run/restore) are thin wrappers around this API
//! and belong to the integrator.
//!
//! Deleted IDs fail closed: [`validate_export`] refuses the whole file on
//! a dangling user reference, and the operator repair is dropping the
//! orphan rows from v2 and re-exporting. The planner's drop-and-count
//! branches are defense in depth unreachable through [`pipeline::run_import`].

pub mod envelope;
pub mod pipeline;
pub mod r8;
pub mod report;
pub mod validate;

pub use envelope::{ExportFile, unseal_value};
pub use pipeline::{ImportRequest, run_import};
pub use report::{ExitCode, ImportReport};
pub use validate::{ValidationIssue, ValidationReport, validate_export};
