//! Authentication: sessions, users, federated login, compat auth contracts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// An account other users may see by its display name, over an
/// `auth_users` alias: the person chose that name. The user directory and
/// whom a playlist can be shared with both read it.
#[macro_export]
macro_rules! listed_user {
    ($user:literal) => {
        concat!($user, ".display_name_chosen = 1")
    };
}

pub mod app_connect;
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
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod times;
pub mod wiring;
