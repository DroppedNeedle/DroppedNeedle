//! Acquisition flows: registered durable operations plus the four
//! ephemeral loops that keep requests moving.
//!
//! [`operations`] runs free-music and drop-import as durable operations in
//! the job registry, closing v2's unregistered gap; [`loops`] runs the
//! wanted watcher, follow new-release poll, background upgrade sweep, and
//! request-status-sync as registered ephemeral loops with shutdown
//! plumbing. [`seams`] holds the boundary this slice does not own
//! (downloads dispatch, provider/indexer search), and [`stores`] the memory
//! state the flows read and write.

pub mod loops;
pub mod operations;
pub mod seams;
pub mod stores;
