//! DroppedNeedle kernel: config, the database and its writer lane, the
//! settings store, errors, ids, the HTTP client factory, events, background
//! jobs, logging and provider rate policy. Every other crate builds on it,
//! and it depends on none of them.
//!
//! `unwrap`, `expect` and `panic!` are denied in non-test code, as in the
//! server crate.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod clock;
pub mod config;
pub mod db;
pub mod error;
pub mod events;
pub mod extract;
pub mod http_client;
pub mod ids;
pub mod jobs;
pub mod mount;
pub mod observability;
pub mod platform;
pub mod runtime_config;
pub mod schema;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<db::statement::StatementFile> {
    vec![
        db::sql::registered(),
        db::cache::sql::registered(),
        jobs::queue::sql::registered(),
        runtime_config::sql::registered(),
    ]
}
