//! Acquisition flows: the ephemeral loops that keep requests moving.
//!
//! [`loops`] runs the wanted watcher, follow new-release poll, background
//! upgrade sweep, and request-status-sync as registered ephemeral loops
//! with shutdown plumbing. [`seams`] holds the boundary to code owned
//! elsewhere (downloads dispatch, provider/indexer search), and [`stores`]
//! the state the flows read and write. Drop import lives in its own
//! module ([`super::drop_import`]).

pub mod loops;
pub mod seams;
pub mod stores;
