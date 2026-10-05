//! Acquisition flows: registered durable operations plus the four
//! ephemeral loops that keep requests moving.
//!
//! [`operations`] models free-music and drop-import as durable operations
//! in the job registry (the server does not start them yet); [`loops`] runs
//! the wanted watcher, follow new-release poll, background upgrade sweep,
//! and request-status-sync as registered ephemeral loops with shutdown
//! plumbing. [`seams`] holds the boundary to code owned elsewhere
//! (downloads dispatch, provider/indexer search), and [`stores`] the
//! in-memory state the flows read and write.

pub mod loops;
pub mod operations;
pub mod seams;
pub mod stores;
