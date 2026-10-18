//! The offline migration tool: v2 export, export validation and import,
//! and the operator helpers behind the `droppedneedle-tool` binary.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod export;
pub mod import;
pub mod tooling;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![export::sql::registered()]
}
