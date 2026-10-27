//! Authentication: sessions, users, federated login, compat auth contracts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod compat_auth;
pub mod federated;
pub mod session;
pub mod users;

pub use users::roles::{Admin, Curator, Principal, Role, RoleRequired, User};

// Password hashing, the repositories, avatar files, the routers, and the
// wiring that binds them.
pub mod avatars;
pub mod passwords;
pub mod prod;
pub mod routes;
pub mod sqlite;
pub mod times;
pub mod wiring;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    sqlite::statement_files()
}
