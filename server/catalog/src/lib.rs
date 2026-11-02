//! The catalog: library, search, artist and album pages, discover,
//! collections, artwork, YouTube links, lyrics, upcoming concerts, the
//! version check and wrapped. [`wiring`] builds them into one bundle.

// The local catalog's one `sql!` block expands one level per statement.
#![recursion_limit = "256"]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod artwork;
pub mod cache_hold;
pub mod collections;
pub mod concerts;
pub mod discover;
pub mod enrichment;
pub mod local;
pub mod lyrics;
pub mod metadata;
pub mod search;
pub mod version;
pub mod wiring;
pub mod wrapped;
pub mod youtube;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![
        artwork::sql::registered(),
        collections::sql::registered(),
        concerts::sql::registered(),
        discover::sql::registered(),
        local::sql::registered(),
        search::sql::registered(),
        youtube::sql::registered(),
    ]
}
