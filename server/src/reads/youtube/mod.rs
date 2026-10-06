//! YouTube links: saved full-album and per-track YouTube videos that the
//! album page and the YouTube library page play.
//!
//! Videos are found through the server's one YouTube client (the same one
//! the discover queue uses), so every search spends from the one daily
//! budget in `youtube_quota.json`. People can also paste a video by hand,
//! for an album the server knows or one it does not. Links live in
//! `youtube_links` and `youtube_track_links` and are shared by everyone on
//! the server.

pub mod handlers;
pub mod models;
pub mod service;
pub mod store;

pub use handlers::router;
pub use service::YouTubeLinks;
