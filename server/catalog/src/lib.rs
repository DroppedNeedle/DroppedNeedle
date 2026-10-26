//! The read side: library, search, artist and album pages, discover,
//! collections and the platform reads, plus upcoming concerts.

// The local catalog's one `sql!` block expands one level per statement.
#![recursion_limit = "256"]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod concerts;
pub mod reads;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![
        concerts::sql::registered(),
        reads::library::sql::registered(),
    ]
}
