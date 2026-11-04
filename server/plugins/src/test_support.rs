//! What tests need from this crate and production never uses: the statement registry.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

use droppedneedle_kernel::db::statement::StatementFile;

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![crate::capabilities::sql::registered()]
}
