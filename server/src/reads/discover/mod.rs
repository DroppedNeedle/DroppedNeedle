//! Stage-4 discover slice: discover + queue + radio + batches, and home
//! shelves with the range-pair redesign.
//!
//! Provider data arrives through the [`ports`] traits; stage 4 runs the
//! [`fakes`], stage 5 wires real providers behind the same handlers.
//!
//! Wiring: mounted as `reads::discover` under `/api/v3` via
//! [`reads_router`] (with [`discover_router`] and [`home_router`]), and
//! its paths and schemas are registered in the utoipa document. The
//! stage-4 now-playing snapshot moved to the `playback` slice in stage 6.

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
