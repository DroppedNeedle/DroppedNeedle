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

// The stage-4 slices address shared items through the crate name so their
// standalone `#[path]` briefs keep compiling; this alias lets the same
// paths resolve once the slices are wired into the tree.
extern crate self as droppedneedle;

pub mod acquire;
pub mod admin;
pub mod app;
pub mod auth;
pub mod compat;
pub mod config;
pub mod db;
pub mod docs;
pub mod error;
pub mod export;
pub mod handlers;
pub mod http_client;
pub mod ids;
pub mod import;
pub mod jobs;
pub mod library;
pub mod media;
pub mod middleware;
pub mod observability;
pub mod playback;
pub mod plugins;
pub mod provider_policy;
pub mod providers;
pub mod reads;
pub mod remotes;
#[path = "config/facade.rs"]
pub mod runtime_config;
pub mod schema;
pub mod settings;
pub mod stage6;
pub mod state;
pub mod stream;
pub mod tooling;

pub use app::create_app;
pub use config::AppConfig;
pub use state::AppState;
