//! Authentication: sessions, users, federated login, compat auth contracts.

pub mod compat_auth;
pub mod federated;
pub mod session;
pub mod users;

// Production adapters: password hashing, the SQLite stores, the routers,
// and the wiring that binds them.
pub mod passwords;
pub mod prod;
pub mod routes;
pub mod sqlite;
pub mod times;
pub mod wiring;
