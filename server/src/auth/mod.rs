//! Authentication: sessions, users, federated login, compat auth contracts.

pub mod compat_auth;
pub mod federated;
pub mod session;
pub mod users;

// Stage-3 close-out modules (production adapters). These `pub mod` lines are
// the minimal mechanical requirement for the new files; the slice wiring
// above is untouched. HTTP (routers, handlers, IdP calls) belongs to the
// sibling routers slice, not here.
pub mod passwords;
pub mod prod;
pub mod routes;
pub mod sqlite;
pub mod times;
pub mod wiring;
