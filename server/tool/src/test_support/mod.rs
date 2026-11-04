//! What tests need from the tool and production never uses: the
//! statement registry, the builders its carry families are checked over,
//! and the v2 fixture tests upgrade.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

pub(crate) mod carry;
pub mod fixture;

use droppedneedle_kernel::db::statement::StatementFile;

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::export::sql::registered(),
        crate::import::sql::registered(),
        crate::import::carry::sql::registered(),
        crate::tooling::sql::registered(),
    ]
}
