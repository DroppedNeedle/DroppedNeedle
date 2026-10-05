//! Discover: discover + queue + radio + batches, and home
//! shelves with the range-pair redesign.
//!
//! Provider data arrives through the [`ports`] traits; production still
//! runs the [`fakes`], and real providers belong behind the same handlers.
//!
//! Wiring: mounted as `reads::discover` under `/api/v3` via
//! [`reads_router`] (with [`discover_router`] and [`home_router`]), and
//! its paths and schemas are registered in the utoipa document. The
//! now-playing route is served by `playback`.

pub mod error;
pub mod fakes;
pub mod handlers;
pub mod models;
pub mod ports;
pub mod refresh;
pub mod services;

pub use error::ReadsError;
pub use handlers::{discover_router, home_router, now_playing_router, reads_router};
pub use services::ReadsDeps;
