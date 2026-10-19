//! Discover: discover + queue + radio + batches, and home
//! shelves with the range-pair redesign.
//!
//! Provider data arrives through the [`ports`] traits. Production runs the
//! [`adapters`]: live charts, previews and YouTube search, and honest empty
//! or "not available" answers where the v2 builders are not ported yet.
//! The fakes exist for tests only.
//!
//! Wiring: the app mounts [`routes`], which also registers the paths in
//! the API document. The now-playing route is served by `playback`.

pub mod adapters;
pub mod demand;
pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod services;

pub use handlers::{now_playing_router, routes};
pub use services::ReadsDeps;
