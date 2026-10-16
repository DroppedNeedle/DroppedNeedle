//! Production seam bindings: the library both protocols read, durable
//! queues, playback reports, streaming and app-password auth.

pub mod engines;
pub mod jellyfin_library;
pub mod library;
pub mod playback;
pub mod principal;
pub mod queues;
pub mod subsonic_store;
