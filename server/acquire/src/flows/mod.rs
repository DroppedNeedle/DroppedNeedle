//! Acquisition flows: the loops that keep requests moving.
//!
//! [`loops`] runs the wanted watcher, follow new-release poll, background
//! upgrade sweep, request-status sweep and store prune on the loop
//! supervisor. [`seams`] holds the boundary to code owned
//! elsewhere (downloads dispatch, provider/indexer search), and [`stores`]
//! the state the flows read and write. Drop import lives in its own
//! module ([`super::drop_import`]).

pub mod bundle;
pub mod loops;
pub mod seams;
pub(crate) mod sql;
pub mod stores;
