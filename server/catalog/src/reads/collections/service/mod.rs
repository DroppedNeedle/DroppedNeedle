//! Collections business rules: playlist ownership and visibility, favorite
//! kinds, follow and auto-download state.
//!
//! The service returns [`CollectionsError`](super::error::CollectionsError)
//! and knows nothing of HTTP. Native handlers, both compat protocols and the
//! remote playlist import all go through it, so one set of rules guards one
//! set of rows.

mod favorites;
mod follows;
mod play_marks;
mod playlists;
mod ratings;

pub use follows::follow_status;
pub use playlists::{LOCAL_SOURCE, SavedPlaylist, Visible};

use super::state::CollectionsState;

/// The collections service over one state.
#[derive(Clone, Copy)]
pub struct CollectionsService<'a> {
    state: &'a CollectionsState,
}

impl<'a> CollectionsService<'a> {
    /// Service over the shared state.
    pub fn new(state: &'a CollectionsState) -> Self {
        Self { state }
    }
}
