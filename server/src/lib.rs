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

pub mod acquire;
pub mod admin;
pub mod app;
pub mod auth;
pub mod bootstrap;
pub mod client_ip;
pub mod compat;
pub mod concerts;
pub mod config;
pub mod db;
pub mod docs;
pub mod error;
pub mod events;
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
pub mod runtime_config;
pub mod schema;
pub mod settings;
pub mod state;
pub mod stream;
pub mod tooling;
pub mod web;

pub use app::{create_app, create_app_with_web};
pub use config::AppConfig;
pub use state::AppState;
