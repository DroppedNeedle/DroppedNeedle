//! DroppedNeedle v3 backend.
//!
//! Layers: handlers (thin Axum handlers) call services holding domain logic;
//! services reach the outside world through trait ports implemented by
//! adapters. `AppState` carries every long-lived dependency, built once at
//! boot and injected by constructor. No global singletons.
//!
//! Panic-based handling of runtime failure is banned: `unwrap`, `expect` and
//! `panic!` are denied in non-test code, so fallible paths return typed
//! errors. Unit tests keep the idiomatic asserts via the `cfg(test)` allow
//! below; integration tests are separate crates and unaffected.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod admin;
pub mod app;
pub mod bootstrap;
pub mod docs;
pub mod export;
pub mod handlers;
pub mod import;
pub mod middleware;
pub mod security_headers;
pub mod settings;
pub mod state;
pub mod tooling;
pub mod web;

pub use app::{create_app, create_app_with_web};
pub use state::AppState;
