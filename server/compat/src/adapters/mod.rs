//! What the compat handlers call: the library both protocols read,
//! durable queues, playback reports, streaming and app-password auth.

pub mod audio;
pub mod jellyfin_ids;
pub mod jellyfin_library;
pub mod library;
pub mod playback;
pub mod principal;
pub mod queues;
pub mod subsonic_store;
