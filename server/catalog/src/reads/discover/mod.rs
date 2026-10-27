//! Discover: discover + queue + radio + batches, and home
//! shelves with the range-pair redesign.
//!
//! The handlers call [`services`], which hold the concrete [`adapters`]:
//! the live providers, the database, and honest "not configured" answers
//! where a feature needs setup first.
//!
//! Wiring: the app mounts [`routes`], which also registers the paths in
//! the API document. The now-playing route is served by `playback`.

pub mod adapters;
pub mod demand;
pub mod error;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod services;

pub use handlers::routes;
pub use services::ReadsDeps;
