//! Discover: discover + queue + radio + batches, and home
//! shelves with the range-pair redesign.
//!
//! Provider data arrives through the [`ports`] traits. Production runs the
//! [`adapters`]: live charts, previews and YouTube search, and honest empty
//! or "not available" answers where the v2 builders are not ported yet.
//! The fakes exist for tests only.
//!
//! Wiring: mounted as `reads::discover` under `/api/v3` via
//! [`reads_router`] (with [`discover_router`] and [`home_router`]), and
//! its paths and schemas are registered in the utoipa document. The
//! now-playing route is served by `playback`.

pub mod adapters;
pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod services;

pub use error::ReadsError;
pub use handlers::{discover_router, home_router, now_playing_router, reads_router};
pub use services::ReadsDeps;
