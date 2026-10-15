//! Authentication: sessions, users, federated login, compat auth contracts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod compat_auth;
pub mod federated;
pub mod session;
pub mod users;

// Production adapters: password hashing, the SQLite stores, avatar files,
// the routers, and the wiring that binds them.
pub mod avatars;
pub mod passwords;
pub mod prod;
pub mod routes;
pub mod sqlite;
pub mod times;
pub mod wiring;
