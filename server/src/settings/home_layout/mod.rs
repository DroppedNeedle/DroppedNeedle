//! Each person's home layout: the widgets they keep, where they sit, how big
//! they are, and which ones they hide. The row lives in `app_home_layouts`;
//! no row means the built-in order, which the client knows.
//!
//! The `personalization` settings section decides whose layout is served:
//! `allow` serves the caller's own row, `force` serves the first admin's.
//! Forcing never touches anyone else's row, so turning it back to `allow`
//! returns every person to the layout they saved.

pub mod handlers;
pub mod models;
pub(crate) mod sql;
pub mod store;
