//! What tests need from the server crate and production never uses: the
//! statement registry. The test hooks the app serves are in `app.rs`,
//! `handlers.rs` and `bootstrap/jobs.rs`, behind `config.test_hooks`.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

use droppedneedle_kernel::db::statement::StatementFile;

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::admin::sql::registered(),
        crate::appearance::sql::registered(),
        crate::bootstrap::sql::registered(),
        crate::settings::sql::registered(),
        crate::upgrade_mode::sql::registered(),
    ]
}
